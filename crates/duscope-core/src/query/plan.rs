//! How an index reads a subtree's largest files, for `top --files` and
//! `stale`, kept to a [`Cut`]: the filters that read no path (kinds, size
//! bounds, a time window, depths, `stale`'s own cut) and the size ranked
//! by. Three plans read them:
//!
//! - **Range:** the subtree's id range of `files`, filtered and sorted. Its
//!   cost is the subtree's file count, whatever the cut keeps.
//! - **Walk:** the size index (`files_allocated`, `INDEXED BY`) from the
//!   largest, kept to the id range and filtered as it goes, until the limit
//!   is reached. Its cost is how far it walks: little when the matches are
//!   common, the whole index when none match. Apparent size has no index,
//!   so a walk by apparent size is the range.
//! - **Dirs:** the subtree's directories at the depths the cut keeps, each
//!   one's own files read by their id range (`files` has no depth column).
//!
//! The database's own planner takes the range even where the walk is
//! thousands of times faster, so the choice is made here. Without a
//! condition the count is the subtree's `n_files`, and the subtree's size
//! decides ([`Plan::choose`]). With one, the matches are counted first, and
//! the count chooses: in a large subtree, a band of allocated sizes that
//! the index counts cheaply (a narrow one, or one that is the only
//! condition in half the scan or more) is counted and read along the size
//! index; otherwise the matches are counted over the range (or the
//! directories, for a depth), and the walk is taken when they are common
//! enough that it stops early (`walks`).
//!
//! Every plan returns the rows the arena's queries return: ranked by size,
//! ties by path (the tied rows found in path order by
//! `Index::first_in_path_order`, never by id), and the count before the
//! limit.

use std::collections::VecDeque;

use rusqlite::OptionalExtension;
use rusqlite::types::Value;

use super::{Filters, Node, SizeKind, SortKey, TimeField, TreeView, sort_ids_by};
use crate::CoreError;
use crate::index::Index;

/// How a ranked read of a subtree's files is made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plan {
    /// The subtree's id range, filtered and sorted by size.
    Range,
    /// The size index, walked from the largest, filtered to the id range
    /// and the cut; the range when ranking by apparent size.
    Walk,
    /// The subtree's directories at the depths kept, each one's own files
    /// read by their id range, filtered and sorted by size.
    Dirs,
}

impl Plan {
    /// The plan for every file of a subtree of `files` files in a scan of
    /// `total`: the range below 3·√total, the walk from there.
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

/// Whether walking the size index for `limit` of the `matched` files a cut
/// keeps beats reading the `read` rows the range (or the directories) must,
/// in a subtree of `files` files of a scan of `total`. The walk visits
/// about `limit / matched` of the index, which covers the whole scan, and
/// with a `residual` condition (one the index does not hold) reads the
/// table row of each entry in the subtree it meets.
///
/// Measured on T2 (875,000 files, warm, a release build on an arm64
/// Mac): a row of the range costs about 41 ns with a condition to judge,
/// an entry of the size index about 19 ns (half a row), and one whose
/// table row a condition reads about 95 ns, so the table row adds about
/// two rows. The walk's costs are weighed with a margin of four, since
/// the matches need not be spread evenly over the sizes, and a walk that
/// meets them late reads the whole index.
fn walks(matched: u64, read: u64, files: u64, total: u64, limit: usize, residual: bool) -> bool {
    let limit = u128::try_from(limit).unwrap_or(u128::MAX);
    let per_match = 2 * u128::from(total) + if residual { 8 * u128::from(files) } else { 0 };
    u128::from(matched) * u128::from(read) >= limit.saturating_mul(per_match)
}

/// What a ranked read keeps of a subtree's files, and the size it ranks
/// them by: the filters that read no path. The default keeps every file and
/// ranks by allocated size.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Cut {
    /// The size the files are ranked, bounded and tied by.
    pub size: SizeKind,
    /// The kinds kept, as [`super::kind_name`] names them (`--type`); empty
    /// keeps every kind.
    pub kinds: Vec<&'static str>,
    /// Size at least this.
    pub min_size: u64,
    /// Size at most this.
    pub max_size: Option<u64>,
    /// Depth below the subtree's root at least this (a file directly in it
    /// is at 1).
    pub min_depth: Option<u32>,
    /// Depth below the subtree's root at most this.
    pub max_depth: Option<u32>,
    /// The time `after` and `before` read.
    pub time: TimeField,
    /// That time at or after this; a file without that time is left out.
    pub after: Option<i64>,
    /// That time before this; a file without that time is left out.
    pub before: Option<i64>,
    /// `stale`'s cut: modification and change times both before this.
    pub unchanged_before: Option<i64>,
    /// With `unchanged_before`, the access time before it too; a file
    /// without one is left out.
    pub atime: bool,
}

