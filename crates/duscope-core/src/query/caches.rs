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
//! The pass reads every directory, and of the files only those it looks
//! at: a `CACHEDIR.TAG` and the rules' markers, the names taken from the
//! same rules that judge. Of a saved scan it reads the two from their own
//! tables, each in id order, and merges them (`Index::dirs_in`,
//! `Index::files_named`), so that no other file's row is read whole; past
//! a confirmed cache, both reads start again after its subtree.
//!
//! Under `loose`, a name without its marker is a row, but the pass goes on
//! inside it: a confirmed cache inside it shows the name was not a cache
//! (a `build` directory of tools, one of them with its `node_modules`), and
//! the row gives way to what is inside. Otherwise what is inside is part of
//! it.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use globset::{Glob, GlobSet, GlobSetBuilder};

use super::{
    After, Filters, Page, SizeKind, SortKey, TreeView, failed, next_after, pct, ranked_after,
    remain, timegrammar::iso8601,
};
use crate::envelope::{CacheEntry, ProjectEntry, name_fields, path_fields};
use crate::index::reader::{DirRow, FileRow, Paged};
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
            let (path, path_raw) = path_fields(&f.path);
            let (project_root, project_root_raw) = project
                .as_deref()
                .map(path_fields)
                .map_or((None, None), |(p, raw)| (Some(p), raw));
            CacheEntry {
                path,
                path_bytes: f.path.clone(),
                name,
                name_raw,
                rule: f.rule.clone(),
                regenerable_by: f.regenerable_by,
                project_root,
                project_bytes: project.unwrap_or_default(),
                confirmed: f.confirmed,
                cachedir_tag: f.cachedir_tag,
                allocated_bytes: node.allocated,
                apparent_bytes: node.apparent,
                pct_of_root: pct(node.size(q.size), root_total),
                file_count: node.file_count,
                max_mtime: iso8601(node.max_mtime_ns),
                path_raw,
                project_root_raw,
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
        .map(|(project, group)| {
            let (project_root, project_root_raw) = project
                .as_deref()
                .map(path_fields)
                .map_or((None, None), |(p, raw)| (Some(p), raw));
            ProjectEntry {
                project_root,
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
                project_root_raw,
            }
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
    /// Where its name starts in that path.
    name_at: usize,
    /// The markers among its own files, by their index in the pass's list.
    markers: Vec<usize>,
    /// Whether its own files include a `CACHEDIR.TAG`.
    tag: bool,
    judged: bool,
    /// The unconfirmed row it is, or the nearest one above it.
    loose: Option<usize>,
}

impl Open {
    /// The directory `entry`, whose own files and subtree end at `ids`,
    /// entered from `parent`, the innermost directory open, its path put in
    /// the pass's path buffer `path`.
    fn enter(entry: &Item<'_>, ids: (u32, u32), parent: Option<&Open>, path: &mut Vec<u8>) -> Open {
        let mut name_at = path.len();
        if let Some(parent) = parent {
            path.truncate(parent.len);
            if path.last() != Some(&b'/') {
                path.push(b'/');
            }
            name_at = path.len();
            path.extend_from_slice(&entry.name);
        }
        Open {
            id: entry.id,
            files_end: ids.0,
            last: ids.1,
            len: path.len(),
            name_at,
            markers: Vec::new(),
            tag: false,
            judged: false,
            loose: parent.and_then(|o| o.loose),
        }
    }

    /// Notes that the directory holds a file `name`: the tag, or one of
    /// `markers`.
    fn holds(&mut self, name: &[u8], markers: &[&str]) {
        if name == TAG {
            self.tag = true;
        }
        if let Some(i) = markers.iter().position(|m| m.as_bytes() == name)
            && !self.markers.contains(&i)
        {
            self.markers.push(i);
        }
    }
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

/// The marker names of the rules, each once, in the rules' order.
fn rule_markers(caches: &Caches) -> Vec<&str> {
    let mut markers: Vec<&str> = Vec::new();
    for rule in caches.rules() {
        if let Confirm::Markers(ms) = &rule.confirm {
            for m in ms {
                if !markers.contains(&m.as_str()) {
                    markers.push(m);
                }
            }
        }
    }
    markers
}

/// The names of the only files the pass reads: the tag, and `markers`.
fn looked_for<'a>(markers: &[&'a str]) -> Vec<&'a [u8]> {
    let mut names: Vec<&[u8]> = vec![TAG];
    names.extend(markers.iter().map(|m| m.as_bytes()).filter(|&m| m != TAG));
    names
}

/// Every cache under the root, outermost only, in pre-order.
fn find(view: &(impl TreeView + ?Sized), q: &CachesQuery<'_>) -> Vec<Found> {
    let mut found: Vec<Found> = Vec::new();
    if view.is_empty() || !view.node(0).is_dir() {
        return found;
    }
    let markers = rule_markers(q.caches);
    let wanted = looked_for(&markers);
    let mut names = GlobSetBuilder::new();
    for rule in q.caches.rules() {
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
    let mut entries = Entries::new(view, last, &wanted);
    let mut next = entries.next();
    // The first id not passed: the entries before it are read, or inside a
    // cache passed over.
    let mut passed = 0;
    loop {
        // The id of the entry read next; past the last once none is left.
        let mut id = next
            .as_ref()
            .map_or(last.saturating_add(1), |e| e.id)
            .max(passed);
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
        passed = id;
        if id > last || failed(view) {
            break;
        }
        let Some(entry) = next.take() else {
            break;
        };
        if entry.id < id {
            // Inside a cache confirmed: read on from past it.
            entries.skip_to(id);
            next = entries.next();
            continue;
        }
        match entry.dir {
            Some(ids) => {
                let dir = Open::enter(&entry, ids, open.last(), &mut path);
                open.push(dir);
            }
            None => {
                if let Some(parent) = open.last_mut() {
                    parent.holds(&entry.name, &judge.markers);
                }
            }
        }
        next = entries.next();
    }
    standing(found)
}

/// An entry the pass reads: a directory, with the last ids of its own files
/// and of its subtree, or a file it looks for.
struct Item<'v> {
    id: u32,
    dir: Option<(u32, u32)>,
    name: Cow<'v, [u8]>,
}

