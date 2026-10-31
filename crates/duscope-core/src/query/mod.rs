//! The list verbs' queries, over any [`TreeView`]: the arena right after a
//! scan (phase 1) and the index (phase 2).
//!
//! A view numbers its entries in pre-order, children in canonical order (a
//! directory's files by name, then its subdirectories by name), so a
//! directory's subtree is the range `id ..= last_id` and its own files are
//! `id + 1 ..= files_end`. Each query reads what it needs and returns its
//! rows with the count before `--limit` ([`Page`]).

mod by_type;
pub mod resolve;
mod stale;
pub mod timegrammar;
mod top;
mod tree;

/// The clock every "now" comes from: the scan's start and end, the
/// queries' ages and cutoffs.
pub mod clock {
    pub use super::timegrammar::now;
}

use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use duscope_walk::{Flags, Kind};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};

use crate::CoreError;
use crate::arena::{Arena, DirRec, EntryIdx};
use crate::rollup::RollUp;

pub use by_type::{ByTypeQuery, by_type, classify, root_dirs};
/// The regular expressions `--regex` takes, matched against path bytes.
pub use regex::bytes::Regex;
pub use resolve::{
    Resolved, ScanQuery, ScanSelector, index_files, resolve_scan, short_id, shortest_prefix,
};
pub use stale::{DEFAULT_MIN_SIZE, StaleQuery, stale};
pub use top::{TopQuery, top};
pub use tree::{TreeQuery, tree};

/// One entry of a scan, as the queries read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
// Independent facts of one entry, as the index stores them.
#[allow(clippy::struct_excessive_bools)]
pub struct Node {
    /// Its pre-order id; the root is 0.
    pub id: u32,
    /// Its parent's pre-order id; the root is its own parent.
    pub parent: u32,
    /// How far below the root it is; the root is 0.
    pub depth: u32,
    /// What it is.
    pub kind: Kind,
    /// Bytes on disk: a directory's subtree, hard links and clones counted
    /// once; a file's own, charged or not.
    pub allocated: u64,
    /// Apparent bytes, on the same terms.
    pub apparent: u64,
    /// Non-directories in the subtree; 1 for a file.
    pub file_count: u64,
    /// Directories in the subtree, the directory included; 0 for a file.
    pub dir_count: u64,
    /// Its own modification time, Unix nanoseconds.
    pub mtime_ns: i64,
    /// The newest modification time in its subtree; its own for a file.
    pub max_mtime_ns: i64,
    /// Its inode change time.
    pub ctime_ns: i64,
    /// Its access time, when `--attrs full` collected it.
    pub atime_ns: Option<i64>,
    /// Its creation time, when collected and the filesystem has one.
    pub btime_ns: Option<i64>,
    /// Its mode bits, when collected.
    pub mode: Option<u32>,
    /// A non-directory's link count.
    pub nlink: u32,
    /// Whether its bytes count in its ancestors' totals (a later hard link
    /// or clone does not).
    pub charged: bool,
    /// A later link of an inode: its apparent size is left out of its
    /// ancestors' totals too, where an uncharged clone's is not.
    pub linked_away: bool,
    /// Storage flags.
    pub flags: Flags,
    /// A directory whose totals may include shared blocks.
    pub upper_bound: bool,
    /// A directory that could not be read.
    pub unreadable: bool,
    /// A directory's last own file; the id itself for a file.
    pub files_end: u32,
    /// The last id of a directory's subtree; the id itself for a file.
    pub last_id: u32,
}

impl Node {
    /// Its size in `size`.
    pub fn size(&self, size: SizeKind) -> u64 {
        match size {
            SizeKind::Allocated => self.allocated,
            SizeKind::Apparent => self.apparent,
        }
    }

    /// What it adds to its parent's total in `size`: a directory's subtree;
    /// nothing for an uncharged file's allocated bytes, or a later link's
    /// apparent ones.
    pub fn counted(&self, size: SizeKind) -> u64 {
        match size {
            _ if self.is_dir() => self.size(size),
            SizeKind::Allocated if !self.charged => 0,
            SizeKind::Apparent if self.linked_away => 0,
            _ => self.size(size),
        }
    }

    /// Whether it is a directory.
    pub fn is_dir(&self) -> bool {
        self.kind == Kind::Dir
    }
}

