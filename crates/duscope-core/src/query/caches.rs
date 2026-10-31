//! `caches`: the regenerable cache directories under the root, by the cache
//! rules (`caches.toml`): a name with a marker file in an ancestor (a
//! `node_modules` with a `package.json` above it), a name under an ancestor
//! path (Xcode's `DerivedData`), or any directory holding a valid
//! `CACHEDIR.TAG`. A name without what confirms it is left out (precision
//! over recall) unless `loose`. A cache inside a cache is part of the outer
//! one, never a row of its own.
//!
//! One pass over the view in pre-order. A directory is judged once its own
//! files are read, before its subdirectories: by then the markers of every
//! directory above it are known, and whether it holds a `CACHEDIR.TAG`.
//! Markers above the root are asked of the caller; a tag's signature is
//! read from the filesystem.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use super::{
    Filters, Page, SizeKind, SortKey, TreeView, abs_path, pct, ranked, timegrammar::iso8601,
};
use crate::envelope::{CacheEntry, ProjectEntry, name_fields};
use crate::rules::{Caches, Confirm};

/// `caches`' default `--min-size`: 10 MiB.
pub const DEFAULT_MIN_SIZE: u64 = 10 << 20;

/// The file a valid tag is held in.
const TAG: &[u8] = b"CACHEDIR.TAG";

/// The label of the rule a valid `CACHEDIR.TAG` confirms.
const TAG_RULE: &str = "CACHEDIR.TAG";

/// What `caches` was asked.
pub struct CachesQuery<'a> {
    /// The cache rules, the user's merged over the vendored ones.
    pub caches: &'a Caches,
    /// Whether the directory above the root at `dir` holds a file `name`:
    /// a saved scan's answer where it holds `dir`, else the filesystem's.
    pub above: &'a dyn Fn(&Path, &str) -> bool,
    /// `--loose`: a name without what confirms it is listed too.
    pub loose: bool,
    /// `--min-size`: smaller caches (or, by project, projects) are left out.
    pub min_size: u64,
    /// The shared filters, applied to the caches.
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

impl std::fmt::Debug for CachesQuery<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CachesQuery")
            .field("loose", &self.loose)
            .field("min_size", &self.min_size)
            .finish_non_exhaustive()
    }
}

/// A cache found: its id, the rule that names it, and its project.
#[derive(Debug, Clone)]
struct Found {
    id: u32,
    rule: String,
    regenerable_by: &'static str,
    project: Option<Vec<u8>>,
    confirmed: bool,
    cachedir_tag: bool,
}

/// The cache directories under the root of `view`, largest first.
pub fn caches(view: &(impl TreeView + ?Sized), q: &CachesQuery<'_>) -> Page<CacheEntry> {
    let found = find(view, q);
    let by_id: BTreeMap<u32, &Found> = found.iter().map(|f| (f.id, f)).collect();
    let candidates = found.iter().map(|f| f.id).filter(|&id| {
        let node = view.node(id);
        node.size(q.size) >= q.min_size && q.filters.keeps(view, &node, q.size)
    });
    let (ids, total_count) = ranked(view, candidates, q.sort, q.size, q.reverse, q.limit);
    let root_total = if view.is_empty() {
        0
    } else {
        view.node(0).size(q.size)
    };
    let entries = ids
        .into_iter()
        .filter_map(|id| Some((id, *by_id.get(&id)?)))
        .map(|(id, f)| {
            let node = view.node(id);
            let path_bytes = abs_path(view, id);
            let (name, name_raw) = name_fields(&view.name(id));
            CacheEntry {
                path: String::from_utf8_lossy(&path_bytes).into_owned(),
                path_bytes,
                name,
                name_raw,
                rule: f.rule.clone(),
                regenerable_by: f.regenerable_by,
                project_root: f
                    .project
                    .as_ref()
                    .map(|p| String::from_utf8_lossy(p).into_owned()),
                project_bytes: f.project.clone().unwrap_or_default(),
                confirmed: f.confirmed,
                cachedir_tag: f.cachedir_tag,
                allocated_bytes: node.allocated,
                apparent_bytes: node.apparent,
                pct_of_root: pct(node.size(q.size), root_total),
                file_count: node.file_count,
                max_mtime: iso8601(node.max_mtime_ns),
            }
        })
        .collect();
    Page {
        entries,
        total_count,
    }
}

/// One project's caches, as they are summed.
struct Group {
    allocated: u64,
    apparent: u64,
    newest_ns: i64,
    /// Each cache's size and path.
    caches: Vec<(u64, Vec<u8>)>,
}

