//! How an index reads a subtree's largest files, for `top --files` and
//! `stale`: a range of the primary key, sorted, when the subtree is small;
//! a walk of the size index (`files_allocated`, `INDEXED BY`), kept to the
//! subtree's id range, when it is large. The database's own planner takes
//! the range even where the walk is thousands of times faster, so the choice is
//! made here, from the subtree's file count against 3·√N for a scan of N
//! files: below it the range holds few rows to sort; above it the walk
//! finds the subtree's largest files within its first rows.
//!
//! Either plan returns the rows the arena's queries return: ranked by size,
//! ties by path, and the count before the limit, taken from `n_files` or
//! counted the way the plan reads.

use std::collections::VecDeque;

use rusqlite::OptionalExtension;
use rusqlite::types::Value;

use super::{SizeKind, SortKey, TreeView, sort_ids_by};
use crate::CoreError;
use crate::index::Index;

/// How a ranked read of a subtree's files is made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plan {
    /// The subtree's id range, sorted by size.
    Range,
    /// The size index, walked from the largest, filtered to the id range.
    Walk,
}

impl Plan {
    /// The plan for a subtree of `files` files in a scan of `total`: the
    /// range below 3·√total, the walk from there.
    pub fn choose(files: u64, total: u64) -> Plan {
        // 3·√total, compared squared: files² < 9·total.
        let files = u128::from(files);
        if files * files < 9 * u128::from(total) {
            Plan::Range
        } else {
            Plan::Walk
        }
    }
}

/// What a ranked read keeps besides the subtree: `stale`'s cut.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cut {
    /// Modification and change times both before this, Unix nanoseconds.
    pub before: i64,
    /// Allocated bytes at least this.
    pub min_allocated: u64,
}

/// A subtree's largest files: the ids, ranked by allocated size, ties by
/// path, and how many files matched before the limit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ranked {
    /// The ids, at most the limit.
    pub ids: Vec<u32>,
    /// The files that matched.
    pub total_count: u64,
}

impl Index {
    /// The largest non-directories under `dir` (allocated bytes, ties by
    /// path), at most `limit`, that pass `cut` when given, read with
    /// `plan`, or the plan [`Plan::choose`] picks when `None`.
    pub fn largest_files(
        &self,
        dir: u32,
        limit: usize,
        cut: Option<Cut>,
        plan: Option<Plan>,
    ) -> Result<Ranked, CoreError> {
        let root = self.node(dir);
        let total_files = self.node(0).file_count;
        let plan = plan.unwrap_or_else(|| Plan::choose(root.file_count, total_files));
        let corrupt = |e: rusqlite::Error| CoreError::IndexCorrupt {
            file: self.file().display().to_string(),
            message: e.to_string(),
        };
        let (first, last) = (dir, root.last_id);
        let indexed = match plan {
            Plan::Range => "",
            Plan::Walk => " INDEXED BY files_allocated",
        };
        let mut cond = String::from("id BETWEEN ?1 AND ?2");
        let mut params: Vec<Value> = vec![first.into(), last.into()];
        if let Some(cut) = cut {
            cond.push_str(" AND allocated >= ?3 AND mtime < ?4 AND ctime < ?4");
            params.push(signed(cut.min_allocated).into());
            params.push(cut.before.into());
        }
        // The count: every file of the subtree, or those past the cut,
        // counted as the plan reads: over the subtree's id range when it is
        // small, along the size index from the smallest size kept when it is
        // large (few files reach stale's default 10 MiB).
        let total_count = if cut.is_some() {
            let sql = format!("SELECT COUNT(*) FROM files{indexed} WHERE {cond}");
            let n: i64 = self
                .db()
                .query_row(&sql, rusqlite::params_from_iter(&params), |row| row.get(0))
                .map_err(corrupt)?;
            u64::try_from(n).unwrap_or(0)
        } else {
            root.file_count
        };
        if limit == 0 || total_count == 0 {
            return Ok(Ranked {
                ids: Vec::new(),
                total_count,
            });
        }
        // SQLite's LIMIT is a signed 64-bit integer.
        let cap = i64::try_from(limit).unwrap_or(i64::MAX);
        let found: Vec<(u32, i64)> = self
            .db()
            .prepare(&format!(
                "SELECT id, allocated FROM files{indexed} WHERE {cond} \
                 ORDER BY allocated DESC, id LIMIT {cap}"
            ))
            .and_then(|mut stmt| {
                stmt.query_map(rusqlite::params_from_iter(&params), |row| {
                    Ok((row.get(0)?, row.get(1)?))
                })?
                .collect()
            })
            .map_err(corrupt)?;
        // A subtree that is one file is that file.
        if !root.is_dir() {
            return Ok(Ranked {
                ids: found.into_iter().map(|(id, _)| id).collect(),
                total_count,
            });
        }
        // The rows larger than the last one kept are all here, and few: they
        // are ranked by size, then path. The rows tied with the last one may
        // be most of the tree (a tree of equal files), so the first of them
        // in path order are found by walking the tree in that order.
        let mut ids: Vec<u32> = match found.last() {
            Some(&(_, boundary)) if found.len() == limit => {
                let mut above: Vec<u32> = found
                    .iter()
                    .filter(|&&(_, size)| size != boundary)
                    .map(|&(id, _)| id)
                    .collect();
                sort_ids_by(self, &mut above, SortKey::Size, false, |n| {
                    n.size(SizeKind::Allocated)
                });
                let ties = self
                    .first_in_path_order(dir, boundary, cut, limit - above.len())
                    .map_err(corrupt)?;
                above.extend(ties);
                above
            }
            _ => {
                let mut all: Vec<u32> = found.into_iter().map(|(id, _)| id).collect();
                sort_ids_by(self, &mut all, SortKey::Size, false, |n| {
                    n.size(SizeKind::Allocated)
                });
                all
            }
        };
        ids.truncate(limit);
        Ok(Ranked { ids, total_count })
    }
}

