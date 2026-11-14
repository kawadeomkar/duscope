//! The list verbs' queries, over any [`TreeView`]: the arena right after a
//! scan (phase 1) and the index (phase 2).
//!
//! A view numbers its entries in pre-order, children in canonical order (a
//! directory's files by name, then its subdirectories by name), so a
//! directory's subtree is the range `id ..= last_id` and its own files are
//! `id + 1 ..= files_end`. Each query reads what it needs and returns its
//! rows with the count before `--limit` ([`Page`]).

mod by_type;
pub mod caches;
pub mod cursor;
pub mod diff;
pub mod explain;
pub mod plan;
pub mod resolve;
pub mod sql;
mod stale;
pub mod summary;
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
use crate::index::Index;
use crate::rollup::RollUp;

pub use by_type::{ByTypeQuery, by_type, classify, root_dirs};
pub use caches::{CachesQuery, caches, projects};
pub use cursor::{After, Cursor, query_hash};
pub use diff::{DEFAULT_MIN_DELTA, DiffQuery, diff};
pub use explain::{
    ExplainQuery, explain, explain_unindexed, fs_holds_file, holds_file, shared_extents,
};
pub use plan::{Cut, Plan, Ranked};
/// The regular expressions `--regex` takes, matched against path bytes.
pub use regex::bytes::Regex;
pub use resolve::{
    Resolved, ScanQuery, ScanSelector, index_files, resolve_scan, short_id, shortest_prefix, under,
};
pub use sql::{SqlQuery, human_size, sql};
pub use stale::{DEFAULT_MIN_SIZE, StaleQuery, StaleRows, stale, stale_rows};
pub use summary::{
    SubvolumeBytes, SummaryBytes, VolumeBytes, VolumeRoot, VolumeUse, file_root, referenced,
    shares, subvolume_shares, summary, unaccounted,
};
pub use top::{TopQuery, TopRows, top, top_rows};
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

    /// The saved scan the view reads, and the id in it of the view's root:
    /// what a query reads by SQL where that is faster than a walk; `None`
    /// for the arena.
    fn as_index(&self) -> Option<(&Index, u32)> {
        None
    }

    /// The directories under `dir` (itself first) where a volume starts,
    /// in id order, with their totals; for a file, the file, on its
    /// directory's device ([`file_root`]). A view that knows no volumes has
    /// one, starting at `dir`, whose device is 0 and whose bytes as `du`
    /// counts them are its allocated ones.
    fn volume_roots(&self, dir: u32) -> Vec<VolumeRoot> {
        let node = self.node(dir);
        vec![VolumeRoot {
            id: dir,
            last: node.last_id,
            dev: 0,
            allocated: node.allocated,
            apparent: node.apparent,
            du_compatible: node.allocated,
        }]
    }

    /// The subvolumes under `dir`, itself included, in id order, with their
    /// totals: the directories flagged `subvolume` (a btrfs subvolume, a ZFS
    /// dataset). A view that cannot count as `du` does counts their
    /// allocated bytes; `dev` is 0, since a subvolume is placed by its path.
    fn subvolume_roots(&self, dir: u32) -> Vec<VolumeRoot> {
        let last = self.node(dir).last_id;
        (dir..=last)
            .map(|id| self.node(id))
            .filter(|node| node.is_dir() && node.flags.contains(Flags::SUBVOLUME))
            .map(|node| VolumeRoot {
                id: node.id,
                last: node.last_id,
                dev: 0,
                allocated: node.allocated,
                apparent: node.apparent,
                du_compatible: node.allocated,
            })
            .collect()
    }

    /// The owner and group of entry `id` (`uid`, `gid`), where `--attrs
    /// full` collected them; `None` in a basic scan, as its mode is. A
    /// view that keeps no owners has none.
    fn owner(&self, _id: u32) -> Option<(u32, u32)> {
        None
    }

    /// The logical size of the dataless files under `dir`.
    fn dataless_bytes(&self, dir: u32) -> u64 {
        self.dataless(dir).apparent
    }

    /// The dataless files under `dir` (`dir` itself, when it is one): how
    /// many, their logical size and the bytes they hold on disk; a later
    /// link of one counts once.
    fn dataless(&self, dir: u32) -> Dataless {
        dataless_nodes(self, dir)
    }
}

