//! `diff`: what changed between two scans of one tree.
//!
//! The two trees are walked together, a directory's children matched by
//! name: its files first, then its subdirectories, each bytewise, as both
//! scans number them. A name in one scan only is `added` or `removed`, one
//! entry however large its subtree (`collapsed_count` says how many entries
//! it holds); a name in both whose kind differs is `kind_changed`; a file
//! whose own size changed by at least `--min-delta` is `grown` or
//! `shrunk`. With `--depth`, a directory at that depth is compared whole,
//! and the changes beneath it are its own row's; the totals still count
//! every one. An entry added or removed counts its bytes as the totals do
//! (a later hard link, an uncharged clone counts nothing), so two scans of
//! an unchanged tree differ nowhere, and a hard link that takes the charge
//! from its original leaves the original unchanged. There is no rename
//! detection: a renamed directory is one `removed` and one `added` entry.

use std::cmp::Ordering;

use super::{Node, Page, SizeKind, TreeView, joined, kind_name, rel_path, remain};
use crate::envelope::{DiffEntry, DiffTotals, name_fields, path_fields};

/// `diff`'s default `--min-delta`: 1 MiB.
pub const DEFAULT_MIN_DELTA: u64 = 1 << 20;

/// What `diff` was asked.
#[derive(Debug, Clone)]
pub struct DiffQuery {
    /// `--min-delta`: smaller growth or shrinkage is left out.
    pub min_delta: u64,
    /// `--depth`: directories at this depth are compared whole.
    pub depth: Option<u32>,
    /// The size compared.
    pub size: SizeKind,
    /// `--limit`; `None` returns every row.
    pub limit: Option<usize>,
}

/// What a change is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum What {
    Added,
    Removed,
    KindChanged,
    Grown,
    Shrunk,
}

impl What {
    /// The change as the row spells it.
    fn as_str(self) -> &'static str {
        match self {
            What::Added => "added",
            What::Removed => "removed",
            What::KindChanged => "kind_changed",
            What::Grown => "grown",
            What::Shrunk => "shrunk",
        }
    }
}

/// A change found, before it is spelled as a row: 40 bytes, since every
/// change of a tree is held until they are ranked.
#[derive(Debug, Clone)]
struct Change {
    what: What,
    /// The entry in the new scan, or the old one for `removed`.
    id: u32,
    /// Its bytes in the old scan; 0 for `added`, which has none.
    old_bytes: u64,
    /// Its bytes in the new scan; 0 for `removed`, which has none.
    new_bytes: u64,
    /// The entries an added or removed directory holds, it included.
    collapsed: Option<u64>,
}

impl Change {
    fn delta(&self) -> i128 {
        i128::from(self.new_bytes) - i128::from(self.old_bytes)
    }

    /// Its bytes in the old scan, unless it was added.
    fn before(&self) -> Option<u64> {
        (self.what != What::Added).then_some(self.old_bytes)
    }

    /// Its bytes in the new scan, unless it was removed.
    fn after(&self) -> Option<u64> {
        (self.what != What::Removed).then_some(self.new_bytes)
    }
}

/// `diff`'s rows, ranked and not yet built: each change in order, with
/// its path below the root, and what the page says of them. Rows are built one at a time
/// from it ([`DiffRows::rows`]), each read from the scan its entry is in,
/// for a writer that streams them, or all at once ([`DiffRows::page`]).
#[derive(Debug, Clone)]
pub struct DiffRows {
    /// Rows matched before `--limit`.
    pub total_count: u64,
    /// Whether rows remain after these.
    pub truncated: bool,
    /// The changes listed, in order, each with its path relative to its
    /// scan's root.
    changes: Vec<(Box<[u8]>, Change)>,
}

impl DiffRows {
    /// How many rows there are.
    pub fn len(&self) -> usize {
        self.changes.len()
    }

    /// Whether there are none.
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    /// The row at `at`, counted from 0, read from `old` (a `removed` entry)
    /// or `new`.
    pub fn row(
        &self,
        old: &(impl TreeView + ?Sized),
        new: &(impl TreeView + ?Sized),
        at: usize,
    ) -> DiffEntry {
        let (rel, c) = &self.changes[at];
        let (node, name, root) = if c.what == What::Removed {
            (old.node(c.id), old.name(c.id), old.root_path())
        } else {
            (new.node(c.id), new.name(c.id), new.root_path())
        };
        let path_bytes = joined(root, rel);
        let (name, name_raw) = name_fields(&name);
        let (path, path_raw) = path_fields(&path_bytes);
        DiffEntry {
            path,
            path_bytes,
            name,
            name_raw,
            change: c.what.as_str(),
            kind: kind_name(node.kind),
            old_bytes: c.before(),
            new_bytes: c.after(),
            delta_bytes: i64::try_from(c.delta()).unwrap_or(i64::MAX),
            collapsed_count: c.collapsed,
            path_raw,
        }
    }