/// A directory being walked in path order: its own files still to read
/// (from `next` to `files_end`, a page at a time), the page read, and its
/// subdirectories not yet entered, keyed by their names and a `/`.
struct Frame {
    next: u32,
    files_end: u32,
    page: VecDeque<(u32, Vec<u8>)>,
    subdirs: VecDeque<(Vec<u8>, u32)>,
}

/// Files a page of a directory's own files holds.
const PAGE: usize = 256;

impl Index {
    /// The first `n` files under `dir` in path order (bytewise, as the
    /// arena ranks ties) whose allocated size is `size` and that pass `cut`.
    /// A path orders a directory's children by name, a subdirectory's name
    /// followed by `/`; a directory's own files are stored in name order, so
    /// they are read in pages, and a subtree holding no such file, found by
    /// a seek on the size index, is passed over.
    fn first_in_path_order(
        &self,
        dir: u32,
        size: i64,
        cut: Option<Cut>,
        n: usize,
    ) -> rusqlite::Result<Vec<u32>> {
        let mut found = Vec::new();
        if n == 0 {
            return Ok(found);
        }
        let cond = match cut {
            Some(_) => "allocated = ?1 AND allocated >= ?4 AND mtime < ?5 AND ctime < ?5",
            None => "allocated = ?1 AND ?4 = ?4 AND ?5 = ?5",
        };
        let (min, before) = cut.map_or((0, 0), |c| (signed(c.min_allocated), c.before));
        let mut holds = self.db().prepare(&format!(
            "SELECT 1 FROM files INDEXED BY files_allocated \
             WHERE id BETWEEN ?2 AND ?3 AND {cond} LIMIT 1"
        ))?;
        let mut page = self.db().prepare(&format!(
            "SELECT id, name FROM files WHERE id BETWEEN ?2 AND ?3 AND {cond} \
             ORDER BY id LIMIT {PAGE}"
        ))?;
        let mut subdirs = self
            .db()
            .prepare("SELECT name, id FROM dirs WHERE parent_id = ?1")?;
        let mut frame = |id: u32| -> rusqlite::Result<Option<Frame>> {
            let node = self.node(id);
            let held = holds
                .query_row(
                    rusqlite::params![size, id, node.last_id, min, before],
                    |_| Ok(()),
                )
                .optional()?;
            if held.is_none() {
                return Ok(None);
            }
            let mut dirs: Vec<(Vec<u8>, u32)> = subdirs
                .query_map([id], |row| {
                    let mut key: Vec<u8> = row.get(0)?;
                    key.push(b'/');
                    Ok((key, row.get(1)?))
                })?
                .collect::<rusqlite::Result<_>>()?;
            dirs.sort();
            Ok(Some(Frame {
                next: id + 1,
                files_end: node.files_end,
                page: VecDeque::new(),
                subdirs: dirs.into(),
            }))
        };
        let mut stack: Vec<Frame> = frame(dir)?.into_iter().collect();
        while let Some(top) = stack.last_mut() {
            if top.page.is_empty() && top.next <= top.files_end {
                let rows: Vec<(u32, Vec<u8>)> = page
                    .query_map(
                        rusqlite::params![size, top.next, top.files_end, min, before],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )?
                    .collect::<rusqlite::Result<_>>()?;
                top.next = match rows.last() {
                    Some(&(last, _)) if rows.len() == PAGE => last + 1,
                    _ => top.files_end + 1,
                };
                top.page.extend(rows);
            }
            let file_first = match (top.page.front(), top.subdirs.front()) {
                (Some((_, file)), Some((sub, _))) => file < sub,
                (Some(_), None) => true,
                (None, Some(_)) => false,
                (None, None) => {
                    stack.pop();
                    continue;
                }
            };
            if file_first {
                if let Some((id, _)) = top.page.pop_front() {
                    found.push(id);
                    if found.len() == n {
                        break;
                    }
                }
            } else if let Some((_, sub)) = top.subdirs.pop_front()
                && let Some(next) = frame(sub)?
            {
                stack.push(next);
            }
        }
        Ok(found)
    }
}

/// A size bound as the index compares it: its signed integer, a bound
/// past the largest the index can hold held to that largest.
fn signed(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests;