impl Cut {
    /// The cut `filters` make, ranking by `size`; `None` when one of them
    /// reads paths (`--glob`, `--regex`, `--exclude`, so `--invert` too),
    /// which the index holds no column for.
    pub fn of(filters: &Filters, size: SizeKind) -> Option<Cut> {
        if filters.glob.is_some() || filters.regex.is_some() || filters.exclude.is_some() {
            return None;
        }
        Some(Cut {
            size,
            kinds: filters.kinds.clone(),
            min_size: filters.min_size.unwrap_or(0),
            max_size: filters.max_size,
            min_depth: filters.min_depth,
            max_depth: filters.max_depth,
            time: filters.time,
            after: filters.after,
            before: filters.before,
            unchanged_before: None,
            atime: false,
        })
    }

    /// The column of `size`.
    fn column(&self) -> &'static str {
        match self.size {
            SizeKind::Allocated => "allocated",
            SizeKind::Apparent => "apparent",
        }
    }

    /// Whether it bounds the size ranked by.
    fn bounds_size(&self) -> bool {
        self.min_size > 0 || self.max_size.is_some()
    }

    /// Whether it has a condition besides the size bounds and the depths.
    fn has_rest(&self) -> bool {
        !self.kinds.is_empty()
            || self.after.is_some()
            || self.before.is_some()
            || self.unchanged_before.is_some()
    }

    /// Whether it bounds the depth.
    fn bounds_depth(&self) -> bool {
        self.min_depth.is_some() || self.max_depth.is_some()
    }
}

/// A subtree's largest files: the ids, ranked by size, ties by path, and
/// how many files matched before the limit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ranked {
    /// The ids, at most the limit.
    pub ids: Vec<u32>,
    /// The files that matched.
    pub total_count: u64,
}

/// A cut as it applies to one subtree: the cut, and the depths of the
/// directories whose own files it keeps (inclusive; `None` keeps every
/// depth). A file's depth is its parent directory's plus one, as the
/// index reads it, and a cut counts depth from the subtree's root.
struct Terms<'c> {
    cut: &'c Cut,
    parents: Option<(i64, i64)>,
}

/// The values of a statement's numbered parameters from `?{first}` on,
/// gathered as its conditions are written.
#[derive(Debug, Clone)]
struct Binds {
    first: usize,
    values: Vec<Value>,
}

impl Binds {
    /// The parameters after the first `fixed`.
    fn after(fixed: usize) -> Binds {
        Binds {
            first: fixed + 1,
            values: Vec::new(),
        }
    }

    /// A parameter holding `value`, as the SQL names it.
    fn mark(&mut self, value: impl Into<Value>) -> String {
        self.values.push(value.into());
        format!("?{}", self.first + self.values.len() - 1)
    }

    /// The statement's parameters: `fixed`, then these.
    fn params(&self, fixed: impl IntoIterator<Item = Value>) -> Vec<Value> {
        fixed
            .into_iter()
            .chain(self.values.iter().cloned())
            .collect()
    }
}

/// Conditions joined by `AND`; none is true.
fn all_of(conds: &[String]) -> String {
    if conds.is_empty() {
        "1".to_owned()
    } else {
        conds.join(" AND ")
    }
}

/// How a plan reads a subtree: the plan, `FROM` and its joins, the
/// conditions that pick the rows it reads (the subtree's ids, `?1` to `?2`,
/// and for the directories their depths), those that judge each row, and
/// the values.
struct Read {
    plan: Plan,
    from: &'static str,
    reads: Vec<String>,
    judged: Vec<String>,
    binds: Binds,
}

impl Read {
    /// Its parameters, the subtree's first and last ids first.
    fn params(&self, first: u32, last: u32) -> Vec<Value> {
        self.binds.params([first.into(), last.into()])
    }