    /// The rows in order, each built as it is read.
    pub fn rows<'a, O: TreeView + ?Sized, N: TreeView + ?Sized>(
        &'a self,
        old: &'a O,
        new: &'a N,
    ) -> impl Iterator<Item = DiffEntry> + 'a {
        (0..self.changes.len()).map(move |at| self.row(old, new, at))
    }

    /// The page: every row built.
    pub fn page(
        self,
        old: &(impl TreeView + ?Sized),
        new: &(impl TreeView + ?Sized),
    ) -> Page<DiffEntry> {
        Page {
            entries: self.rows(old, new).collect(),
            total_count: self.total_count,
            next: None,
            truncated: self.truncated,
        }
    }
}

/// The changes from `old` to `new`, two views of one tree, largest first
/// by the size of the change, ties by path; and the totals.
pub fn diff(
    old: &(impl TreeView + ?Sized),
    new: &(impl TreeView + ?Sized),
    q: &DiffQuery,
) -> (Page<DiffEntry>, DiffTotals) {
    let (rows, totals) = diff_rows(old, new, q);
    (rows.page(old, new), totals)
}

/// [`diff`]'s rows, ranked and not yet built, and the totals.
pub fn diff_rows(
    old: &(impl TreeView + ?Sized),
    new: &(impl TreeView + ?Sized),
    q: &DiffQuery,
) -> (DiffRows, DiffTotals) {
    let mut found = Found::default();
    if !old.is_empty() && !new.is_empty() {
        walk(old, new, q, &mut found);
    }
    let mut totals = found.totals;
    totals.net_bytes = signed(size_of(new, 0, q.size)) - signed(size_of(old, 0, q.size));
    // Largest first, the path deciding among equal changes. The two roots
    // are one path, two scans of one tree, and then the paths below it
    // order the rows as the whole paths do.
    let roots = (old.root_path(), new.root_path());
    let one_root = roots.0.as_os_str() == roots.1.as_os_str();
    let whole = |(rel, c): &(Box<[u8]>, Change)| {
        let root = if c.what == What::Removed {
            roots.0
        } else {
            roots.1
        };
        joined(root, rel)
    };
    let mut changes = found.changes;
    changes.sort_by(|a, b| {
        b.1.delta().abs().cmp(&a.1.delta().abs()).then_with(|| {
            if one_root {
                a.0.cmp(&b.0)
            } else {
                whole(a).cmp(&whole(b))
            }
        })
    });
    let total_count = changes.len() as u64;
    if let Some(limit) = q.limit {
        changes.truncate(limit);
    }
    let rows = DiffRows {
        truncated: remain(0, changes.len(), total_count),
        total_count,
        changes,
    };
    (rows, totals)
}

/// What the walk has found so far.
#[derive(Debug, Default)]
struct Found {
    /// Each change, with its path relative to its scan's root.
    changes: Vec<(Box<[u8]>, Change)>,
    totals: DiffTotals,
}

/// The bytes of entry `id` as its parent's total counts them.
fn size_of(view: &(impl TreeView + ?Sized), id: u32, size: SizeKind) -> u64 {
    view.node(id).counted(size)
}

fn signed(n: u64) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

/// The entries in the subtree of `node`, it included.
fn entries(node: &Node) -> u64 {
    if node.is_dir() {
        node.file_count + node.dir_count
    } else {
        1
    }
}

/// Entries by id and name.
type Named = Vec<(u32, Vec<u8>)>;

/// A directory's children, its files and then its subdirectories, as
/// (id, name), in the order both scans number them. Held to the layout as
/// [`super::children`] holds them, so that the walk reads each entry once:
/// a child that is not `dir`'s, or a last subtree that ends past `dir`'s,
/// is the index's failed read, and the lists end there.
fn children(view: &(impl TreeView + ?Sized), dir: u32) -> (Named, Named) {
    let node = view.node(dir);
    let mut files = Vec::new();
    for id in super::ids(view, dir + 1, node.files_end) {
        if !super::own_child(view, dir, id, false) {
            return (files, Vec::new());
        }
        files.push((id, view.name(id).into_owned()));
    }
    // From sibling to sibling through the nodes, whose blocks a walk of the
    // whole tree has read, rather than the index's query per directory.
    let mut dirs = Vec::new();
    let mut next = node.files_end.checked_add(1);
    while let Some(id) = next.filter(|&id| id <= node.last_id) {
        if !super::own_child(view, dir, id, true) {
            return (files, dirs);
        }
        dirs.push((id, view.name(id).into_owned()));
        next = view.node(id).last_id.checked_add(1);
    }
    if node.is_dir() && next != node.last_id.checked_add(1) {
        super::unfilled(view, dir, node.files_end, node.last_id);
    }
    (files, dirs)
}

