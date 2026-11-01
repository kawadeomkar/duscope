//! `by-type`: bytes and counts by file class, with each class's largest
//! files.
//!
//! A file's class is that of the outermost directory above it that a
//! directory rule matches (a rule wins over the extension of every file
//! beneath it), else that of its extension in the classes table. A file
//! without an extension is `binaries` when the scan collected its mode and
//! an execute bit is set, `other` otherwise, as is an extension the table
//! does not list; a directory rule matching a directory above the root
//! applies too. A hard-linked file counts once, at the link charged with
//! it; a clone's allocated bytes count at the member charged with them,
//! its apparent bytes and the file itself at each member, as in the
//! totals. Classes sort by `--sort` (size by default; file count; newest
//! file; name), and `other` is always the last row.

use std::borrow::Cow;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use crate::CoreError;
use crate::envelope::{ByTypeEntry, Example, name_fields};
use crate::index::writer::LATER_LINK;
use crate::rules::{Class, Classes, ext};

use super::{Filters, Node, Page, SizeKind, SortKey, TreeView, abs_path, pct};

/// What `by-type` was asked.
#[derive(Debug, Clone)]
pub struct ByTypeQuery<'c> {
    /// The classes table, a user's merged over the vendored one.
    pub classes: &'c Classes,
    /// `--examples`: the largest files listed per class.
    pub examples: usize,
    /// The shared filters, applied to files.
    pub filters: Filters,
    /// `--limit`: the classes listed; `None` lists all.
    pub limit: Option<usize>,
    /// The size classes are ranked and shown by.
    pub size: SizeKind,
    /// `--sort`: by size, file count, newest file or class name.
    pub sort: SortKey,
    /// `-r`: the key reversed; equals keep the legend's order.
    pub reverse: bool,
}

/// What one class adds up to.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Tally {
    allocated: u64,
    apparent: u64,
    files: u64,
    /// The newest file's modification time, Unix nanoseconds.
    newest_mtime_ns: i64,
    /// The largest files: (size, id), largest first.
    largest: Vec<(u64, u32)>,
}

/// The file classes under the root of `view`.
pub fn by_type(view: &(impl TreeView + ?Sized), q: &ByTypeQuery<'_>) -> Page<ByTypeEntry> {
    if view.is_empty() {
        return Page {
            entries: Vec::new(),
            total_count: 0,
            next: None,
        };
    }
    let root = view.node(0);
    let tallies = from_index(view, q).unwrap_or_else(|| tally(view, q));
    let root_total = root.size(q.size);
    let mut classes: Vec<Class> = Class::ALL
        .into_iter()
        .filter(|&class| tallies[index(class)].files > 0)
        .collect();
    let size_of = |class: Class| match q.size {
        SizeKind::Allocated => tallies[index(class)].allocated,
        SizeKind::Apparent => tallies[index(class)].apparent,
    };
    // By the key, `other` last; legend order among equals (the sort is
    // stable and the classes start in that order).
    let key = |class: Class| {
        let tally = &tallies[index(class)];
        let value = match q.sort {
            SortKey::Size => -i128::from(size_of(class)),
            SortKey::Count => -i128::from(tally.files),
            SortKey::Mtime => -i128::from(tally.newest_mtime_ns),
            SortKey::Path => 0,
        };
        if q.reverse { -value } else { value }
    };
    classes.sort_by(|&a, &b| {
        let by_name = match (q.sort, q.reverse) {
            (SortKey::Path, false) => a.as_str().cmp(b.as_str()),
            (SortKey::Path, true) => b.as_str().cmp(a.as_str()),
            _ => std::cmp::Ordering::Equal,
        };
        (a == Class::Other)
            .cmp(&(b == Class::Other))
            .then(key(a).cmp(&key(b)))
            .then(by_name)
    });
    let total_count = classes.len() as u64;
    if let Some(limit) = q.limit {
        classes.truncate(limit);
    }
    let entries = classes
        .into_iter()
        .map(|class| {
            let tally = &tallies[index(class)];
            ByTypeEntry {
                class: class.as_str(),
                allocated_bytes: tally.allocated,
                apparent_bytes: tally.apparent,
                pct_of_root: pct(size_of(class), root_total),
                file_count: tally.files,
                largest: tally
                    .largest
                    .iter()
                    .map(|&(_, id)| {
                        let node = view.node(id);
                        let (name, name_raw) = name_fields(&view.name(id));
                        Example {
                            path: String::from_utf8_lossy(&abs_path(view, id)).into_owned(),
                            name,
                            name_raw,
                            allocated_bytes: node.allocated,
                            apparent_bytes: node.apparent,
                        }
                    })
                    .collect(),
            }
        })
        .collect();
    Page {
        entries,
        total_count,
        next: None,
    }
}