/// The dataless files under `dir` of `view`, read a node at a time.
fn dataless_nodes(view: &(impl TreeView + ?Sized), dir: u32) -> Dataless {
    let last = view.node(dir).last_id;
    (dir..=last)
        .map(|id| view.node(id))
        .filter(|node| !node.is_dir() && !node.linked_away && node.flags.contains(Flags::DATALESS))
        .fold(Dataless::default(), |sum, node| sum.add(&node))
}

/// The dataless files of a subtree: cloud placeholders (iCloud, Dropbox,
/// `OneDrive`) whose content is not on the disk, so deleting one frees no
/// space here.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Dataless {
    /// How many there are.
    pub files: u64,
    /// Their logical size: apparent bytes.
    pub apparent: u64,
    /// The bytes on disk of those charged to the subtree, little or none.
    pub allocated: u64,
}

impl Dataless {
    /// The tally with `node`, a dataless file, added.
    fn add(self, node: &Node) -> Dataless {
        Dataless {
            files: self.files + 1,
            apparent: self.apparent + node.apparent,
            allocated: self.allocated + if node.charged { node.allocated } else { 0 },
        }
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

    /// From the entry's `--attrs full` side record.
    fn owner(&self, id: u32) -> Option<(u32, u32)> {
        self.arena
            .full(self.rollup.entry_at(id))
            .map(|full| (full.uid, full.gid))
    }

    /// From the arena's crossings: where the walk met another device.
    fn volume_roots(&self, dir: u32) -> Vec<VolumeRoot> {
        let last = self.node(dir).last_id;
        let charges = self.rollup.charges();
        let root = |id: u32, dev: u64| {
            let rec = self
                .arena
                .entry(self.rollup.entry_at(id))
                .dir
                .map(|d| (self.arena.dir(d), d));
            rec.map(|(rec, d)| VolumeRoot {
                id,
                last: rec.last_id,
                dev,
                allocated: rec.cum_allocated,
                apparent: rec.cum_apparent,
                du_compatible: charges.du_compatible(self.arena, d),
            })
        };
        let mut crossings: Vec<(u32, u64)> = self
            .arena
            .crossings()
            .iter()
            .map(|c| (self.rollup.id_of(c.entry), c.dev))
            .collect();
        crossings.sort_unstable();
        // The device of `dir` itself: that of the innermost crossing at or
        // above it, else the root's. A crossing is a directory, so a file's
        // is its directory's.
        let dev = crossings
            .iter()
            .filter(|&&(id, _)| id <= dir && self.node(id).last_id >= dir)
            .max_by_key(|&&(id, _)| id)
            .map_or(self.arena.root_volume().0, |&(_, dev)| dev);
        let node = self.node(dir);
        if !node.is_dir() {
            return file_root(&node, Some(dev)).into_iter().collect();
        }
        let mut roots: Vec<VolumeRoot> = root(dir, dev).into_iter().collect();
        roots.extend(
            crossings
                .into_iter()
                .filter(|&(id, _)| id > dir && id <= last)
                .filter_map(|(id, dev)| root(id, dev)),
        );
        roots
    }

    /// The whole tree's in one pass over the arena's records, which a scan's
    /// `dataless_present` warning asks for, far cheaper than a node each;
    /// a subtree's as the default reads it.
    fn dataless(&self, dir: u32) -> Dataless {
        if dir != 0 {
            return dataless_nodes(self, dir);
        }
        let charges = self.rollup.charges();
        let mut sum = Dataless::default();
        for entry in self.arena.entries() {
            if entry.dir.is_none()
                && entry.flags.contains(Flags::DATALESS)
                && !charges.linked_away(entry.idx)
            {
                sum.files += 1;
                sum.apparent += entry.apparent;
                if charges.charged(entry.idx) {
                    sum.allocated += entry.allocated;
                }
            }
        }
        sum
    }

    /// Through the entries' flags, as the default does, counting as `du`
    /// does from the roll-up.
    fn subvolume_roots(&self, dir: u32) -> Vec<VolumeRoot> {
        let last = self.node(dir).last_id;
        let charges = self.rollup.charges();
        (dir..=last)
            .filter_map(|id| {
                let view = self.arena.entry(self.rollup.entry_at(id));
                let d = view.dir.filter(|_| view.flags.contains(Flags::SUBVOLUME))?;
                let rec = self.arena.dir(d);
                Some(VolumeRoot {
                    id,
                    last: rec.last_id,
                    dev: 0,
                    allocated: rec.cum_allocated,
                    apparent: rec.cum_apparent,
                    du_compatible: charges.du_compatible(self.arena, d),
                })
            })
            .collect()
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
        let at = id + self.base;
        let mut node = self.view.node(at);
        // An entry below the base lies inside its subtree: its parent at or
        // after the base, its own files and subtree ending by the
        // subtree's end. A saved scan's row that does not is a failed read,
        // and its ids are kept inside the subtree, so that no walk of the
        // view reads past its last id.
        let end = self.last.max(at);
        if (id != 0 && node.parent < self.base) || node.last_id > end || node.files_end > end {
            let table = if node.is_dir() { "dirs" } else { "files" };
            let (parent, last_id) = (node.parent, node.last_id);
            out_of_layout(self, table, id, |base| {
                // `base` is this view's root in the scan; the node's ids are
                // those of the view below, whose root lies `self.base`
                // before it.
                let shift = base.saturating_sub(self.base);
                format!(
                    "its parent_id {} and last_id {} put it outside the subtree of the entry \
                     {base}, which ends at {}",
                    parent.saturating_add(shift),
                    last_id.saturating_add(shift),
                    self.last.saturating_add(shift)
                )
            });
        }
        node.id = id;
        node.parent = if id == 0 {
            0
        } else {
            node.parent.saturating_sub(self.base)
        };
        node.depth = node.depth.saturating_sub(self.depth);
        node.files_end = node.files_end.clamp(at, end) - self.base;
        node.last_id = node.last_id.clamp(at, end) - self.base;
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

    fn as_index(&self) -> Option<(&Index, u32)> {
        let (index, base) = self.view.as_index()?;
        Some((index, base + self.base))
    }

    fn volume_roots(&self, dir: u32) -> Vec<VolumeRoot> {
        self.view
            .volume_roots(dir + self.base)
            .into_iter()
            .map(|r| VolumeRoot {
                id: r.id.saturating_sub(self.base),
                last: r.last.saturating_sub(self.base),
                ..r
            })
            .collect()
    }

    fn subvolume_roots(&self, dir: u32) -> Vec<VolumeRoot> {
        self.view
            .subvolume_roots(dir + self.base)
            .into_iter()
            .map(|r| VolumeRoot {
                id: r.id.saturating_sub(self.base),
                last: r.last.saturating_sub(self.base),
                ..r
            })
            .collect()
    }

    fn dataless(&self, dir: u32) -> Dataless {
        self.view.dataless(dir + self.base)
    }

    fn owner(&self, id: u32) -> Option<(u32, u32)> {
        self.view.owner(id + self.base)
    }
}

/// The largest non-directories under `dir` of `view` in allocated bytes,
/// ties by path, at most `limit`, those past `cut` when given, and how many
/// matched, where the view reads a saved scan ([`Index::largest_files`]);
/// `None` for the arena, or when the read failed, which the index records.
pub fn ranked_files(
    view: &(impl TreeView + ?Sized),
    dir: u32,
    limit: usize,
    cut: Option<Cut>,
) -> Option<Ranked> {
    let (index, base) = view.as_index()?;
    match index.largest_files(dir + base, limit, cut, None) {
        Ok(ranked) => Some(Ranked {
            ids: ranked
                .ids
                .into_iter()
                .map(|id| id.saturating_sub(base))
                .collect(),
            total_count: ranked.total_count,
        }),
        Err(e) => {
            index.record(&e);
            None
        }
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
///
/// A saved scan's rows are held to the layout as they are listed, so that
/// a walk down the tree reads each entry once, however its rows lie: each
/// id up to `files_end` must be a file whose parent is `dir`, and the
/// subdirectories' subtrees must fill the ids after the files, to
/// `last_id`, in order. The first that does not is the index's failed read
/// (`index_corrupt`), and the list ends there, as it does once any read has
/// failed ([`failed`]). The arena keeps the layout, and is not checked.
pub fn children<'v>(
    view: &'v (impl TreeView + ?Sized),
    dir: &Node,
) -> impl Iterator<Item = u32> + 'v {
    let (id, files_end, last) = (dir.id, dir.files_end, dir.last_id);
    let dirs = if dir.is_dir() {
        view.child_dirs(id)
    } else {
        Vec::new()
    };
    let saved = view.as_index().is_some();
    // Checked once the files are listed, so that a damaged row met on the
    // way is the one reported.
    let mut filled = (saved && dir.is_dir()).then(|| fills(&dirs, files_end, last));
    let files = ((id + 1)..=files_end).take_while(move |&file| own_child(view, id, file, false));
    let settle = std::iter::from_fn(move || {
        if filled.take() == Some(false) {
            unfilled(view, id, files_end, last);
        }
        None
    });
    files
        .chain(settle)
        .chain(dirs.into_iter().map(|(id, _)| id))
        .take_while(move |_| !failed(view))
}

/// Whether entry `id` of `view`, which directory `dir`'s ids place among
/// its files (or its subdirectories, when `is_dir`), is one: of that kind,
/// and `dir` its parent. One that is not breaks `dir`'s row, which is
/// recorded; so is a read that failed. The arena keeps the layout.
pub(crate) fn own_child(view: &(impl TreeView + ?Sized), dir: u32, id: u32, is_dir: bool) -> bool {
    if view.as_index().is_none() {
        return true;
    }
    let node = view.node(id);
    if failed(view) {
        return false;
    }
    let own = node.parent == dir && node.is_dir() == is_dir;
    if !own {
        out_of_layout(view, "dirs", dir, |base| {
            let (column, kind) = if is_dir {
                ("last_id", "directory")
            } else {
                ("files_end", "file")
            };
            format!(
                "its {column} takes in id {} as a {kind}, which is not a {kind} whose parent_id \
                 is {}",
                id.saturating_add(base),
                dir.saturating_add(base)
            )
        });
    }
    own
}

/// Records that the subdirectories of directory `dir` of `view`, whose own
/// files end at `files_end` and subtree at `last`, do not fill the ids
/// after its files ([`fills`]).
pub(crate) fn unfilled(view: &(impl TreeView + ?Sized), dir: u32, files_end: u32, last: u32) {
    out_of_layout(view, "dirs", dir, |base| {
        format!(
            "the subtrees of its subdirectories do not fill its ids from files_end + 1 to \
             last_id (files_end {}, last_id {})",
            files_end.saturating_add(base),
            last.saturating_add(base)
        )
    });
}

/// Whether the subtrees `dirs` (each one's first and last id) fill the ids
/// from `files_end + 1` to `last` in order, with no gap and no overlap.
fn fills(dirs: &[(u32, u32)], files_end: u32, last: u32) -> bool {
    let mut next = u64::from(files_end) + 1;
    for &(first, end) in dirs {
        if u64::from(first) != next || end < first {
            return false;
        }
        next = u64::from(end) + 1;
    }
    next == u64::from(last) + 1
}

/// Records on the saved scan `view` reads, as its first failed read when it
/// is, that the row of entry `id` (an id of `view`) in `table` breaks the
/// layout: `what`, given the id in the scan of the view's root, which the
/// view's ids are counted from. The arena keeps the layout.
pub(crate) fn out_of_layout(
    view: &(impl TreeView + ?Sized),
    table: &str,
    id: u32,
    what: impl FnOnce(u32) -> String,
) {
    if let Some((index, base)) = view.as_index() {
        index.out_of_layout(table, id.saturating_add(base), &what(base));
    }
}

/// Whether a read of the saved scan `view` reads has failed
/// ([`Index::failed`]): the verb then fails `index_corrupt`, whatever else
/// it reads. Never for the arena.
pub fn failed(view: &(impl TreeView + ?Sized)) -> bool {
    view.as_index().is_some_and(|(index, _)| index.failed())
}

/// The ids `first ..= last` of `view`, in order, until a read of the saved
/// scan it reads has failed ([`failed`]): a walk over the ids stops at the
/// first id that no row holds, rather than ask for each of the billions a
/// crafted `n_entries` may leave.
pub fn ids(view: &(impl TreeView + ?Sized), first: u32, last: u32) -> impl Iterator<Item = u32> {
    let index = view.as_index().map(|(index, _)| index);
    (first..=last).take_while(move |_| !index.is_some_and(Index::failed))
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

/// `bytes` in the largest unit of `base` it reaches (`units` from bytes
/// up), with one decimal; below the first step, a whole number of bytes. A
/// value that rounds up to the next unit is shown in it (`1.0 MiB`, not
/// `1024.0 KiB`).
pub fn scaled(bytes: u64, base: u64, units: &[&str]) -> String {
    let mut unit = 0;
    let mut divisor = 1u128;
    while u128::from(bytes) >= divisor * u128::from(base) && unit + 1 < units.len() {
        divisor *= u128::from(base);
        unit += 1;
    }
    if unit == 0 {
        return format!("{bytes} {}", units[0]);
    }
    // One decimal from integer arithmetic: tenths of the unit, rounded.
    let mut tenths = (u128::from(bytes) * 10 + divisor / 2) / divisor;
    if tenths >= u128::from(base) * 10 && unit + 1 < units.len() {
        divisor *= u128::from(base);
        unit += 1;
        tenths = (u128::from(bytes) * 10 + divisor / 2) / divisor;
    }
    format!("{}.{} {}", tenths / 10, tenths % 10, units[unit])
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
    /// Whether no filter is set: every row is kept.
    pub fn is_empty(&self) -> bool {
        self.glob.is_none()
            && self.regex.is_none()
            && self.exclude.is_none()
            && self.kinds.is_empty()
            && self.min_size.is_none()
            && self.max_size.is_none()
            && self.min_depth.is_none()
            && self.max_depth.is_none()
            && self.before.is_none()
            && self.after.is_none()
    }

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
    /// Rows matched before `--limit`, those of earlier pages included.
    pub total_count: u64,
    /// Where the next page starts, when rows remain and the verb pages.
    pub next: Option<After>,
    /// Whether rows remain after this page: after `--limit`, or after a
    /// cursor's page.
    pub truncated: bool,
}

/// Whether rows remain past `returned` rows after `seen`, of `total`.
pub fn remain(seen: u64, returned: usize, total: u64) -> bool {
    seen.saturating_add(returned as u64) < total
}

/// [`ranked_by`] for the page after `after`: the first `limit` candidates
/// that sort after the row it names, and how many candidates there were in
/// all, those before it included. Without `after`, the first page.
pub fn ranked_after(
    view: &(impl TreeView + ?Sized),
    candidates: impl IntoIterator<Item = u32>,
    key: SortKey,
    reverse: bool,
    limit: Option<usize>,
    size_of: impl Fn(&Node) -> u64,
    after: Option<&After>,
) -> (Vec<u32>, u64) {
    let Some(after) = after else {
        return ranked_by(view, candidates, key, reverse, limit, size_of);
    };
    let row_value = sort_value(&view.node(after.id), key, reverse, &size_of);
    let row_path = rel_path(view, after.id);
    let paths_reversed = reverse && key == SortKey::Path;
    let total = std::cell::Cell::new(0u64);
    let past = candidates
        .into_iter()
        .inspect(|_| total.set(total.get() + 1))
        .filter(
            |&id| match sort_value(&view.node(id), key, reverse, &size_of).cmp(&row_value) {
                Ordering::Less => false,
                Ordering::Greater => true,
                Ordering::Equal => {
                    let path = rel_path(view, id);
                    if paths_reversed {
                        path < row_path
                    } else {
                        path > row_path
                    }
                }
            },
        );
    let (ids, _) = ranked_by(view, past, key, reverse, limit, |node: &Node| size_of(node));
    (ids, total.get())
}

/// Where the page after `ids` starts, `seen` rows having come before
/// them, of `total`: after its last row, when rows remain.
pub fn next_after(
    view: &(impl TreeView + ?Sized),
    ids: &[u32],
    key: SortKey,
    size: SizeKind,
    seen: u64,
    total: u64,
) -> Option<After> {
    let seen = seen.saturating_add(ids.len() as u64);
    let last = *ids.last()?;
    (seen < total).then(|| After::of(&view.node(last), key, size, seen))
}

/// Whether `after` names a row of `view` with the sort value it records:
/// a cursor of this scan and sort, not another's.
pub fn after_fits(
    view: &(impl TreeView + ?Sized),
    after: &After,
    key: SortKey,
    size: SizeKind,
) -> bool {
    after.id < view.len()
        && After::of(&view.node(after.id), key, size, after.seen).value == after.value
}

/// The page of `ranked` (the first `seen + limit` rows, as the index's
/// plans rank them) after `after`, when its row is where the cursor says:
/// the rows after it, else `None` (the walk pages).
pub fn page_of(ranked: Ranked, after: Option<&After>) -> Option<Ranked> {
    let Some(after) = after else {
        return Some(ranked);
    };
    let seen = usize::try_from(after.seen).ok()?;
    let at = seen.checked_sub(1)?;
    (ranked.ids.get(at) == Some(&after.id)).then(|| Ranked {
        ids: ranked.ids[seen..].to_vec(),
        total_count: ranked.total_count,
    })
}

/// What a query was asked for that the scan did not collect.
pub(crate) fn attrs_not_collected(what: &'static str) -> CoreError {
    CoreError::AttrsNotCollected(what)
}

#[cfg(test)]
mod tests;