/// A scan the queries can read.
///
/// [`TreeView::node`] answers an entry's scalar columns and
/// [`TreeView::name`] its name. Paths and a directory's subdirectories have
/// methods of their own, which the queries call where they need them: their
/// defaults read them through `node` and `name`, which suits the arena, and
/// a view that does not keep every entry in memory (the index) answers each
/// from its own storage in one step.
pub trait TreeView {
    /// The number of entries.
    fn len(&self) -> u32;
    /// Whether the scan holds no entries.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// The entry with pre-order id `id`.
    fn node(&self, id: u32) -> Node;
    /// The root's path, absolute.
    fn root_path(&self) -> &Path;
    /// Whether `--attrs full` was collected (atime, btime, mode).
    fn full_attrs(&self) -> bool;

    /// The name of entry `id`, as bytes.
    fn name(&self, id: u32) -> Cow<'_, [u8]>;

    /// The path of `id` relative to the root, `/`-separated; empty for the
    /// root.
    fn path(&self, id: u32) -> Vec<u8> {
        let mut names = Vec::new();
        let mut at = id;
        while at != 0 {
            names.push(self.name(at));
            at = self.node(at).parent;
        }
        let mut path = Vec::new();
        for name in names.iter().rev() {
            if !path.is_empty() {
                path.push(b'/');
            }
            path.extend_from_slice(name);
        }
        path
    }

    /// The subdirectories of `dir`, in canonical order: each one's id and
    /// the last id of its subtree. Empty for a file.
    fn child_dirs(&self, dir: u32) -> Vec<(u32, u32)> {
        let node = self.node(dir);
        let mut dirs = Vec::new();
        let mut next = node.files_end.checked_add(1);
        while let Some(id) = next.filter(|&id| id <= node.last_id) {
            let last = self.node(id).last_id;
            dirs.push((id, last));
            next = last.checked_add(1);
        }
        dirs
    }
}

/// The arena and its roll-up, as a [`TreeView`].
#[derive(Debug)]
pub struct ArenaView<'a> {
    arena: &'a Arena,
    rollup: &'a RollUp,
    root: PathBuf,
}

impl<'a> ArenaView<'a> {
    /// The view of a rolled-up arena whose root is at `root`.
    pub fn new(arena: &'a Arena, rollup: &'a RollUp, root: PathBuf) -> ArenaView<'a> {
        ArenaView {
            arena,
            rollup,
            root,
        }
    }
}

impl TreeView for ArenaView<'_> {
    fn len(&self) -> u32 {
        u32::try_from(self.rollup.len()).unwrap_or(u32::MAX)
    }

    fn node(&self, id: u32) -> Node {
        let idx = self.rollup.entry_at(id);
        let view = self.arena.entry(idx);
        let full = self.arena.full(idx);
        // The root has no parent directory (and a file root no directory
        // at all).
        let (parent, depth) = if id == 0 {
            (0, 0)
        } else {
            let parent_dir = self.arena.dir(view.parent);
            (
                self.rollup.id_of(EntryIdx::from_raw(parent_dir.entry)),
                u32::from(parent_dir.depth) + 1,
            )
        };
        let charges = self.rollup.charges();
        let mut node = Node {
            id,
            parent,
            depth,
            kind: view.kind,
            allocated: view.allocated,
            apparent: view.apparent,
            file_count: 1,
            dir_count: 0,
            mtime_ns: view.mtime_ns,
            max_mtime_ns: view.mtime_ns,
            ctime_ns: view.ctime_ns,
            atime_ns: full.map(|f| f.atime_ns),
            btime_ns: full.and_then(|f| f.btime_ns),
            mode: full.map(|f| f.mode),
            nlink: view.nlink,
            charged: charges.charged(idx),
            linked_away: charges.linked_away(idx),
            flags: view.flags,
            upper_bound: false,
            unreadable: false,
            files_end: id,
            last_id: id,
        };
        if let Some(dir) = view.dir {
            let rec = self.arena.dir(dir);
            node.depth = u32::from(rec.depth);
            node.allocated = rec.cum_allocated;
            node.apparent = rec.cum_apparent;
            node.file_count = u64::from(rec.n_files);
            node.dir_count = u64::from(rec.n_dirs) + 1;
            node.max_mtime_ns = rec.max_mtime_ns;
            node.upper_bound = rec.flags & DirRec::UPPER_BOUND != 0;
            node.unreadable = rec.flags & DirRec::UNREADABLE != 0;
            node.files_end = rec.files_end;
            node.last_id = rec.last_id;
        }
        node
    }

    fn root_path(&self) -> &Path {
        &self.root
    }

    fn full_attrs(&self) -> bool {
        self.arena.attrs() == duscope_walk::Attrs::Full
    }

    fn name(&self, id: u32) -> Cow<'_, [u8]> {
        Cow::Borrowed(self.arena.entry(self.rollup.entry_at(id)).name)
    }
}

