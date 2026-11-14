//! `tree`: the root and its entries down to `--depth` (default 2), each
//! directory's children largest first (or in `--sort` order), with each
//! entry's share of the root and of its parent. `--min-pct` leaves out an
//! entry below that share of its parent, and what is under it. A file whose
//! bytes are charged elsewhere (a later hard link, a clone) has a share of
//! nothing and sorts as empty, so the shares add up.

use std::collections::BinaryHeap;

use crate::envelope::{TreeEntry, name_fields, path_fields};

use super::{
    Filters, Node, Page, SizeKind, SortKey, TreeView, abs_path, children, flag_names, kind_name,
    pct, remain, sort_ids_by, timegrammar::iso8601,
};

/// What `tree` was asked.
#[derive(Debug, Clone, Default)]
pub struct TreeQuery {
    /// `--depth`; `None` is 2.
    pub depth: Option<u32>,
    /// `--min-pct`: the share of its parent below which an entry is left
    /// out, in percent.
    pub min_pct: f64,
    /// The shared filters.
    pub filters: Filters,
    /// The order of each directory's children.
    pub sort: SortKey,
    /// `-r`.
    pub reverse: bool,
    /// `--limit`; `None` returns every row.
    pub limit: Option<usize>,
    /// The size rows are ranked and shown by.
    pub size: SizeKind,
}

/// `tree`'s rows, kept and not yet built: their ids in pre-order, what the
/// page says of them, and what each row needs besides its entry. Rows are
/// built one at a time from it ([`TreeRows::rows`]), for a writer that
/// streams them, or all at once ([`TreeRows::page`]).
#[derive(Debug, Clone, PartialEq)]
pub struct TreeRows {
    /// Rows matched before `--limit`.
    pub total_count: u64,
    /// Whether rows remain after these.
    pub truncated: bool,
    /// The rows' ids, in pre-order, each with its parent's size, which its
    /// share is of.
    rows: Vec<(u32, u64)>,
    /// The root's size, which each row's share of the root is of.
    root_total: u64,
    /// The size rows are shown by.
    size: SizeKind,
}

impl TreeRows {
    /// How many rows there are.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether there are none.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// The row at `at`, counted from 0.
    pub fn row(&self, view: &(impl TreeView + ?Sized), at: usize) -> TreeEntry {
        let (id, parent_total) = self.rows[at];
        let node = view.node(id);
        let share = pct(node.counted(self.size), parent_total);
        entry(view, &node, self.root_total, share, self.size)
    }

    /// The rows in order, each built as it is read.
    pub fn rows<'a, V: TreeView + ?Sized>(
        &'a self,
        view: &'a V,
    ) -> impl Iterator<Item = TreeEntry> + 'a {
        (0..self.rows.len()).map(move |at| self.row(view, at))
    }

    /// The page: every row built.
    pub fn page(self, view: &(impl TreeView + ?Sized)) -> Page<TreeEntry> {
        Page {
            entries: self.rows(view).collect(),
            total_count: self.total_count,
            next: None,
            truncated: self.truncated,
        }
    }
}

/// The tree under the root of `view`, in pre-order. `--limit` keeps the
/// shallowest rows: every row of one depth before any row below it, the
/// earlier in pre-order first within a depth, so a kept row's parent is
/// kept too; the kept rows stay in pre-order.
pub fn tree(view: &(impl TreeView + ?Sized), q: &TreeQuery) -> Page<TreeEntry> {
    tree_rows(view, q).page(view)
}

/// [`tree`]'s rows, kept and not yet built.
pub fn tree_rows(view: &(impl TreeView + ?Sized), q: &TreeQuery) -> TreeRows {
    if view.is_empty() {
        return TreeRows {
            total_count: 0,
            truncated: false,
            rows: Vec::new(),
            root_total: 0,
            size: q.size,
        };
    }
    let depth = q.depth.unwrap_or(2);
    let root = view.node(0);
    let root_total = root.size(q.size);
    // The rows kept so far, the worst on top: (depth, pre-order number,
    // id, the parent's size).
    let mut kept: BinaryHeap<(u32, u64, u32, u64)> = BinaryHeap::new();
    let mut total_count = 0u64;
    // (id, its parent's size), the next to visit last.
    let mut stack = vec![(0u32, root_total)];
    while let Some((id, parent_total)) = stack.pop() {
        let node = view.node(id);
        let share = pct(node.counted(q.size), parent_total);
        if id != 0 && share < q.min_pct {
            continue;
        }
        if q.filters.keeps(view, &node, q.size) {
            total_count += 1;
            let row = (node.depth, total_count, id, parent_total);
            match q.limit {
                Some(limit) if kept.len() >= limit => {
                    if kept.peek().is_some_and(|worst| row < *worst) {
                        kept.pop();
                        kept.push(row);
                    }
                }
                _ => kept.push(row),
            }
        }
        if node.is_dir() && node.depth < depth {
            let mut kids: Vec<u32> = children(view, &node).collect();
            sort_ids_by(view, &mut kids, q.sort, q.reverse, |n| n.counted(q.size));
            let total = node.size(q.size);
            stack.extend(kids.into_iter().rev().map(|kid| (kid, total)));
        }
    }
    let mut rows = kept.into_vec();
    rows.sort_unstable_by_key(|&(_, order, _, _)| order);
    let rows: Vec<(u32, u64)> = rows
        .into_iter()
        .map(|(_, _, id, parent_total)| (id, parent_total))
        .collect();
    TreeRows {
        truncated: remain(0, rows.len(), total_count),
        total_count,
        rows,
        root_total,
        size: q.size,
    }
}

/// The row of `node`, `share` of its parent.
fn entry(
    view: &(impl TreeView + ?Sized),
    node: &Node,
    root_total: u64,
    share: f64,
    size: SizeKind,
) -> TreeEntry {
    let path_bytes = abs_path(view, node.id);
    let (path, path_raw) = path_fields(&path_bytes);
    let (name, name_raw) = name_fields(&view.name(node.id));
    TreeEntry {
        path,
        path_bytes,
        name,
        name_raw,
        kind: kind_name(node.kind),
        depth: node.depth,
        allocated_bytes: node.allocated,
        apparent_bytes: node.apparent,
        pct_of_root: pct(node.counted(size), root_total),
        pct_of_parent: if node.id == 0 { 100.0 } else { share },
        file_count: node.file_count,
        dir_count: node.dir_count,
        max_mtime: iso8601(node.max_mtime_ns),
        flags: flag_names(node),
        path_raw,
    }
}