impl Found {
    /// Notes `change`, of an entry of `view` (the old scan for `removed`,
    /// else the new), with its path below the root, which every change
    /// shares.
    fn push(&mut self, view: &(impl TreeView + ?Sized), change: Change) {
        let path = rel_path(view, change.id).into_boxed_slice();
        self.changes.push((path, change));
    }

    /// `id` of `old`, removed; a row when `report`.
    fn removed(&mut self, old: &(impl TreeView + ?Sized), id: u32, q: &DiffQuery, report: bool) {
        let node = old.node(id);
        let bytes = size_of(old, id, q.size);
        self.totals.removed_count += entries(&node);
        self.totals.removed_bytes += bytes;
        if report {
            let removed = Change {
                what: What::Removed,
                id,
                old_bytes: bytes,
                new_bytes: 0,
                collapsed: node.is_dir().then(|| entries(&node)),
            };
            self.push(old, removed);
        }
    }

    /// `id` of `new`, added; a row when `report`.
    fn added(&mut self, new: &(impl TreeView + ?Sized), id: u32, q: &DiffQuery, report: bool) {
        let node = new.node(id);
        let bytes = size_of(new, id, q.size);
        self.totals.added_count += entries(&node);
        self.totals.added_bytes += bytes;
        if report {
            let added = Change {
                what: What::Added,
                id,
                old_bytes: 0,
                new_bytes: bytes,
                collapsed: node.is_dir().then(|| entries(&node)),
            };
            self.push(new, added);
        }
    }

    /// `was` of `old` and `is` of `new`, one name of another kind; a row
    /// when `report`. A file become a directory (or the reverse) is an
    /// entry removed and one added, in the totals; another kind of file,
    /// an entry changed.
    fn kind_changed(
        &mut self,
        (old, was): (&(impl TreeView + ?Sized), u32),
        (new, is): (&(impl TreeView + ?Sized), u32),
        q: &DiffQuery,
        report: bool,
    ) {
        let (old_bytes, new_bytes) = (size_of(old, was, q.size), size_of(new, is, q.size));
        let (was_node, is_node) = (old.node(was), new.node(is));
        if was_node.is_dir() == is_node.is_dir() {
            self.totals.changed_count += 1;
        } else {
            self.totals.removed_count += entries(&was_node);
            self.totals.added_count += entries(&is_node);
            self.totals.removed_bytes += old_bytes;
            self.totals.added_bytes += new_bytes;
        }
        if report {
            let changed = Change {
                what: What::KindChanged,
                id: is,
                old_bytes,
                new_bytes,
                collapsed: None,
            };
            self.push(new, changed);
        }
    }

    /// A change of size of `is` of `new`, from `old_bytes` to `new_bytes`:
    /// `grown` or `shrunk` when it reaches `--min-delta`.
    fn resized(
        &mut self,
        (new, is): (&(impl TreeView + ?Sized), u32),
        old_bytes: u64,
        new_bytes: u64,
        q: &DiffQuery,
    ) {
        if old_bytes.abs_diff(new_bytes) >= q.min_delta {
            let resized = Change {
                what: if new_bytes > old_bytes {
                    What::Grown
                } else {
                    What::Shrunk
                },
                id: is,
                old_bytes,
                new_bytes,
                collapsed: None,
            };
            self.push(new, resized);
        }
    }
}

/// The names in one scan only (`only_old`, `only_new`, with their names):
/// those removed, those added, and the names both have as a file in one
/// and a directory in the other.
fn unmatched(mut only_old: Named, mut only_new: Named) -> (Vec<u32>, Vec<u32>, Vec<(u32, u32)>) {
    only_old.sort_by(|a, b| a.1.cmp(&b.1));
    only_new.sort_by(|a, b| a.1.cmp(&b.1));
    let (mut removed, mut added, mut kinds) = (Vec::new(), Vec::new(), Vec::new());
    let (mut at_old, mut at_new) = (0, 0);
    while at_old < only_old.len() || at_new < only_new.len() {
        let order = match (only_old.get(at_old), only_new.get(at_new)) {
            (Some(gone), Some(came)) => gone.1.cmp(&came.1),
            (Some(_), None) => Ordering::Less,
            _ => Ordering::Greater,
        };
        match order {
            Ordering::Less => {
                removed.push(only_old[at_old].0);
                at_old += 1;
            }
            Ordering::Greater => {
                added.push(only_new[at_new].0);
                at_new += 1;
            }
            Ordering::Equal => {
                kinds.push((only_old[at_old].0, only_new[at_new].0));
                at_old += 1;
                at_new += 1;
            }
        }
    }
    (removed, added, kinds)
}