/// The subtree of a view at one of its directories, as a view of its own:
/// that directory is id 0 at depth 0 and its path is the root path, so a
/// query reads it as a scan of that directory alone. A verb asked about a
/// path inside a saved scan's root reads the scan this way.
#[derive(Debug)]
pub struct Subtree<'v, V: TreeView + ?Sized> {
    view: &'v V,
    base: u32,
    last: u32,
    depth: u32,
    /// The length of the base's path relative to the view's root, and its
    /// `/`: what the base's descendants' paths start with.
    prefix: usize,
    root: PathBuf,
}

impl<'v, V: TreeView + ?Sized> Subtree<'v, V> {
    /// The subtree of `view` at entry `base`.
    pub fn new(view: &'v V, base: u32) -> Subtree<'v, V> {
        let node = view.node(base);
        let rel = view.path(base);
        let mut root = view.root_path().to_path_buf();
        if !rel.is_empty() {
            root.push(std::ffi::OsStr::from_bytes(&rel));
        }
        Subtree {
            view,
            base,
            last: node.last_id,
            depth: node.depth,
            prefix: if rel.is_empty() { 0 } else { rel.len() + 1 },
            root,
        }
    }
}

impl<V: TreeView + ?Sized> TreeView for Subtree<'_, V> {
    fn len(&self) -> u32 {
        self.last - self.base + 1
    }

    fn node(&self, id: u32) -> Node {
        // Saturating: an entry an index could not read is empty, at depth 0
        // under the root, and the verb reports the failed read.
        let mut node = self.view.node(id + self.base);
        node.id = id;
        node.parent = if id == 0 {
            0
        } else {
            node.parent.saturating_sub(self.base)
        };
        node.depth = node.depth.saturating_sub(self.depth);
        node.files_end = node.files_end.saturating_sub(self.base);
        node.last_id = node.last_id.saturating_sub(self.base);
        node
    }

    fn root_path(&self) -> &Path {
        &self.root
    }

    fn full_attrs(&self) -> bool {
        self.view.full_attrs()
    }

    fn name(&self, id: u32) -> Cow<'_, [u8]> {
        self.view.name(id + self.base)
    }

    fn path(&self, id: u32) -> Vec<u8> {
        if id == 0 {
            return Vec::new();
        }
        let mut path = self.view.path(id + self.base);
        path.drain(..self.prefix.min(path.len()));
        path
    }

    fn child_dirs(&self, dir: u32) -> Vec<(u32, u32)> {
        self.view
            .child_dirs(dir + self.base)
            .into_iter()
            .map(|(id, last)| (id.saturating_sub(self.base), last.saturating_sub(self.base)))
            .collect()
    }
}

/// The size the rows are ranked and shown by.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SizeKind {
    /// Bytes on disk.
    #[default]
    Allocated,
    /// Apparent length.
    Apparent,
}

impl SizeKind {
    /// As the envelope spells it.
    pub fn as_str(self) -> &'static str {
        match self {
            SizeKind::Allocated => "allocated",
            SizeKind::Apparent => "apparent",
        }
    }
}

/// The ids of `dir`'s children: its files, then the root of each
/// subdirectory's subtree ([`TreeView::child_dirs`]).
pub fn children<'v>(
    view: &'v (impl TreeView + ?Sized),
    dir: &Node,
) -> impl Iterator<Item = u32> + 'v {
    let files = (dir.id + 1)..=dir.files_end;
    let dirs = if dir.is_dir() {
        view.child_dirs(dir.id)
    } else {
        Vec::new()
    };
    files.chain(dirs.into_iter().map(|(id, _)| id))
}

/// The path of `id` relative to the root, `/`-separated; empty for the
/// root ([`TreeView::path`]).
pub fn rel_path(view: &(impl TreeView + ?Sized), id: u32) -> Vec<u8> {
    view.path(id)
}

/// The absolute path of `id`, as bytes.
pub fn abs_path(view: &(impl TreeView + ?Sized), id: u32) -> Vec<u8> {
    let mut path = view.root_path().as_os_str().as_bytes().to_vec();
    let rel = rel_path(view, id);
    if !rel.is_empty() {
        if path.last() != Some(&b'/') {
            path.push(b'/');
        }
        path.extend_from_slice(&rel);
    }
    path
}

