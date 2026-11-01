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

use super::{Node, Page, SizeKind, TreeView, abs_path, kind_name, remain};
use crate::envelope::{DiffEntry, DiffTotals, name_fields};

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

/// A change found, before it is spelled as a row.
#[derive(Debug, Clone)]
struct Change {
    what: &'static str,
    /// The entry in the new scan, or the old one for `removed`.
    side: Side,
    id: u32,
    old_bytes: Option<u64>,
    new_bytes: Option<u64>,
    collapsed: Option<u64>,
}

impl Change {
    fn delta(&self) -> i128 {
        i128::from(self.new_bytes.unwrap_or(0)) - i128::from(self.old_bytes.unwrap_or(0))
    }
}

/// Which scan an entry is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Old,
    New,
}

/// The changes from `old` to `new`, two views of one tree, largest first
/// by the size of the change, ties by path; and the totals.
pub fn diff(
    old: &(impl TreeView + ?Sized),
    new: &(impl TreeView + ?Sized),
    q: &DiffQuery,
) -> (Page<DiffEntry>, DiffTotals) {
    let mut found = Found::default();
    if !old.is_empty() && !new.is_empty() {
        walk(old, new, q, &mut found);
    }
    let mut totals = found.totals;
    totals.net_bytes = signed(size_of(new, 0, q.size)) - signed(size_of(old, 0, q.size));
    let mut changes = found.changes;
    // Each row's path, for the order among equal changes.
    let mut keyed: Vec<(i128, Vec<u8>, Change)> = changes
        .drain(..)
        .map(|c| {
            let path = match c.side {
                Side::Old => abs_path(old, c.id),
                Side::New => abs_path(new, c.id),
            };
            (c.delta().abs(), path, c)
        })
        .collect();
    keyed.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let total_count = keyed.len() as u64;
    if let Some(limit) = q.limit {
        keyed.truncate(limit);
    }
    let entries: Vec<DiffEntry> = keyed
        .into_iter()
        .map(|(_, path_bytes, c)| {
            let (node, name) = match c.side {
                Side::Old => (old.node(c.id), old.name(c.id).into_owned()),
                Side::New => (new.node(c.id), new.name(c.id).into_owned()),
            };
            let (name, name_raw) = name_fields(&name);
            DiffEntry {
                path: String::from_utf8_lossy(&path_bytes).into_owned(),
                path_bytes,
                name,
                name_raw,
                change: c.what,
                kind: kind_name(node.kind),
                old_bytes: c.old_bytes,
                new_bytes: c.new_bytes,
                bytes_delta: i64::try_from(c.delta()).unwrap_or(i64::MAX),
                collapsed_count: c.collapsed,
            }
        })
        .collect();
    let page = Page {
        truncated: remain(0, entries.len(), total_count),
        entries,
        total_count,
        next: None,
    };
    (page, totals)
}

/// What the walk has found so far.
#[derive(Debug, Default)]
struct Found {
    changes: Vec<Change>,
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
/// (id, name), in the order both scans number them.
fn children(view: &(impl TreeView + ?Sized), dir: u32) -> (Named, Named) {
    let node = view.node(dir);
    let files = (dir + 1..=node.files_end)
        .filter(|_| node.files_end > dir)
        .map(|id| (id, view.name(id).into_owned()))
        .collect();
    // From sibling to sibling through the nodes, whose blocks a walk of the
    // whole tree has read, rather than the index's query per directory.
    let mut dirs = Vec::new();
    let mut next = node.files_end.checked_add(1);
    while let Some(id) = next.filter(|&id| id <= node.last_id) {
        dirs.push((id, view.name(id).into_owned()));
        next = view.node(id).last_id.checked_add(1);
    }
    (files, dirs)
}

impl Found {
    /// `id` of `old`, removed; a row when `report`.
    fn removed(&mut self, old: &(impl TreeView + ?Sized), id: u32, q: &DiffQuery, report: bool) {
        let node = old.node(id);
        let bytes = size_of(old, id, q.size);
        self.totals.entries_removed += entries(&node);
        self.totals.removed_bytes += bytes;
        if report {
            self.changes.push(Change {
                what: "removed",
                side: Side::Old,
                id,
                old_bytes: Some(bytes),
                new_bytes: None,
                collapsed: node.is_dir().then(|| entries(&node)),
            });
        }
    }

    /// `id` of `new`, added; a row when `report`.
    fn added(&mut self, new: &(impl TreeView + ?Sized), id: u32, q: &DiffQuery, report: bool) {
        let node = new.node(id);
        let bytes = size_of(new, id, q.size);
        self.totals.entries_added += entries(&node);
        self.totals.added_bytes += bytes;
        if report {
            self.changes.push(Change {
                what: "added",
                side: Side::New,
                id,
                old_bytes: None,
                new_bytes: Some(bytes),
                collapsed: node.is_dir().then(|| entries(&node)),
            });
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
            self.totals.entries_changed += 1;
        } else {
            self.totals.entries_removed += entries(&was_node);
            self.totals.entries_added += entries(&is_node);
            self.totals.removed_bytes += old_bytes;
            self.totals.added_bytes += new_bytes;
        }
        if report {
            self.changes.push(Change {
                what: "kind_changed",
                side: Side::New,
                id: is,
                old_bytes: Some(old_bytes),
                new_bytes: Some(new_bytes),
                collapsed: None,
            });
        }
    }

    /// A change of size of `is`, from `old_bytes` to `new_bytes`: `grown`
    /// or `shrunk` when it reaches `--min-delta`.
    fn resized(&mut self, is: u32, old_bytes: u64, new_bytes: u64, q: &DiffQuery) {
        if old_bytes.abs_diff(new_bytes) >= q.min_delta {
            self.changes.push(Change {
                what: if new_bytes > old_bytes {
                    "grown"
                } else {
                    "shrunk"
                },
                side: Side::New,
                id: is,
                old_bytes: Some(old_bytes),
                new_bytes: Some(new_bytes),
                collapsed: None,
            });
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
            found.resized(is, ob, nb, q);
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
    found.totals.entries_changed += 1;
    if report {
        found.resized(is, ob, nb, q);
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