/// The entries the pass reads, in id order: every directory, and the files
/// whose name is one of those it looks for. A file's only part in the pass
/// is to tell its directory that it holds a tag or a marker, so the other
/// files change nothing. A saved scan's entries are read from its two tables
/// apart and merged, so that the other files' rows are never read whole; the
/// arena's are every entry.
enum Entries<'v, V: TreeView + ?Sized> {
    /// Every id, its node and name, the files not looked for passed over.
    Every {
        view: &'v V,
        /// The id read next; `None` past the last.
        next: Option<u32>,
        last: u32,
        wanted: &'v [&'v [u8]],
    },
    /// A saved scan's directories and files looked for.
    Saved(Box<Saved<'v>>),
}

/// A saved scan's directories, and its files looked for, by id, each
/// table's next row held. The files are read no further than the
/// directories have been, so that the files of a cache passed over are not
/// read, as the walk does not read them.
struct Saved<'v> {
    /// The view's id 0 in the scan.
    base: u32,
    dirs: Paged<'v, DirRow>,
    files: Paged<'v, FileRow>,
    dir: Option<DirRow>,
    file: Option<FileRow>,
}

impl<'v, V: TreeView + ?Sized> Entries<'v, V> {
    /// The entries of `view` from its root to `last`, the files among them
    /// only those named in `wanted`.
    fn new(view: &'v V, last: u32, wanted: &'v [&'v [u8]]) -> Entries<'v, V> {
        if let Some((index, base)) = view.as_index() {
            let end = base.saturating_add(last);
            if let Some(files) = index.files_named(base, end, wanted) {
                let mut dirs = index.dirs_in(base, end);
                return Entries::Saved(Box::new(Saved {
                    base,
                    dir: dirs.next(),
                    file: None,
                    dirs,
                    files,
                }));
            }
        }
        Entries::Every {
            view,
            next: Some(0),
            last,
            wanted,
        }
    }

    /// The next entry, in id order.
    fn next(&mut self) -> Option<Item<'v>> {
        match self {
            Entries::Every {
                view,
                next,
                last,
                wanted,
            } => {
                while let Some(id) = next.filter(|id| id <= last) {
                    if failed(*view) {
                        break;
                    }
                    *next = id.checked_add(1);
                    let node = view.node(id);
                    let name = view.name(id);
                    if node.is_dir() {
                        let dir = Some((node.files_end, node.last_id));
                        return Some(Item { id, dir, name });
                    }
                    if wanted.contains(&&*name) {
                        let dir = None;
                        return Some(Item { id, dir, name });
                    }
                }
                *next = None;
                None
            }
            Entries::Saved(saved) => saved.next(),
        }
    }

    /// Passes over the entries before id `id`.
    fn skip_to(&mut self, id: u32) {
        match self {
            Entries::Every { next, .. } => *next = next.map(|n| n.max(id)),
            Entries::Saved(saved) => saved.skip_to(id),
        }
    }
}

impl<'v> Saved<'v> {
    /// The next entry, in the view's ids; a directory before a file of the
    /// same id, which only a damaged index holds.
    fn next(&mut self) -> Option<Item<'v>> {
        if self.file.is_none() {
            // Up to the last directory read; to the end once every one is.
            let upto = self
                .dir
                .as_ref()
                .map_or(u32::MAX, |d| self.dirs.reach().unwrap_or(d.id));
            self.file = self.files.next_upto(upto);
        }
        let take_dir = match (&self.dir, &self.file) {
            (Some(d), Some(f)) => d.id <= f.id,
            (Some(_), None) => true,
            (None, _) => false,
        };
        let base = self.base;
        if take_dir {
            let row = std::mem::replace(&mut self.dir, self.dirs.next())?;
            let ids = (row.files_end, row.last_id);
            Some(Item {
                id: row.id.saturating_sub(base),
                dir: Some((ids.0.saturating_sub(base), ids.1.saturating_sub(base))),
                name: Cow::Owned(row.name),
            })
        } else {
            let row = self.file.take()?;
            Some(Item {
                id: row.id.saturating_sub(base),
                dir: None,
                name: Cow::Owned(row.name),
            })
        }
    }

    /// Passes over the entries before the view's id `id`.
    fn skip_to(&mut self, id: u32) {
        let id = self.base.saturating_add(id);
        if self.dir.as_ref().is_some_and(|d| d.id < id) {
            self.dirs.skip_to(id);
            self.dir = self.dirs.next();
        }
        if self.file.as_ref().is_none_or(|f| f.id < id) {
            self.file = None;
            self.files.skip_to(id);
        }
    }
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
        // The root's name as the view gives it; any other's is the end of
        // its path.
        let name = if dir.id == 0 {
            view.name(0)
        } else {
            Cow::Borrowed(path.get(dir.name_at..).unwrap_or_default())
        };
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