/// `part` as a percentage of `whole`, with one decimal.
pub fn pct(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        return 0.0;
    }
    let tenths = (u128::from(part) * 1000 + u128::from(whole) / 2) / u128::from(whole);
    f64::from(u32::try_from(tenths).unwrap_or(u32::MAX)) / 10.0
}

/// The storage and directory flags a row names.
pub fn flag_names(node: &Node) -> Vec<&'static str> {
    const NAMES: [(Flags, &str); 8] = [
        (Flags::SPARSE, "sparse"),
        (Flags::COMPRESSED, "compressed"),
        (Flags::DATALESS, "dataless"),
        (Flags::PURGEABLE, "purgeable"),
        (Flags::MAY_SHARE, "may_share"),
        (Flags::SHARES_ALL, "shares_all"),
        (Flags::MOUNT_ROOT, "mount_root"),
        (Flags::SUBVOLUME, "subvolume"),
    ];
    let mut names: Vec<&'static str> = NAMES
        .iter()
        .filter(|(flag, _)| node.flags.contains(*flag))
        .map(|&(_, name)| name)
        .collect();
    if node.upper_bound {
        names.push("upper_bound");
    }
    if node.unreadable {
        names.push("unreadable");
    }
    if !node.is_dir() && !node.charged {
        names.push("uncharged");
    }
    names
}

/// A kind as rows and `--type` name it.
pub fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Dir => "dir",
        Kind::File => "file",
        Kind::Symlink => "symlink",
        _ => "other",
    }
}

/// Which time a filter reads.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TimeField {
    /// Modification; a directory's is the newest in its subtree.
    #[default]
    Mtime,
    /// Access (`--attrs full`).
    Atime,
    /// Inode change.
    Ctime,
    /// Creation (`--attrs full`).
    Btime,
}

impl TimeField {
    /// The field's value for `node`; `None` when not collected.
    pub fn of(self, node: &Node) -> Option<i64> {
        match self {
            TimeField::Mtime => Some(node.max_mtime_ns),
            TimeField::Atime => node.atime_ns,
            TimeField::Ctime => Some(node.ctime_ns),
            TimeField::Btime => node.btime_ns,
        }
    }

    /// Whether only `--attrs full` collects it.
    pub fn needs_full(self) -> bool {
        matches!(self, TimeField::Atime | TimeField::Btime)
    }
}

/// The filters every list verb shares, applied to its rows after the
/// index is read.
#[derive(Debug, Clone, Default)]
pub struct Filters {
    /// `--glob`: rows whose path relative to the root matches.
    pub glob: Option<GlobSet>,
    /// `--regex`: rows whose path relative to the root matches.
    pub regex: Option<Regex>,
    /// `--invert`: the rows `--glob` and `--regex` would leave out instead.
    pub invert: bool,
    /// `--exclude`: rows whose path relative to the root, and the paths of
    /// whose ancestors, do not match.
    pub exclude: Option<GlobSet>,
    /// `--type`: the kinds kept, by [`kind_name`]; empty keeps all.
    pub kinds: Vec<&'static str>,
    /// `--min-size`, in the size kind of the query.
    pub min_size: Option<u64>,
    /// `--max-size`.
    pub max_size: Option<u64>,
    /// `--min-depth`.
    pub min_depth: Option<u32>,
    /// `--max-depth`, when it filters rather than lists.
    pub max_depth: Option<u32>,
    /// `--time`.
    pub time: TimeField,
    /// Rows whose time is before this, Unix nanoseconds (`--older-than`,
    /// `--until`).
    pub before: Option<i64>,
    /// Rows whose time is at or after this (`--newer-than`, `--since`).
    pub after: Option<i64>,
    /// The filters as the command line gave them, for `query.filters`.
    pub given: Vec<String>,
}

impl Filters {
    /// Whether a time filter asks for a time `--attrs basic` does not
    /// collect.
    pub fn needs_full(&self) -> bool {
        (self.before.is_some() || self.after.is_some()) && self.time.needs_full()
    }