/// The caches under the root of `view`, grouped by the directory holding
/// their marker, largest first; the caches no marker confirms form one
/// group of their own, whose `project_root` is null.
pub fn projects(view: &(impl TreeView + ?Sized), q: &CachesQuery<'_>) -> Page<ProjectEntry> {
    let found = find(view, q);
    let mut groups: BTreeMap<Option<Vec<u8>>, Group> = BTreeMap::new();
    for f in &found {
        let node = view.node(f.id);
        if !q.filters.keeps(view, &node, q.size) {
            continue;
        }
        let group = groups.entry(f.project.clone()).or_insert(Group {
            allocated: 0,
            apparent: 0,
            newest_ns: i64::MIN,
            caches: Vec::new(),
        });
        group.allocated += node.allocated;
        group.apparent += node.apparent;
        group.newest_ns = group.newest_ns.max(node.max_mtime_ns);
        group.caches.push((node.size(q.size), abs_path(view, f.id)));
    }
    let root = view.root_path().as_os_str().as_bytes();
    let size = |g: &Group| match q.size {
        SizeKind::Allocated => g.allocated,
        SizeKind::Apparent => g.apparent,
    };
    let mut rows: Vec<(Option<Vec<u8>>, Group)> = groups
        .into_iter()
        .map(|(project, mut group)| {
            // The project's newest time is recursive over the project
            // where the view holds it, else over its caches.
            if let Some(id) = project.as_deref().and_then(|p| dir_id(view, root, p)) {
                group.newest_ns = group.newest_ns.max(view.node(id).max_mtime_ns);
            }
            group
                .caches
                .sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
            (project, group)
        })
        .filter(|(_, group)| size(group) >= q.min_size)
        .collect();
    rows.sort_by(|(pa, a), (pb, b)| {
        let order = match q.sort {
            SortKey::Size => size(b).cmp(&size(a)),
            SortKey::Count => b.caches.len().cmp(&a.caches.len()),
            SortKey::Mtime => b.newest_ns.cmp(&a.newest_ns),
            SortKey::Path => pa.cmp(pb),
        };
        if q.reverse { order.reverse() } else { order }.then_with(|| pa.cmp(pb))
    });
    let total_count = rows.len() as u64;
    if let Some(limit) = q.limit {
        rows.truncate(limit);
    }
    let entries = rows
        .into_iter()
        .map(|(project, group)| ProjectEntry {
            project_root: project
                .as_ref()
                .map(|p| String::from_utf8_lossy(p).into_owned()),
            path_bytes: project.unwrap_or_default(),
            allocated_bytes: group.allocated,
            apparent_bytes: group.apparent,
            cache_count: group.caches.len() as u64,
            max_mtime: iso8601(group.newest_ns),
            caches: group
                .caches
                .into_iter()
                .map(|(_, p)| String::from_utf8_lossy(&p).into_owned())
                .collect(),
        })
        .collect();
    Page {
        entries,
        total_count,
    }
}

/// The id of the directory at `path` (absolute) in `view`, whose root is at
/// `root`; `None` when the view does not hold it.
fn dir_id(view: &(impl TreeView + ?Sized), root: &[u8], path: &[u8]) -> Option<u32> {
    let rel = path.strip_prefix(root)?;
    if !(rel.is_empty() || rel.starts_with(b"/") || root.ends_with(b"/")) {
        return None;
    }
    let mut at = 0;
    for name in rel.split(|&b| b == b'/').filter(|n| !n.is_empty()) {
        at = view
            .child_dirs(at)
            .into_iter()
            .map(|(id, _)| id)
            .find(|&id| *view.name(id) == *name)?;
    }
    Some(at)
}

/// A directory open above the entry being read.
struct Open {
    id: u32,
    files_end: u32,
    last: u32,
    /// Its absolute path.
    path: Vec<u8>,
    /// The marker names among its own files.
    markers: Vec<Vec<u8>>,
    /// Whether its own files include a `CACHEDIR.TAG`.
    tag: bool,
    judged: bool,
}