/// The names of the directories above `root`, outermost first, which the
/// directory rules match with the names below them; and the class a rule
/// gives a directory among them, the outermost's, which everything under
/// the root then takes.
pub fn root_dirs<'p>(classes: &Classes, root: &'p Path) -> (Vec<&'p [u8]>, Option<Class>) {
    let mut components: Vec<&[u8]> = root
        .as_os_str()
        .as_bytes()
        .split(|&b| b == b'/')
        .filter(|c| !c.is_empty())
        .collect();
    components.pop();
    let class = (1..=components.len()).find_map(|end| classes.of_dir(&components[..end]));
    (components, class)
}

/// Calls `each` with every entry under the root of `view`, in pre-order,
/// and its class: a directory's is that of the outermost directory rule
/// above or at it, the directories above the root included (`other` when
/// none matches); a file's is that rule's, else its extension's in the
/// table, else `binaries` for a file without an extension whose mode has
/// an execute bit, else `other`. `by-type` tallies these classes and the
/// treemap colours its tiles with them, so the legend and the tiles agree.
pub fn classify(
    view: &(impl TreeView + ?Sized),
    classes: &Classes,
    mut each: impl FnMut(&Node, Class),
) {
    if view.is_empty() {
        return;
    }
    let root = view.node(0);
    // The directory rules match a directory's last path components, the
    // root's own included.
    let (above_root, above) = root_dirs(classes, view.root_path());
    let mut components: Vec<Cow<'_, [u8]>> = above_root.into_iter().map(Cow::Borrowed).collect();
    // The directories open above the current id: (last id, its class).
    let mut open: Vec<(u32, Option<Class>)> = vec![(u32::MAX, above)];
    for id in 0..=root.last_id {
        let node = view.node(id);
        while open.last().is_some_and(|&(last, _)| id > last) {
            open.pop();
            components.pop();
        }
        let inherited = open.last().and_then(|&(_, class)| class);
        if node.is_dir() {
            components.push(view.name(id));
            let class = inherited.or_else(|| classes.of_dir(&components));
            open.push((node.last_id, class));
            each(&node, class.unwrap_or(Class::Other));
            continue;
        }
        let class = inherited.unwrap_or_else(|| {
            let suffix = ext(&view.name(id));
            if suffix.is_empty() {
                let executable = node.mode.is_some_and(|mode| mode & 0o111 != 0);
                if executable {
                    Class::Binaries
                } else {
                    Class::Other
                }
            } else {
                classes.of_ext(&suffix).unwrap_or(Class::Other)
            }
        });
        each(&node, class);
    }
}

/// Each class's files under the root, by [`index`].
fn tally(view: &(impl TreeView + ?Sized), q: &ByTypeQuery<'_>) -> Vec<Tally> {
    let mut tallies: Vec<Tally> = vec![Tally::default(); Class::ALL.len()];
    classify(view, q.classes, |node, class| {
        if node.is_dir() || node.linked_away || !q.filters.keeps(view, node, q.size) {
            return;
        }
        let id = node.id;
        let tally = &mut tallies[index(class)];
        tally.allocated += node.counted(SizeKind::Allocated);
        tally.apparent += node.counted(SizeKind::Apparent);
        tally.newest_mtime_ns = if tally.files == 0 {
            node.mtime_ns
        } else {
            tally.newest_mtime_ns.max(node.mtime_ns)
        };
        tally.files += 1;
        if q.examples > 0 {
            let size = node.counted(q.size);
            let at = tally
                .largest
                .partition_point(|&(s, other)| s > size || (s == size && other < id));
            if at < q.examples {
                tally.largest.insert(at, (size, id));
                tally.largest.truncate(q.examples);
            }
        }
    });
    tallies
}