/// A directory of each scan at one path, still to compare: how deep, and
/// whether its changes are rows (not beneath a directory compared whole).
struct Open {
    was: u32,
    is: u32,
    depth: u32,
    report: bool,
}

/// Walks both trees from their roots.
fn walk(
    old: &(impl TreeView + ?Sized),
    new: &(impl TreeView + ?Sized),
    q: &DiffQuery,
    found: &mut Found,
) {
    let mut stack: Vec<Open> = Vec::new();
    pair(old, new, (0, 0), 0, true, q, found, &mut stack);
    while let Some(Open {
        was,
        is,
        depth,
        report,
    }) = stack.pop()
    {
        let (old_files, old_dirs) = children(old, was);
        let (new_files, new_dirs) = children(new, is);
        // Files against files, directories against directories; a name
        // that is a file in one and a directory in the other is found as
        // one of each, and matched after.
        let mut only_old: Named = Vec::new();
        let mut only_new: Named = Vec::new();
        let mut pairs: Vec<(u32, u32)> = Vec::new();
        for (olds, news) in [(old_files, new_files), (old_dirs, new_dirs)] {
            merge(olds, news, &mut only_old, &mut only_new, &mut pairs);
        }
        let (removed, added, kinds) = unmatched(only_old, only_new);
        for (gone, came) in kinds {
            found.kind_changed((old, gone), (new, came), q, report);
        }
        for id in removed {
            found.removed(old, id, q, report);
        }
        for id in added {
            found.added(new, id, q, report);
        }
        for ids in pairs {
            pair(old, new, ids, depth + 1, report, q, found, &mut stack);
        }
    }
}

/// Compares `was` of `old` with `is` of `new`, at one path and `depth`: a
/// change of kind; a directory to walk (compared whole as well when it is
/// at `--depth`); a file whose own size changed.
// The walk's state, each piece its own.
#[allow(clippy::too_many_arguments)]
fn pair(
    old: &(impl TreeView + ?Sized),
    new: &(impl TreeView + ?Sized),
    (was, is): (u32, u32),
    depth: u32,
    report: bool,
    q: &DiffQuery,
    found: &mut Found,
    stack: &mut Vec<Open>,
) {
    let (was_node, is_node) = (old.node(was), new.node(is));
    if was_node.kind != is_node.kind {
        found.kind_changed((old, was), (new, is), q, report);
        return;
    }
    if is_node.is_dir() {
        // At the depth asked, the directory's row holds what changed
        // beneath it, which the walk below still counts.
        let whole = q.depth.is_some_and(|d| depth >= d);
        if whole && report {
            let (ob, nb) = (size_of(old, was, q.size), size_of(new, is, q.size));
            found.resized((new, is), ob, nb, q);
        }
        stack.push(Open {
            was,
            is,
            depth,
            report: report && !whole,
        });
        return;
    }
    // A file by its own size: a charge moved to another link is no change.
    let (ob, nb) = (was_node.size(q.size), is_node.size(q.size));
    if ob == nb {
        return;
    }
    found.totals.changed_count += 1;
    if report {
        found.resized((new, is), ob, nb, q);
    }
}

/// Merges two lists of children sorted by name: the names in `olds` only,
/// in `news` only (with their names), and the pairs.
fn merge(
    olds: Named,
    news: Named,
    only_old: &mut Named,
    only_new: &mut Named,
    pairs: &mut Vec<(u32, u32)>,
) {
    let mut olds = olds.into_iter().peekable();
    let mut news = news.into_iter().peekable();
    loop {
        let order = match (olds.peek(), news.peek()) {
            (Some(was), Some(is)) => was.1.cmp(&is.1),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => break,
        };
        match order {
            Ordering::Less => only_old.extend(olds.next()),
            Ordering::Greater => only_new.extend(news.next()),
            Ordering::Equal => {
                if let (Some(was), Some(is)) = (olds.next(), news.next()) {
                    pairs.push((was.0, is.0));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
