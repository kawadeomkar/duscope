//! `stale`: large files nobody has changed since a cutoff, measured from
//! now (not from the scan): `mtime` and `ctime` both before it, and with
//! `--atime` the access time too, which needs `--attrs full`.

use crate::CoreError;
use crate::envelope::{StaleEntry, name_fields};

use super::{
    After, Cut, Filters, Page, SizeKind, SortKey, TreeView, abs_path, attrs_not_collected,
    flag_names, kind_name, next_after, page_of, ranked_after, ranked_files, remain,
    timegrammar::iso8601,
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
    /// `--cursor`: the page after this row.
    pub after: Option<After>,
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
            next: None,
            truncated: false,
        });
    }
    let cutoff = q.now.saturating_sub(q.older_than);
    let root = view.node(0);
    let after = q.after.as_ref();
    let seen = after.map_or(0, |a| a.seen);
    // Without a filter, from the view's own ranking where it has one (the
    // index's plans): the same cut, the same order.
    let ranked = (root.is_dir()
        && !q.atime
        && q.filters.is_empty()
        && q.sort == SortKey::Size
        && !q.reverse
        && q.size == SizeKind::Allocated)
        .then_some(q.limit)
        .flatten()
        .and_then(|limit| {
            let cut = Cut {
                before: cutoff,
                min_allocated: q.min_size,
            };
            let first = usize::try_from(seen).ok()?.checked_add(limit)?;
            page_of(ranked_files(view, 0, first, Some(cut))?, after)
        });
    let candidates = (0..=root.last_id).filter(|&id| {
        let node = view.node(id);
        !node.is_dir()
            && node.mtime_ns < cutoff
            && node.ctime_ns < cutoff
            && (!q.atime || node.atime_ns.is_some_and(|atime| atime < cutoff))
            && node.size(q.size) >= q.min_size
            && q.filters.keeps(view, &node, q.size)
    });
    let (ids, total_count) = match ranked {
        Some(r) => (r.ids, r.total_count),
        None => ranked_after(
            view,
            candidates,
            q.sort,
            q.reverse,
            q.limit,
            |node: &super::Node| node.size(q.size),
            after,
        ),
    };
    let next = next_after(view, &ids, q.sort, q.size, seen, total_count);
    let entries: Vec<_> = ids
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
        truncated: remain(seen, entries.len(), total_count),
        entries,
        total_count,
        next,
    })
}