/// The tallies of a whole saved scan read without a filter, from its own
/// tables: `type_stats` for each extension's files; the outermost
/// directories a directory rule matches, read from `dirs` in pre-order as
/// [`classify`] reads them, their files' tallies moved to the rule's class;
/// and each class's largest files from one pass down `files_allocated`,
/// whose order (allocated bytes down, ids up among equals) is the walk's.
/// `None` where the walk is needed: a subtree, a filter, a scan with modes
/// (an extensionless file's class reads its execute bit), `--sort mtime`
/// (the newest file is not in the tables), apparent sizes, or a class whose
/// examples would include a file that counts no bytes.
fn from_index(view: &(impl TreeView + ?Sized), q: &ByTypeQuery<'_>) -> Option<Vec<Tally>> {
    let (saved, base) = view.as_index()?;
    if base != 0
        || !q.filters.is_empty()
        || view.full_attrs()
        || q.sort == SortKey::Mtime
        || q.size != SizeKind::Allocated
    {
        return None;
    }
    let read = || -> rusqlite::Result<Option<Vec<Tally>>> {
        let db = saved.db();
        let class_of_ext = |suffix: &str| {
            if suffix.is_empty() {
                Class::Other
            } else {
                q.classes.of_ext(suffix).unwrap_or(Class::Other)
            }
        };
        let (_, above) = root_dirs(q.classes, view.root_path());
        let ranges = rule_ranges(view, q.classes)?;
        let mut tallies: Vec<Tally> = vec![Tally::default(); Class::ALL.len()];
        let mut add = |class: Class, files: i64, allocated: i64, apparent: i64, sign: i64| {
            let tally = &mut tallies[index(class)];
            let apply = |total: &mut u64, n: i64| {
                *total = total.saturating_add_signed(sign.saturating_mul(n));
            };
            apply(&mut tally.files, files);
            apply(&mut tally.allocated, allocated);
            apply(&mut tally.apparent, apparent);
        };
        if above.is_none() {
            let mut stmt = db.prepare("SELECT ext, n, allocated, apparent FROM type_stats")?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                let suffix: String = row.get(0)?;
                add(
                    class_of_ext(&suffix),
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    1,
                );
            }
        }
        // Counted as `type_stats` counts: a later link not at all, a
        // clone's bytes on disk at the member charged with them.
        let mut stmt = db.prepare(&format!(
            "SELECT ext, SUM(flags & {LATER_LINK} = 0), \
             SUM(CASE WHEN flags & {LATER_LINK} = 0 AND charged THEN allocated ELSE 0 END), \
             SUM(CASE WHEN flags & {LATER_LINK} = 0 THEN apparent ELSE 0 END) \
             FROM files WHERE id BETWEEN ?1 AND ?2 GROUP BY ext"
        ))?;
        for &(first, last, class) in &ranges {
            let mut rows = stmt.query([first, last])?;
            while let Some(row) = rows.next()? {
                let suffix: String = row.get(0)?;
                let (files, allocated, apparent): (i64, i64, i64) =
                    (row.get(1)?, row.get(2)?, row.get(3)?);
                if above.is_none() {
                    add(class_of_ext(&suffix), files, allocated, apparent, -1);
                }
                add(class, files, allocated, apparent, 1);
            }
        }
        if q.examples == 0 {
            return Ok(Some(tallies));
        }
        let class_at = |id: u32, suffix: &str| {
            let at = ranges.partition_point(|&(first, _, _)| first <= id);
            match at.checked_sub(1).map(|i| ranges[i]) {
                Some((_, last, class)) if id <= last => class,
                _ => class_of_ext(suffix),
            }
        };
        let settled = largest_by_class(saved, q.examples, &mut tallies, class_at)?;
        Ok(settled.then_some(tallies))
    };
    read().unwrap_or_else(|e| {
        saved.record(&CoreError::IndexCorrupt {
            file: saved.file().display().to_string(),
            message: e.to_string(),
        });
        None
    })
}

