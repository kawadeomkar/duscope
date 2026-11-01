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
//!
//! Under `loose`, a name without its marker is a row, but the pass goes on
//! inside it: a confirmed cache inside it shows the name was not a cache
//! (a `build` directory of tools, one of them with its `node_modules`), and
//! the row gives way to what is inside. Otherwise what is inside is part of
//! it.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use globset::{Glob, GlobSet, GlobSetBuilder};

use super::{
    After, Filters, Page, SizeKind, SortKey, TreeView, next_after, pct, ranked_after, remain,
    timegrammar::iso8601,
};
use crate::envelope::{CacheEntry, ProjectEntry, name_fields};
use crate::rules::{Caches, Confirm};

/// `caches`' default `--min-size`: 10 MiB.
pub const DEFAULT_MIN_SIZE: u64 = 10 << 20;

/// The file a valid tag is held in.
const TAG: &[u8] = b"CACHEDIR.TAG";

/// The label of the rule a valid `CACHEDIR.TAG` confirms.
const TAG_RULE: &str = "CACHEDIR.TAG";

/// The caches a `--by-project` row names, largest first; `cache_count`
/// counts them all.
pub const PROJECT_CACHES: usize = 10;

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
    /// `--cursor`: the page after this cache (each cache, not by project).
    pub after: Option<After>,
}

impl std::fmt::Debug for CachesQuery<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CachesQuery")
            .field("loose", &self.loose)
            .field("min_size", &self.min_size)
            .finish_non_exhaustive()
    }
}

/// A cache found: the rule that names it, and its project.
#[derive(Debug, Clone)]
struct Found {
    id: u32,
    /// Its absolute path.
    path: Vec<u8>,
    rule: String,
    regenerable_by: &'static str,
    project: Option<Project>,
    confirmed: bool,
    cachedir_tag: bool,
    /// The nearest unconfirmed row above it, under `loose`.
    loose_parent: Option<usize>,
    /// An unconfirmed row with a confirmed cache inside it.
    dropped: bool,
}

/// The directory holding a cache's marker.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Project {
    /// Its absolute path.
    path: Vec<u8>,
    /// Its id, when the view holds it.
    id: Option<u32>,
}

