//! `tree`: the root and its entries down to `--depth` (default 2), each
//! directory's children largest first (or in `--sort` order), with each
//! entry's share of the root and of its parent. `--min-pct` leaves out an
//! entry below that share of its parent, and what is under it. A file whose
//! bytes are charged elsewhere (a later hard link, a clone) has a share of
//! nothing and sorts as empty, so the shares add up.

use crate::envelope::{TreeEntry, name_fields};

use super::{
    Filters, Node, Page, SizeKind, SortKey, TreeView, abs_path, children, flag_names, kind_name,
    pct, sort_ids_by, timegrammar::iso8601,
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

/// The tree under the root of `view`, in pre-order.
pub fn tree(view: &(impl TreeView + ?Sized), q: &TreeQuery) -> Page<TreeEntry> {
    if view.is_empty() {
        return Page {
            entries: Vec::new(),
            total_count: 0,
        };
    }
    let depth = q.depth.unwrap_or(2);
    let root = view.node(0);
    let root_total = root.size(q.size);
    let mut rows = Vec::new();
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
            if q.limit.is_none_or(|limit| rows.len() < limit) {
                rows.push(entry(view, &node, root_total, share, q.size));
            }
        }
        if node.is_dir() && node.depth < depth {
            let mut kids: Vec<u32> = children(view, &node).collect();
            sort_ids_by(view, &mut kids, q.sort, q.reverse, |n| n.counted(q.size));
            let total = node.size(q.size);
            stack.extend(kids.into_iter().rev().map(|kid| (kid, total)));
        }
    }
    Page {
        entries: rows,
        total_count,
    }
}

/// The row of `node`, `share` of its parent.
fn entry(
    view: &(impl TreeView + ?Sized),
    node: &Node<'_>,
    root_total: u64,
    share: f64,
    size: SizeKind,
) -> TreeEntry {
    let path_bytes = abs_path(view, node.id);
    let (name, name_raw) = name_fields(node.name);
    TreeEntry {
        path: String::from_utf8_lossy(&path_bytes).into_owned(),
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
    }
}