/// Every cache under the root, outermost only, in pre-order.
fn find(view: &(impl TreeView + ?Sized), q: &CachesQuery<'_>) -> Vec<Found> {
    let mut found = Vec::new();
    if view.is_empty() || !view.node(0).is_dir() {
        return found;
    }
    let marker_names: Vec<&[u8]> = q
        .caches
        .rules()
        .iter()
        .filter_map(|r| match &r.confirm {
            Confirm::Markers(markers) => Some(markers.iter().map(String::as_bytes)),
            _ => None,
        })
        .flatten()
        .collect();
    let mut above = Above::new(view.root_path(), q.above);
    let last = view.node(0).last_id;
    let mut open: Vec<Open> = Vec::new();
    let mut id = 0u32;
    loop {
        // Settle the directories `id` is past: judge each once its own
        // files are read, close each once its subtree is.
        while let Some((top, outer)) = open.split_last_mut() {
            if !top.judged && id > top.files_end {
                top.judged = true;
                if let Some(cache) = judge(top, outer, q, &mut above) {
                    found.push(cache);
                    // A cache inside it is part of it.
                    id = id.max(top.last.saturating_add(1));
                    open.pop();
                }
            } else if id > top.last {
                open.pop();
            } else {
                break;
            }
        }
        if id > last {
            break;
        }
        let node = view.node(id);
        let name = view.name(id);
        if node.is_dir() {
            let path = match open.last() {
                Some(parent) => {
                    let mut path = parent.path.clone();
                    if path.last() != Some(&b'/') {
                        path.push(b'/');
                    }
                    path.extend_from_slice(&name);
                    path
                }
                None => view.root_path().as_os_str().as_bytes().to_vec(),
            };
            open.push(Open {
                id,
                files_end: node.files_end,
                last: node.last_id,
                path,
                markers: Vec::new(),
                tag: false,
                judged: false,
            });
        } else if let Some(parent) = open.last_mut() {
            if *name == *TAG {
                parent.tag = true;
            }
            if marker_names.contains(&&*name) && !parent.markers.iter().any(|m| *m == *name) {
                parent.markers.push(name.into_owned());
            }
        }
        id += 1;
    }
    found
}

/// The directories above the root, and which markers each holds, asked
/// once each.
struct Above<'a> {
    /// The root's ancestors, by their number of components: `dirs[k]` has
    /// `k`; the root has `dirs.len()`.
    dirs: Vec<&'a Path>,
    holds: &'a dyn Fn(&Path, &str) -> bool,
    asked: BTreeMap<(usize, String), bool>,
}

impl<'a> Above<'a> {
    fn new(root: &'a Path, holds: &'a dyn Fn(&Path, &str) -> bool) -> Above<'a> {
        let mut dirs: Vec<&Path> = root.ancestors().skip(1).collect();
        dirs.reverse();
        Above {
            dirs,
            holds,
            asked: BTreeMap::new(),
        }
    }

    /// Whether the ancestor of the root with `k` components holds `marker`.
    fn holds(&mut self, k: usize, marker: &str) -> bool {
        let Some(dir) = self.dirs.get(k) else {
            return false;
        };
        match self.asked.entry((k, marker.to_owned())) {
            Entry::Occupied(e) => *e.get(),
            Entry::Vacant(e) => *e.insert((self.holds)(dir, marker)),
        }
    }
}

/// The cache `dir` is, if any, given the directories `outer` open above
/// it: the first rule that takes it; or, under `loose`, the first rule its
/// name matches.
fn judge(dir: &Open, outer: &[Open], q: &CachesQuery<'_>, above: &mut Above<'_>) -> Option<Found> {
    let components: Vec<&[u8]> = dir
        .path
        .split(|&b| b == b'/')
        .filter(|c| !c.is_empty())
        .collect();
    let holds_tag = dir.tag && duscope_walk::cachedir_tag(Path::new(OsStr::from_bytes(&dir.path)));
    // An ancestor with k components is open when k reaches the root's.
    let root_len = above.dirs.len();
    let holds = |k: usize, marker: &str| match k.checked_sub(root_len) {
        Some(j) => outer
            .get(j)
            .is_some_and(|o| o.markers.iter().any(|m| m == marker.as_bytes())),
        None => above.holds(k, marker),
    };
    if let Some((rule, at)) = q.caches.taking(&components, holds, holds_tag) {
        let project = at.map(|k| match k.checked_sub(root_len) {
            Some(j) => outer.get(j).map_or_else(Vec::new, |o| o.path.clone()),
            None => above
                .dirs
                .get(k)
                .map_or_else(Vec::new, |d| d.as_os_str().as_bytes().to_vec()),
        });
        return Some(Found {
            id: dir.id,
            rule: rule.name.clone().unwrap_or_else(|| TAG_RULE.to_owned()),
            regenerable_by: rule.regenerable_by.as_str(),
            project,
            confirmed: true,
            cachedir_tag: holds_tag,
        });
    }
    if !q.loose {
        return None;
    }
    let name = components.last().copied().unwrap_or_default();
    let rule = q.caches.named(name).next()?;
    Some(Found {
        id: dir.id,
        rule: rule.name.clone().unwrap_or_default(),
        regenerable_by: rule.regenerable_by.as_str(),
        project: None,
        confirmed: false,
        cachedir_tag: holds_tag,
    })
}

#[cfg(test)]
mod tests;
