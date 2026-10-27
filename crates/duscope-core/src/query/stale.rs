//! `stale`: large files nobody has changed since a cutoff, measured from
//! now (not from the scan): `mtime` and `ctime` both before it, and with
//! `--atime` the access time too, which needs `--attrs full`.

use crate::CoreError;
use crate::envelope::{StaleEntry, name_fields};

use super::{
    Filters, Page, SizeKind, SortKey, TreeView, abs_path, attrs_not_collected, flag_names,
    kind_name, sort_ids, timegrammar::iso8601,
};

/// `stale`'s default `--min-size`: 10 MiB.
pub const DEFAULT_MIN_SIZE: u64 = 10 << 20;

/// What `stale` was asked.
#[derive(Debug, Clone, Default)]
pub struct StaleQuery {
    /// Now, Unix nanoseconds.
    pub now: i64,
    /// `--older-than`, in nanoseconds: the cutoff is now minus this.
    pub older_than: i64,
    /// `--atime`: the access time must be older too.
    pub atime: bool,
    /// `--min-size`.
    pub min_size: u64,
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

/// The stale files under the root of `view`.
pub fn stale(
    view: &(impl TreeView + ?Sized),
    q: &StaleQuery,
) -> Result<Page<StaleEntry>, CoreError> {
    if !view.full_attrs() {
        if q.atime {
            return Err(attrs_not_collected("stale --atime"));
        }
        if q.filters.needs_full() {
            return Err(attrs_not_collected("--time atime or btime"));
        }
    }
    if view.is_empty() {
        return Ok(Page {
            entries: Vec::new(),
            total_count: 0,
        });
    }
    let cutoff = q.now.saturating_sub(q.older_than);
    let root = view.node(0);
    let mut ids: Vec<u32> = (0..=root.last_id)
        .filter(|&id| {
            let node = view.node(id);
            !node.is_dir()
                && node.mtime_ns < cutoff
                && node.ctime_ns < cutoff
                && (!q.atime || node.atime_ns.is_some_and(|atime| atime < cutoff))
                && node.size(q.size) >= q.min_size
                && q.filters.keeps(view, &node, q.size)
        })
        .collect();
    let total_count = ids.len() as u64;
    sort_ids(view, &mut ids, q.sort, q.size, q.reverse);
    if let Some(limit) = q.limit {
        ids.truncate(limit);
    }
    let entries = ids
        .into_iter()
        .map(|id| {
            let node = view.node(id);
            let path_bytes = abs_path(view, id);
            let (name, name_raw) = name_fields(&view.name(id));
            StaleEntry {
                path: String::from_utf8_lossy(&path_bytes).into_owned(),
                path_bytes,
                name,
                name_raw,
                kind: kind_name(node.kind),
                allocated_bytes: node.allocated,
                apparent_bytes: node.apparent,
                mtime: iso8601(node.mtime_ns),
                ctime: iso8601(node.ctime_ns),
                atime: node.atime_ns.map(iso8601),
                nlink: node.nlink,
                flags: flag_names(&node),
            }
        })
        .collect();
    Ok(Page {
        entries,
        total_count,
    })
}
