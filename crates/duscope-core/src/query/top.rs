//! `top`: the largest directories (or files) under the root.
//!
//! The rows are the directories at depth `--depth` (default 1), and the
//! shallower directories that have no subdirectories, so the rows split the
//! tree without nesting. With cut-through, a row whose directory holds a
//! child directory of at least 80% of its bytes is replaced by that child,
//! up to three levels down, and `via` names the directories cut through
//! (which is how `Library/Containers` stands beside `Movies`). The filters
//! judge the rows as listed, after cut-through.
//! `--files` lists files instead, at any depth unless `--depth` is given.

use std::collections::HashMap;

use crate::envelope::{TopEntry, name_fields};

use super::{
    Filters, Node, Page, SizeKind, SortKey, TreeView, abs_path, flag_names, kind_name, pct, ranked,
    ranked_files, timegrammar::iso8601,
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
    // The ids each row was cut through, for the rows that were.
    let mut vias: HashMap<u32, Vec<u32>> = HashMap::new();
    // The largest files as the whole view's are, without a filter: from
    // the view's own ranking where it has one (the index's plans).
    let from_view = (q.files
        && root.is_dir()
        && q.depth.is_none()
        && q.filters.is_empty()
        && q.sort == SortKey::Size
        && !q.reverse
        && q.size == SizeKind::Allocated)
        .then_some(q.limit)
        .flatten()
        .and_then(|limit| ranked_files(view, 0, limit, None));
    let (ids, total_count) = if let Some(from_view) = from_view {
        (from_view.ids, from_view.total_count)
    } else if q.files {
        // A root that is a file is its own one row. The files are ranked
        // as they are found, so only the rows --limit can keep are held.
        let max = q.depth.unwrap_or(u32::MAX);
        let candidates = (0..=root.last_id).filter(|&id| {
            let n = view.node(id);
            !n.is_dir() && n.depth <= max && q.filters.keeps(view, &n, q.size)
        });
        ranked(view, candidates, q.sort, q.size, q.reverse, q.limit)
    } else if root.is_dir() {
        let mut rows: Vec<u32> = Vec::new();
        let depth = q.depth.unwrap_or(1).max(1);
        let subdirs = |id: u32| view.child_dirs(id).into_iter().map(|(dir, _)| dir);
        let mut stack: Vec<u32> = subdirs(0).collect();
        while let Some(id) = stack.pop() {
            let node = view.node(id);
            let has_subdirs = node.last_id > node.files_end;
            if node.depth >= depth || !has_subdirs {
                // The filters judge the row that is listed: the directory,
                // or the one it is cut through to, whose path, depth and
                // size the row shows. Cut-through never enters a directory
                // --exclude leaves out.
                let (row, via) = if q.cut_through {
                    cut_through(view, node, q.size, &q.filters)
                } else {
                    (id, Vec::new())
                };
                if !q.filters.keeps(view, &view.node(row), q.size) {
                    continue;
                }
                if !via.is_empty() {
                    vias.insert(row, via);
                }
                rows.push(row);
            } else {
                stack.extend(subdirs(id));
            }
        }
        ranked(view, rows, q.sort, q.size, q.reverse, q.limit)
    } else {
        (Vec::new(), 0)
    };
    let root_total = root.size(q.size);
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
fn cut_through<V: TreeView + ?Sized>(
    view: &V,
    mut node: Node,
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
    node: &Node,
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