    /// Whether `node` is kept, its size read in `size`.
    pub fn keeps(&self, view: &(impl TreeView + ?Sized), node: &Node, size: SizeKind) -> bool {
        if !self.kinds.is_empty() && !self.kinds.contains(&kind_name(node.kind)) {
            return false;
        }
        let bytes = node.size(size);
        if self.min_size.is_some_and(|min| bytes < min)
            || self.max_size.is_some_and(|max| bytes > max)
        {
            return false;
        }
        if self.min_depth.is_some_and(|min| node.depth < min)
            || self.max_depth.is_some_and(|max| node.depth > max)
        {
            return false;
        }
        if self.before.is_some() || self.after.is_some() {
            let Some(time) = self.time.of(node) else {
                return false;
            };
            if self.before.is_some_and(|t| time >= t) || self.after.is_some_and(|t| time < t) {
                return false;
            }
        }
        if self.glob.is_none() && self.regex.is_none() && self.exclude.is_none() {
            return true;
        }
        let rel = rel_path(view, node.id);
        if self.excluded(&rel) {
            return false;
        }
        let path = Path::new(std::ffi::OsStr::from_bytes(&rel));
        let matched = self.glob.as_ref().is_none_or(|set| set.is_match(path))
            && self.regex.as_ref().is_none_or(|re| re.is_match(&rel));
        matched != (self.invert && (self.glob.is_some() || self.regex.is_some()))
    }

    /// Whether `--exclude` leaves out `node`: it, or a directory above it,
    /// matches.
    pub fn excludes(&self, view: &(impl TreeView + ?Sized), node: &Node) -> bool {
        self.exclude.is_some() && self.excluded(&rel_path(view, node.id))
    }

    /// Whether `--exclude` matches `rel`, or a directory above it.
    fn excluded(&self, rel: &[u8]) -> bool {
        let Some(set) = &self.exclude else {
            return false;
        };
        let mut ancestors = rel
            .iter()
            .enumerate()
            .filter(|&(_, &b)| b == b'/')
            .map(|(i, _)| Path::new(std::ffi::OsStr::from_bytes(&rel[..i])));
        set.is_match(Path::new(std::ffi::OsStr::from_bytes(rel)))
            || ancestors.any(|a| set.is_match(a))
    }
}

/// Globs as `--glob` and `--exclude` take them, matched against paths
/// relative to the root: a pattern starting with `/` or `./` is anchored at
/// the root, any other matches at every depth; `*` stays within a component
/// and `**` crosses them; a trailing `/` is dropped. A pattern naming no
/// path below the root (empty, `/` or `.`) is refused.
pub fn globs<'a>(patterns: impl IntoIterator<Item = &'a str>) -> Result<GlobSet, String> {
    let mut set = GlobSetBuilder::new();
    for pattern in patterns {
        let trimmed = pattern.trim_end_matches('/');
        let (anchored, rest) = match trimmed.strip_prefix("./") {
            Some(rest) => (true, rest),
            None => (trimmed.starts_with('/'), trimmed),
        };
        let rest = rest.trim_start_matches('/');
        if rest.is_empty() || rest == "." {
            return Err(format!(
                "{pattern:?}: the pattern names no path below the root"
            ));
        }
        let glob = if anchored {
            rest.to_owned()
        } else {
            format!("**/{rest}")
        };
        set.add(
            GlobBuilder::new(&glob)
                .literal_separator(true)
                .build()
                .map_err(|e| format!("{pattern:?}: {e}"))?,
        );
    }
    set.build().map_err(|e| e.to_string())
}

/// `--sort`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SortKey {
    /// Largest first.
    #[default]
    Size,
    /// Most files first.
    Count,
    /// Newest first.
    Mtime,
    /// Bytewise by path.
    Path,
}

impl SortKey {
    /// As `query.sort` spells it, in `size`, reversed with `-r`.
    pub fn describe(self, size: SizeKind, reverse: bool) -> String {
        let (key, desc) = match self {
            SortKey::Size => (size.as_str(), true),
            SortKey::Count => ("count", true),
            SortKey::Mtime => ("mtime", true),
            SortKey::Path => ("path", false),
        };
        let desc = desc != reverse;
        format!("{key}_{}", if desc { "desc" } else { "asc" })
    }
}

/// Sorts `ids` by `key`, path bytewise among equals; `reverse` reverses
/// the key, and equals stay in path order (except under [`SortKey::Path`],
/// which it reverses whole).
pub fn sort_ids(
    view: &(impl TreeView + ?Sized),
    ids: &mut [u32],
    key: SortKey,
    size: SizeKind,
    reverse: bool,
) {
    sort_ids_by(view, ids, key, reverse, |node| node.size(size));
}