/// The cache directories under the root of `view`, largest first.
pub fn caches(view: &(impl TreeView + ?Sized), q: &CachesQuery<'_>) -> Page<CacheEntry> {
    let found = find(view, q);
    let by_id: BTreeMap<u32, &Found> = found.iter().map(|f| (f.id, f)).collect();
    let candidates = found.iter().map(|f| f.id).filter(|&id| {
        let node = view.node(id);
        node.size(q.size) >= q.min_size && q.filters.keeps(view, &node, q.size)
    });
    let after = q.after.as_ref();
    let seen = after.map_or(0, |a| a.seen);
    let size_of = |node: &super::Node| node.size(q.size);
    let (ids, total_count) =
        ranked_after(view, candidates, q.sort, q.reverse, q.limit, size_of, after);
    let next = next_after(view, &ids, q.sort, q.size, seen, total_count);
    let root_total = if view.is_empty() {
        0
    } else {
        view.node(0).size(q.size)
    };
    let entries: Vec<_> = ids
        .into_iter()
        .filter_map(|id| by_id.get(&id).copied())
        .map(|f| {
            let node = view.node(f.id);
            let (name, name_raw) = name_fields(&view.name(f.id));
            let project = f.project.as_ref().map(|p| p.path.clone());
            CacheEntry {
                path: String::from_utf8_lossy(&f.path).into_owned(),
                path_bytes: f.path.clone(),
                name,
                name_raw,
                rule: f.rule.clone(),
                regenerable_by: f.regenerable_by,
                project_root: project
                    .as_ref()
                    .map(|p| String::from_utf8_lossy(p).into_owned()),
                project_bytes: project.unwrap_or_default(),
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
        truncated: remain(seen, entries.len(), total_count),
        entries,
        total_count,
        next,
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
    let mut groups: BTreeMap<Option<Project>, Group> = BTreeMap::new();
    for f in found {
        let node = view.node(f.id);
        if !q.filters.keeps(view, &node, q.size) {
            continue;
        }
        let group = groups.entry(f.project).or_insert(Group {
            allocated: 0,
            apparent: 0,
            newest_ns: i64::MIN,
            caches: Vec::new(),
        });
        group.allocated += node.allocated;
        group.apparent += node.apparent;
        group.newest_ns = group.newest_ns.max(node.max_mtime_ns);
        group.caches.push((node.size(q.size), f.path));
    }
    let size = |g: &Group| match q.size {
        SizeKind::Allocated => g.allocated,
        SizeKind::Apparent => g.apparent,
    };
    let mut rows: Vec<(Option<Vec<u8>>, Group)> = groups
        .into_iter()
        .map(|(project, mut group)| {
            // The project's newest time is recursive over the project
            // where the view holds it, else over its caches.
            if let Some(id) = project.as_ref().and_then(|p| p.id) {
                group.newest_ns = group.newest_ns.max(view.node(id).max_mtime_ns);
            }
            group
                .caches
                .sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
            (project.map(|p| p.path), group)
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
    let entries: Vec<_> = rows
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
                .take(PROJECT_CACHES)
                .map(|(_, p)| String::from_utf8_lossy(&p).into_owned())
                .collect(),
        })
        .collect();
    Page {
        truncated: remain(0, entries.len(), total_count),
        entries,
        total_count,
        next: None,
    }
}

/// A directory open above the entry being read.
struct Open {
    id: u32,
    files_end: u32,
    last: u32,
    /// Its path's length in the pass's path buffer.
    len: usize,
    /// The markers among its own files, by their index in the pass's list.
    markers: Vec<usize>,
    /// Whether its own files include a `CACHEDIR.TAG`.
    tag: bool,
    judged: bool,
    /// The unconfirmed row it is, or the nearest one above it.
    loose: Option<usize>,
}

/// What every directory of the pass is judged with.
struct Judge<'a> {
    q: &'a CachesQuery<'a>,
    /// The rules' names: a directory none matches is judged only for a
    /// `CACHEDIR.TAG`.
    names: Option<GlobSet>,
    /// The marker names of the rules.
    markers: Vec<&'a str>,
    above: Above<'a>,
}

/// Every cache under the root, outermost only, in pre-order.
fn find(view: &(impl TreeView + ?Sized), q: &CachesQuery<'_>) -> Vec<Found> {
    let mut found: Vec<Found> = Vec::new();
    if view.is_empty() || !view.node(0).is_dir() {
        return found;
    }
    let mut markers: Vec<&str> = Vec::new();
    let mut names = GlobSetBuilder::new();
    for rule in q.caches.rules() {
        if let Confirm::Markers(ms) = &rule.confirm {
            for m in ms {
                if !markers.contains(&m.as_str()) {
                    markers.push(m);
                }
            }
        }
        if let Some(glob) = rule.name.as_deref().and_then(|n| Glob::new(n).ok()) {
            names.add(glob);
        }
    }
    let mut judge = Judge {
        q,
        // Without the set, every directory is judged.
        names: names.build().ok(),
        markers,
        above: Above::new(view.root_path(), q.above),
    };
    let last = view.node(0).last_id;
    let mut path: Vec<u8> = view.root_path().as_os_str().as_bytes().to_vec();
    let mut open: Vec<Open> = Vec::new();
    let mut id = 0u32;
    loop {
        // Settle the directories `id` is past: judge each once its own
        // files are read, close each once its subtree is. An unjudged
        // directory is the innermost one open, and the path buffer holds
        // its path.
        while let Some((top, outer)) = open.split_last_mut() {
            if !top.judged && id > top.files_end {
                top.judged = true;
                path.truncate(top.len);
                let Some(cache) = judge.judge(view, top, outer, &path) else {
                    continue;
                };
                if cache.confirmed {
                    // A confirmed cache: the unconfirmed rows above it were
                    // not caches, and what is inside it is part of it.
                    let mut at = top.loose;
                    while let Some(i) = at {
                        found[i].dropped = true;
                        at = found[i].loose_parent;
                    }
                    found.push(cache);
                    id = id.max(top.last.saturating_add(1));
                    open.pop();
                } else {
                    top.loose = Some(found.len());
                    found.push(cache);
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
            if let Some(parent) = open.last() {
                path.truncate(parent.len);
                if path.last() != Some(&b'/') {
                    path.push(b'/');
                }
                path.extend_from_slice(&name);
            }
            open.push(Open {
                id,
                files_end: node.files_end,
                last: node.last_id,
                len: path.len(),
                markers: Vec::new(),
                tag: false,
                judged: false,
                loose: open.last().and_then(|o| o.loose),
            });
        } else if let Some(parent) = open.last_mut() {
            if *name == *TAG {
                parent.tag = true;
            }
            if let Some(i) = judge.markers.iter().position(|m| m.as_bytes() == &*name)
                && !parent.markers.contains(&i)
            {
                parent.markers.push(i);
            }
        }
        id += 1;
    }
    standing(found)
}

/// The rows that stand: every confirmed cache, and each unconfirmed row
/// that no confirmed cache inside it set aside, unless it is inside
/// another such row, of which it is part.
fn standing(found: Vec<Found>) -> Vec<Found> {
    let stands = |f: &Found| f.confirmed || !f.dropped;
    let kept: Vec<bool> = found
        .iter()
        .map(|f| {
            let mut at = f.loose_parent;
            let mut inside = false;
            while let Some(p) = at {
                inside |= stands(&found[p]);
                at = found[p].loose_parent;
            }
            stands(f) && (f.confirmed || !inside)
        })
        .collect();
    found
        .into_iter()
        .zip(kept)
        .filter_map(|(f, keep)| keep.then_some(f))
        .collect()
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

impl Judge<'_> {
    /// The cache `dir` is, if any, given the directories `outer` open above
    /// it and its path: the rule that takes it; or, under `loose` and
    /// outside another unconfirmed row's judging, the first rule its name
    /// matches.
    fn judge(
        &mut self,
        view: &(impl TreeView + ?Sized),
        dir: &Open,
        outer: &[Open],
        path: &[u8],
    ) -> Option<Found> {
        let name = view.name(dir.id);
        let named = self
            .names
            .as_ref()
            .is_none_or(|set| set.is_match(String::from_utf8_lossy(&name).as_ref()));
        if !named && !dir.tag {
            return None;
        }
        let holds_tag = dir.tag && duscope_walk::cachedir_tag(Path::new(OsStr::from_bytes(path)));
        let components: Vec<&[u8]> = path
            .split(|&b| b == b'/')
            .filter(|c| !c.is_empty())
            .collect();
        // An ancestor with k components is open when k reaches the root's.
        let root_len = self.above.dirs.len();
        let markers = &self.markers;
        let above = &mut self.above;
        let holds = |k: usize, marker: &str| match k.checked_sub(root_len) {
            Some(j) => outer
                .get(j)
                .is_some_and(|o| o.markers.iter().any(|&i| markers.get(i) == Some(&marker))),
            None => above.holds(k, marker),
        };
        let found = |rule: &crate::rules::CacheRule, project, confirmed| Found {
            id: dir.id,
            path: path.to_vec(),
            rule: rule.name.clone().unwrap_or_else(|| TAG_RULE.to_owned()),
            regenerable_by: rule.regenerable_by.as_str(),
            project,
            confirmed,
            cachedir_tag: holds_tag,
            loose_parent: outer.last().and_then(|o| o.loose),
            dropped: false,
        };
        if let Some((rule, at)) = self.q.caches.taking(&components, holds, holds_tag) {
            let project = at.map(|k| match k.checked_sub(root_len) {
                Some(j) => Project {
                    path: outer
                        .get(j)
                        .map_or_else(Vec::new, |o| path[..o.len].to_vec()),
                    id: outer.get(j).map(|o| o.id),
                },
                None => Project {
                    path: self
                        .above
                        .dirs
                        .get(k)
                        .map_or_else(Vec::new, |d| d.as_os_str().as_bytes().to_vec()),
                    id: None,
                },
            });
            return Some(found(rule, project, true));
        }
        if !self.q.loose || !named {
            return None;
        }
        let rule = self
            .q
            .caches
            .named(components.last().copied().unwrap_or_default())
            .next()?;
        Some(found(rule, None, false))
    }
}

#[cfg(test)]
mod tests;
