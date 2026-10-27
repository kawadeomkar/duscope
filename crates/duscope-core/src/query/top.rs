//! `top`: the largest directories (or files) under the root.
//!
//! The rows are the directories at depth `--depth` (default 1), and the
//! shallower directories that have no subdirectories, so the rows split the
//! tree without nesting. With cut-through, a row whose directory holds a
//! child directory of at least 80% of its bytes is replaced by that child,
//! up to three levels down, and `via` names the directories cut through
//! (which is how `Library/Containers` stands beside `Movies`).
//! `--files` lists files instead, at any depth unless `--depth` is given.

use std::collections::HashMap;

use crate::envelope::{TopEntry, name_fields};

use super::{
    Filters, Node, Page, SizeKind, SortKey, TreeView, abs_path, flag_names, kind_name, pct,
    sort_ids, timegrammar::iso8601,
};

/// A child holding at least this share of its directory replaces it.
const CUT_THROUGH_PCT: u64 = 80;
/// How many levels a row may be cut through.
const CUT_THROUGH_LEVELS: usize = 3;

/// What `top` was asked.
#[derive(Debug, Clone, Default)]
pub struct TopQuery {
    /// `--depth`; `None` is 1 for directories and unlimited for files.
    pub depth: Option<u32>,
    /// Off with `--no-cut-through`.
    pub cut_through: bool,
    /// `--files`.
    pub files: bool,
    /// The shared filters.
    pub filters: Filters,
    /// `--sort`.
    pub sort: SortKey,
    /// `-r`.
    pub reverse: bool,
    /// `--limit`; `None` returns every row.
    pub limit: Option<usize>,
    /// The size rows are ranked and shown by.
    pub size: SizeKind,
}

/// The largest entries under the root of `view`.
pub fn top(view: &(impl TreeView + ?Sized), q: &TopQuery) -> Page<TopEntry> {
    if view.is_empty() {
        return Page {
            entries: Vec::new(),
            total_count: 0,
        };
    }
    let root = view.node(0);
    // Each row's id and the ids it was cut through.
    let mut rows: Vec<(u32, Vec<u32>)> = Vec::new();
    if q.files {
        // A root that is a file is its own one row.
        let max = q.depth.unwrap_or(u32::MAX);
        rows.extend(
            (0..=root.last_id)
                .map(|id| view.node(id))
                .filter(|n| !n.is_dir() && n.depth <= max)
                .filter(|n| q.filters.keeps(view, n, q.size))
                .map(|n| (n.id, Vec::new())),
        );
    } else if root.is_dir() {
        let depth = q.depth.unwrap_or(1).max(1);
        let subdirs = |id: u32| view.child_dirs(id).into_iter().map(|(dir, _)| dir);
        let mut stack: Vec<u32> = subdirs(0).collect();
        while let Some(id) = stack.pop() {
            let node = view.node(id);
            let has_subdirs = node.last_id > node.files_end;
            if node.depth >= depth || !has_subdirs {
                // The filters judge the directory at the listed depth; the
                // row it is cut through to stands for it.
                if !q.filters.keeps(view, &node, q.size) {
                    continue;
                }
                rows.push(if q.cut_through {
                    cut_through(view, node, q.size, &q.filters)
                } else {
                    (id, Vec::new())
                });
            } else {
                stack.extend(subdirs(id));
            }
        }
    }
    let total_count = rows.len() as u64;
    let mut ids: Vec<u32> = rows.iter().map(|(id, _)| *id).collect();
    sort_ids(view, &mut ids, q.sort, q.size, q.reverse);
    if let Some(limit) = q.limit {
        ids.truncate(limit);
    }
    let root_total = root.size(q.size);
    let vias: HashMap<u32, Vec<u32>> = rows
        .into_iter()
        .filter(|(_, via)| !via.is_empty())
        .collect();
    let entries = ids
        .into_iter()
        .map(|id| {
            let via = vias.get(&id).map(Vec::as_slice).unwrap_or_default();
            entry(view, &view.node(id), via, root_total, q.size)
        })
        .collect();
    Page {
        entries,
        total_count,
    }
}

/// The row a directory becomes: itself, or the child it is cut through to,
/// never one `--exclude` leaves out.
fn cut_through<'v, V: TreeView + ?Sized>(
    view: &'v V,
    mut node: Node<'v>,
    size: SizeKind,
    filters: &Filters,
) -> (u32, Vec<u32>) {
    let mut via = Vec::new();
    for _ in 0..CUT_THROUGH_LEVELS {
        let bytes = u128::from(node.size(size));
        let dominant = view
            .child_dirs(node.id)
            .into_iter()
            .map(|(id, _)| view.node(id))
            .find(|child| {
                u128::from(child.size(size)) * 100 >= bytes * u128::from(CUT_THROUGH_PCT)
            });
        match dominant {
            Some(child) if bytes > 0 && !filters.excludes(view, &child) => {
                via.push(node.id);
                node = child;
            }
            _ => break,
        }
    }
    (node.id, via)
}

/// The row of `node`.
fn entry(
    view: &(impl TreeView + ?Sized),
    node: &Node<'_>,
    via: &[u32],
    root_total: u64,
    size: SizeKind,
) -> TopEntry {
    let path_bytes = abs_path(view, node.id);
    let (name, name_raw) = name_fields(&view.name(node.id));
    TopEntry {
        path: String::from_utf8_lossy(&path_bytes).into_owned(),
        path_bytes,
        name,
        name_raw,
        kind: kind_name(node.kind),
        depth: node.depth,
        allocated_bytes: node.allocated,
        apparent_bytes: node.apparent,
        pct_of_root: pct(node.size(size), root_total),
        file_count: node.file_count,
        dir_count: node.dir_count,
        max_mtime: iso8601(node.max_mtime_ns),
        via: via
            .iter()
            .map(|&id| String::from_utf8_lossy(&abs_path(view, id)).into_owned())
            .collect(),
        flags: flag_names(node),
    }
}