/// [`sort_ids`], with `size_of` giving the size [`SortKey::Size`] reads.
pub fn sort_ids_by(
    view: &(impl TreeView + ?Sized),
    ids: &mut [u32],
    key: SortKey,
    reverse: bool,
    size_of: impl Fn(&Node) -> u64,
) {
    let mut keyed: Vec<(i128, Vec<u8>, u32)> = ids
        .iter()
        .map(|&id| {
            let value = sort_value(&view.node(id), key, reverse, &size_of);
            (value, rel_path(view, id), id)
        })
        .collect();
    let paths_reversed = reverse && key == SortKey::Path;
    keyed.sort_by(|a, b| {
        let by_path = if paths_reversed {
            b.1.cmp(&a.1)
        } else {
            a.1.cmp(&b.1)
        };
        a.0.cmp(&b.0).then(by_path)
    });
    for (slot, (_, _, id)) in ids.iter_mut().zip(keyed) {
        *slot = id;
    }
}

/// Where `node` sorts under `key`, smallest first; paths break ties.
fn sort_value(node: &Node, key: SortKey, reverse: bool, size_of: &impl Fn(&Node) -> u64) -> i128 {
    let value = match key {
        SortKey::Size => -i128::from(size_of(node)),
        SortKey::Count => -i128::from(node.file_count),
        SortKey::Mtime => -i128::from(node.max_mtime_ns),
        SortKey::Path => 0,
    };
    if reverse { -value } else { value }
}

/// The first `limit` of `candidates` in [`sort_ids`] order (all of them
/// without a limit), and how many candidates there were.
pub fn ranked(
    view: &(impl TreeView + ?Sized),
    candidates: impl IntoIterator<Item = u32>,
    key: SortKey,
    size: SizeKind,
    reverse: bool,
    limit: Option<usize>,
) -> (Vec<u32>, u64) {
    ranked_by(view, candidates, key, reverse, limit, |node| {
        node.size(size)
    })
}

/// [`ranked`], with `size_of` giving the size [`SortKey::Size`] reads.
///
/// With a limit, the candidates are ranked as they are counted: a heap
/// keeps the `limit` best sort keys and a list the candidates that tie
/// with the worst of them, so only those are sorted, and their paths
/// (which decide among equal keys) spelled. Memory stays near the limit,
/// not the number of candidates, unless the keys tie (`--sort path`, which
/// sorts them all).
pub fn ranked_by(
    view: &(impl TreeView + ?Sized),
    candidates: impl IntoIterator<Item = u32>,
    key: SortKey,
    reverse: bool,
    limit: Option<usize>,
    size_of: impl Fn(&Node) -> u64,
) -> (Vec<u32>, u64) {
    let Some(limit) = limit else {
        let mut ids: Vec<u32> = candidates.into_iter().collect();
        sort_ids_by(view, &mut ids, key, reverse, size_of);
        let total = ids.len() as u64;
        return (ids, total);
    };
    let mut best: BinaryHeap<(i128, u32)> = BinaryHeap::with_capacity(limit.saturating_add(1));
    let mut ties: Vec<u32> = Vec::new();
    let mut total = 0u64;
    for id in candidates {
        total += 1;
        if limit == 0 {
            continue;
        }
        let value = sort_value(&view.node(id), key, reverse, &size_of);
        let Some(&(worst, _)) = best.peek().filter(|_| best.len() >= limit) else {
            best.push((value, id));
            continue;
        };
        match value.cmp(&worst) {
            Ordering::Greater => {}
            Ordering::Equal => ties.push(id),
            Ordering::Less => {
                best.push((value, id));
                if let Some((_, out)) = best.pop() {
                    // `out` ties with the worst kept key, or the worst kept
                    // key is now better than every tie.
                    if best.peek().is_some_and(|&(kept, _)| kept == worst) {
                        ties.push(out);
                    } else {
                        ties.clear();
                    }
                }
            }
        }
    }
    let mut ids: Vec<u32> = best.into_iter().map(|(_, id)| id).chain(ties).collect();
    sort_ids_by(view, &mut ids, key, reverse, size_of);
    ids.truncate(limit);
    (ids, total)
}

/// A page of rows: what `--limit` kept, and how many matched.
#[derive(Debug, Clone, PartialEq)]
pub struct Page<T> {
    /// The rows.
    pub entries: Vec<T>,
    /// Rows matched before `--limit`.
    pub total_count: u64,
}

/// What a query was asked for that the scan did not collect.
pub(crate) fn attrs_not_collected(what: &'static str) -> CoreError {
    CoreError::AttrsNotCollected(what)
}

#[cfg(test)]
mod tests;