/// Each class's `examples` largest files, from one pass down
/// `files_allocated` that stops once every class is full: at its examples,
/// or at its every file. `class_at` gives a file's class from its id and
/// extension. Whether every class is settled: it lists every one of its
/// files, or its examples all count bytes (a file that counts none, an
/// uncharged clone member or an empty file, ranks by id among the others,
/// which the walk decides).
fn largest_by_class(
    saved: &crate::index::Index,
    examples: usize,
    tallies: &mut [Tally],
    class_at: impl Fn(u32, &str) -> Class,
) -> rusqlite::Result<bool> {
    let wanted = tallies.iter().filter(|t| t.files > 0).count();
    let mut full = 0;
    let mut stmt = saved.db().prepare(&format!(
        "SELECT id, allocated, ext FROM files INDEXED BY files_allocated \
         WHERE charged AND flags & {LATER_LINK} = 0 ORDER BY allocated DESC, id"
    ))?;
    let mut rows = stmt.query([])?;
    while full < wanted
        && let Some(row) = rows.next()?
    {
        let id: u32 = row.get(0)?;
        let size = unsigned(row.get(1)?);
        let suffix: String = row.get(2)?;
        let tally = &mut tallies[index(class_at(id, &suffix))];
        let target = u64::try_from(examples).map_or(tally.files, |n| n.min(tally.files));
        if (tally.largest.len() as u64) < target {
            tally.largest.push((size, id));
            if tally.largest.len() as u64 == target {
                full += 1;
            }
        }
    }
    Ok(tallies.iter().all(|t| {
        t.largest.len() as u64 == t.files
            || (t.largest.len() == examples && t.largest.last().is_some_and(|l| l.0 > 0))
    }))
}

/// The outermost directories of a whole saved scan that a directory rule
/// matches, as (first id, last id, class), in id order, read from `dirs` in
/// pre-order as [`classify`] reads the tree; a rule matching a directory
/// above the root covers everything.
fn rule_ranges(
    view: &(impl TreeView + ?Sized),
    classes: &Classes,
) -> rusqlite::Result<Vec<(u32, u32, Class)>> {
    let (above_root, above) = root_dirs(classes, view.root_path());
    if let Some(class) = above {
        return Ok(vec![(0, view.node(0).last_id, class)]);
    }
    let Some((saved, _)) = view.as_index() else {
        return Ok(Vec::new());
    };
    let mut ranges = Vec::new();
    let mut components: Vec<Vec<u8>> = above_root.iter().map(|c| c.to_vec()).collect();
    let mut open: Vec<u32> = Vec::new();
    let mut stmt = saved
        .db()
        .prepare("SELECT id, last_id, name FROM dirs ORDER BY id")?;
    let mut rows = stmt.query([])?;
    let mut inside: Option<u32> = None;
    while let Some(row) = rows.next()? {
        let id: u32 = row.get(0)?;
        let last: u32 = row.get(1)?;
        while open.last().is_some_and(|&l| id > l) {
            open.pop();
            components.pop();
        }
        if inside.is_some_and(|l| id > l) {
            inside = None;
        }
        // The root's row holds its whole path; the rules read its last
        // component, as the walk names it.
        let name = if id == 0 {
            view.name(0).into_owned()
        } else {
            row.get(2)?
        };
        components.push(name);
        open.push(last);
        if inside.is_none()
            && let Some(class) = classes.of_dir(&components)
        {
            ranges.push((id, last, class));
            inside = Some(last);
        }
    }
    Ok(ranges)
}

/// A count or size read from a signed column.
fn unsigned(n: i64) -> u64 {
    u64::try_from(n).unwrap_or(0)
}

/// A class's place in [`Class::ALL`].
fn index(class: Class) -> usize {
    Class::ALL.iter().position(|&c| c == class).unwrap_or(0)
}

#[cfg(test)]
mod tests;