    /// `FROM` to the end of `WHERE`.
    fn rows(&self) -> String {
        let conds: Vec<String> = self.reads.iter().chain(&self.judged).cloned().collect();
        format!("{} WHERE {}", self.from, all_of(&conds))
    }
}

impl<'c> Terms<'c> {
    /// The terms of `cut` under `root`; `None` when its depths keep no
    /// file: `--max-depth 0` under a directory, or a minimum depth for a
    /// subtree that is one file (at depth 0).
    fn new(cut: &'c Cut, root: &Node) -> Option<Terms<'c>> {
        if !root.is_dir() {
            if cut.min_depth.is_some_and(|min| min > 0) {
                return None;
            }
            return Some(Terms { cut, parents: None });
        }
        if cut.max_depth == Some(0) {
            return None;
        }
        let parents = cut.bounds_depth().then(|| {
            let base = i64::from(root.depth);
            let lo = base + i64::from(cut.min_depth.unwrap_or(1).max(1)) - 1;
            let hi = cut
                .max_depth
                .map_or(i64::MAX, |max| base + i64::from(max) - 1);
            (lo, hi)
        });
        Some(Terms { cut, parents })
    }

    /// Whether it keeps every file of the subtree.
    fn keeps_all(&self) -> bool {
        !self.cut.bounds_size() && !self.cut.has_rest() && self.parents.is_none()
    }

    /// Whether a walk of the size index must read a table row to judge an
    /// entry: a condition besides the allocated-size band.
    fn residual(&self) -> bool {
        self.cut.has_rest() || self.parents.is_some() || self.cut.size == SizeKind::Apparent
    }

    /// Whether the files of a directory at `depth` are kept.
    fn keeps_depth(&self, depth: u32) -> bool {
        self.parents
            .is_none_or(|(lo, hi)| (lo..=hi).contains(&i64::from(depth)))
    }

    /// Whether a directory at `depth` is below every depth kept: neither
    /// its own files nor any below it are.
    fn below_depths(&self, depth: u32) -> bool {
        self.parents.is_some_and(|(_, hi)| i64::from(depth) > hi)
    }

    /// The size bounds.
    fn band(&self, binds: &mut Binds, out: &mut Vec<String>) {
        let column = self.cut.column();
        if self.cut.min_size > 0 {
            out.push(format!(
                "f.{column} >= {}",
                binds.mark(signed(self.cut.min_size))
            ));
        }
        if let Some(max) = self.cut.max_size {
            out.push(format!("f.{column} <= {}", binds.mark(signed(max))));
        }
    }

    /// The conditions other than the size bounds and the depths.
    fn rest(&self, binds: &mut Binds, out: &mut Vec<String>) {
        let cut = self.cut;
        if !cut.kinds.is_empty() {
            // A row of `files` is never a directory.
            let marks: Vec<String> = cut
                .kinds
                .iter()
                .filter(|&&kind| kind != "dir")
                .map(|&kind| binds.mark(kind.to_owned()))
                .collect();
            out.push(if marks.is_empty() {
                "0".to_owned()
            } else {
                format!("f.kind IN ({})", marks.join(", "))
            });
        }
        let time = match cut.time {
            TimeField::Mtime => "mtime",
            TimeField::Atime => "atime",
            TimeField::Ctime => "ctime",
            TimeField::Btime => "btime",
        };
        if let Some(after) = cut.after {
            out.push(format!("f.{time} >= {}", binds.mark(after)));
        }
        if let Some(before) = cut.before {
            out.push(format!("f.{time} < {}", binds.mark(before)));
        }
        if let Some(before) = cut.unchanged_before {
            let mark = binds.mark(before);
            out.push(format!("f.mtime < {mark} AND f.ctime < {mark}"));
            if cut.atime {
                out.push(format!("f.atime < {mark}"));
            }
        }
    }

    /// The depths, read from each file's parent row.
    fn depth(&self, binds: &mut Binds, out: &mut Vec<String>) {
        if let Some((lo, hi)) = self.parents {
            out.push(format!(
                "(SELECT p.depth FROM dirs AS p WHERE p.id = f.parent_id) BETWEEN {} AND {}",
                binds.mark(lo),
                binds.mark(hi)
            ));
        }
    }

    /// How `plan` reads the subtree.
    fn read(&self, plan: Plan) -> Read {
        let mut binds = Binds::after(2);
        let mut reads = Vec::new();
        let mut judged = Vec::new();
        let plan = self.plan_for(plan);
        let from = match plan {
            Plan::Range => {
                reads.push("f.id BETWEEN ?1 AND ?2".to_owned());
                self.band(&mut binds, &mut judged);
                self.rest(&mut binds, &mut judged);
                self.depth(&mut binds, &mut judged);
                "FROM files AS f NOT INDEXED"
            }
            Plan::Walk => {
                reads.push("f.id BETWEEN ?1 AND ?2".to_owned());
                self.band(&mut binds, &mut judged);
                self.rest(&mut binds, &mut judged);
                self.depth(&mut binds, &mut judged);
                "FROM files AS f INDEXED BY files_allocated"
            }
            Plan::Dirs => {
                reads.push("d.id BETWEEN ?1 AND ?2".to_owned());
                if let Some((lo, hi)) = self.parents {
                    reads.push(format!(
                        "d.depth BETWEEN {} AND {}",
                        binds.mark(lo),
                        binds.mark(hi)
                    ));
                }
                self.band(&mut binds, &mut judged);
                self.rest(&mut binds, &mut judged);
                // Each directory's own files are the ids after it to its
                // files_end, held to the subtree.
                "FROM dirs AS d NOT INDEXED CROSS JOIN files AS f NOT INDEXED \
                 ON f.id BETWEEN d.id + 1 AND min(d.files_end, ?2)"
            }
        };
        Read {
            plan,
            from,
            reads,
            judged,
            binds,
        }
    }

    /// `plan` as it can be taken: a walk by apparent size is the range.
    fn plan_for(&self, plan: Plan) -> Plan {
        match plan {
            Plan::Walk if self.cut.size == SizeKind::Apparent => Plan::Range,
            plan => plan,
        }
    }
}

impl Index {
    /// The largest non-directories under `dir` (ties by path), at most
    /// `limit`, that `cut` keeps (every file, by allocated size, when
    /// `None`), read with `plan`, or the plan the count chooses when `None`.
    pub fn largest_files(
        &self,
        dir: u32,
        limit: usize,
        cut: Option<Cut>,
        plan: Option<Plan>,
    ) -> Result<Ranked, CoreError> {
        let cut = cut.unwrap_or_default();
        let root = self.node(dir);
        let corrupt = |e: rusqlite::Error| CoreError::IndexCorrupt {
            file: self.file().display().to_string(),
            message: e.to_string(),
        };
        let Some(terms) = Terms::new(&cut, &root) else {
            return Ok(Ranked {
                ids: Vec::new(),
                total_count: 0,
            });
        };
        let (first, last) = (dir, root.last_id);
        // A subtree that is one file has no directories to read.
        let plan = plan.map(|p| if root.is_dir() { p } else { Plan::Range });
        let (plan, total_count) = match plan {
            Some(plan) if terms.keeps_all() => (plan, root.file_count),
            Some(plan) => {
                let (_, matched) = self
                    .count(&terms.read(plan), first, last)
                    .map_err(corrupt)?;
                (plan, matched)
            }
            None => self.choose(&terms, &root, limit).map_err(corrupt)?,
        };
        if limit == 0 || total_count == 0 {
            return Ok(Ranked {
                ids: Vec::new(),
                total_count,
            });
        }
        // SQLite's LIMIT is a signed 64-bit integer.
        let cap = i64::try_from(limit).unwrap_or(i64::MAX);
        let column = cut.column();
        let read = terms.read(plan);
        let found: Vec<(u32, i64)> = self
            .db()
            .prepare(&format!(
                "SELECT f.id, f.{column} {} ORDER BY f.{column} DESC, f.id LIMIT {cap}",
                read.rows()
            ))
            .and_then(|mut stmt| {
                stmt.query_map(
                    rusqlite::params_from_iter(read.params(first, last)),
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?
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
        let size_of = |n: &Node| n.size(cut.size);
        let mut ids: Vec<u32> = match found.last() {
            Some(&(_, boundary)) if found.len() == limit => {
                let mut above: Vec<u32> = found
                    .iter()
                    .filter(|&&(_, size)| size != boundary)
                    .map(|&(id, _)| id)
                    .collect();
                sort_ids_by(self, &mut above, SortKey::Size, false, size_of);
                let ties = self
                    .first_in_path_order(dir, boundary, &terms, limit - above.len())
                    .map_err(corrupt)?;
                above.extend(ties);
                above
            }
            _ => {
                let mut all: Vec<u32> = found.into_iter().map(|(id, _)| id).collect();
                sort_ids_by(self, &mut all, SortKey::Size, false, size_of);
                all
            }
        };
        ids.truncate(limit);
        Ok(Ranked { ids, total_count })
    }

    /// The plan for `terms` under `root`, and how many files they keep.
    /// Without a condition, the subtree's size chooses and its file count
    /// is the count. With one, the files kept are counted first: in a
    /// large subtree, a band of allocated sizes the index can count is
    /// counted and read along it ([`Index::by_band`]); otherwise they are
    /// counted where they lie, in the subtree's range or its directories
    /// at the depths kept, which also gives the rows that reading them
    /// there costs, and the walk is taken when it costs less ([`walks`]).
    fn choose(
        &self,
        terms: &Terms<'_>,
        root: &Node,
        limit: usize,
    ) -> rusqlite::Result<(Plan, u64)> {
        let (first, last, files) = (root.id, root.last_id, root.file_count);
        let total = self.node(0).file_count;
        if !root.is_dir() {
            let (_, matched) = self.count(&terms.read(Plan::Range), first, last)?;
            return Ok((Plan::Range, matched));
        }
        let by_size = Plan::choose(files, total);
        if terms.keeps_all() {
            return Ok((terms.plan_for(by_size), files));
        }
        let cut = terms.cut;
        if cut.size == SizeKind::Allocated
            && cut.bounds_size()
            && by_size == Plan::Walk
            && let Some(matched) = self.by_band(terms, root, total)?
        {
            return Ok((Plan::Walk, matched));
        }
        let other = if terms.parents.is_some() {
            Plan::Dirs
        } else {
            Plan::Range
        };
        let (read, matched) = self.count(&terms.read(other), first, last)?;
        let walk = cut.size == SizeKind::Allocated
            && walks(matched, read, files, total, limit, terms.residual());
        Ok((if walk { Plan::Walk } else { other }, matched))
    }

    /// The files `terms` keep under `root`, a large subtree, when the size
    /// index should count and read them, its band of allocated sizes
    /// telling most of them apart: when the index alone judges them and
    /// the subtree is half the scan or more (the index then counts no more
    /// entries than the range reads rows, each for half the cost), or when
    /// the band is narrow, holding less than a sixteenth of the subtree,
    /// which a count of the index alone, stopped there, finds. `None` for a
    /// wide band.
    fn by_band(&self, terms: &Terms<'_>, root: &Node, total: u64) -> rusqlite::Result<Option<u64>> {
        let (first, last, files) = (root.id, root.last_id, root.file_count);
        if !terms.residual() && 2 * files >= total {
            return Ok(Some(self.count(&terms.read(Plan::Walk), first, last)?.1));
        }
        let cap = files / 16 + 1;
        let mut binds = Binds::after(2);
        let mut band = vec!["f.id BETWEEN ?1 AND ?2".to_owned()];
        terms.band(&mut binds, &mut band);
        let in_band: i64 = self.db().query_row(
            &format!(
                "SELECT COUNT(*) FROM (SELECT 1 FROM files AS f INDEXED BY files_allocated \
                 WHERE {} LIMIT {cap})",
                all_of(&band)
            ),
            rusqlite::params_from_iter(binds.params([first.into(), last.into()])),
            |row| row.get(0),
        )?;
        let in_band = u64::try_from(in_band).unwrap_or(0);
        if in_band >= cap {
            return Ok(None);
        }
        Ok(Some(if in_band > 0 && terms.residual() {
            self.count(&terms.read(Plan::Walk), first, last)?.1
        } else {
            in_band
        }))
    }

    /// The rows `read` reads and how many of them its conditions keep;
    /// along the size index, the rows read are not counted, and are given
    /// as the matches.
    fn count(&self, read: &Read, first: u32, last: u32) -> rusqlite::Result<(u64, u64)> {
        let sql = if read.plan == Plan::Walk {
            format!("SELECT COUNT(*), COUNT(*) {}", read.rows())
        } else {
            format!(
                "SELECT COUNT(*), COUNT(*) FILTER (WHERE {}) {} WHERE {}",
                all_of(&read.judged),
                read.from,
                all_of(&read.reads)
            )
        };
        let (rows, matched): (i64, i64) = self.db().query_row(
            &sql,
            rusqlite::params_from_iter(read.params(first, last)),
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Ok((
            u64::try_from(rows).unwrap_or(0),
            u64::try_from(matched).unwrap_or(0),
        ))
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

/// The statements a walk in path order reads tied files with, and the
/// values of their parameters after `?1`, the size, and `?2` and `?3`, the
/// ids: whether a subtree holds one the cut keeps (`holds`), and a page of
/// those among a directory's own files (`page`). The files of a page share
/// their directory's depth, which the walk checks, so the depth is the
/// probe's alone.
struct TieReads<'db> {
    holds: rusqlite::Statement<'db>,
    page: rusqlite::Statement<'db>,
    holds_binds: Binds,
    page_binds: Binds,
}

impl Index {
    /// The statements that find the files tied at a size that `terms`
    /// keep.
    fn tie_reads(&self, terms: &Terms<'_>) -> rusqlite::Result<TieReads<'_>> {
        let column = terms.cut.column();
        let indexed = match terms.cut.size {
            SizeKind::Allocated => "INDEXED BY files_allocated",
            SizeKind::Apparent => "NOT INDEXED",
        };
        let mut binds = Binds::after(3);
        let mut conds = vec![format!("f.{column} = ?1")];
        terms.band(&mut binds, &mut conds);
        terms.rest(&mut binds, &mut conds);
        let page = self.db().prepare(&format!(
            "SELECT f.id, f.name FROM files AS f NOT INDEXED \
             WHERE f.id BETWEEN ?2 AND ?3 AND {} ORDER BY f.id LIMIT {PAGE}",
            all_of(&conds)
        ))?;
        let page_binds = binds.clone();
        terms.depth(&mut binds, &mut conds);
        let holds = self.db().prepare(&format!(
            "SELECT 1 FROM files AS f {indexed} WHERE f.id BETWEEN ?2 AND ?3 AND {} LIMIT 1",
            all_of(&conds)
        ))?;
        Ok(TieReads {
            holds,
            page,
            holds_binds: binds,
            page_binds,
        })
    }

    /// The first `n` files under `dir` in path order (bytewise, as the
    /// arena ranks ties) whose size is `size` and that `terms` keep.
    /// A path orders a directory's children by name, a subdirectory's name
    /// followed by `/`; a directory's own files are stored in name order, so
    /// they are read in pages. A directory whose own files lie at a depth
    /// the cut leaves out is passed through, one below every depth it keeps
    /// is not entered, and a subtree holding no such file, found by a seek
    /// on the size index (a read of its range, by apparent size), is passed
    /// over.
    fn first_in_path_order(
        &self,
        dir: u32,
        size: i64,
        terms: &Terms<'_>,
        n: usize,
    ) -> rusqlite::Result<Vec<u32>> {
        let mut found = Vec::new();
        if n == 0 {
            return Ok(found);
        }
        let TieReads {
            mut holds,
            mut page,
            holds_binds,
            page_binds,
        } = self.tie_reads(terms)?;
        let params = |binds: &Binds, first: u32, last: u32| {
            binds.params([size.into(), first.into(), last.into()])
        };
        // Only rows after the directory, as `child_dirs` reads them: a row
        // that names itself its parent would be entered for ever.
        let mut subdirs = self
            .db()
            .prepare("SELECT name, id FROM dirs WHERE parent_id = ?1 AND id > ?1")?;
        let mut frame = |id: u32| -> rusqlite::Result<Option<Frame>> {
            let node = self.node(id);
            // An entry that does not read as a directory is not walked: a
            // row the index could not read, which the verb reports, reads
            // as an empty entry whose ids nothing checked.
            if !node.is_dir() || terms.below_depths(node.depth) {
                return Ok(None);
            }
            let held = holds
                .query_row(
                    rusqlite::params_from_iter(params(&holds_binds, id, node.last_id)),
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
            // Own files at a depth left out are not read.
            let next = if terms.keeps_depth(node.depth) {
                id + 1
            } else {
                node.files_end.saturating_add(1)
            };
            Ok(Some(Frame {
                next,
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
                        rusqlite::params_from_iter(params(&page_binds, top.next, top.files_end)),
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
