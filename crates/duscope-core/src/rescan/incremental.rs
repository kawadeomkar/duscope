//! The incremental rescan (P5-CORE-01): a saved scan brought up to date
//! from its volumes' `FSEvents` history instead of a full walk.
//!
//! [`plan`] runs the checks: the platform keeps a history, the saved scan
//! is younger than `--max-age`, the options it would scan with are the ones
//! it was taken with, each volume's history is one a rescan reuses (APFS,
//! HFS+) and still answers from the scan's mark (its UUID, its event ids,
//! its logging, no lost events), and the directories the history reports
//! are at most 15% of the scan's. A failed check is a [`Fallback`], which
//! `rescan` turns into a full scan, or with `--incremental` into an error.
//! A volume the scan entered below its root that fails one is read whole
//! instead.
//!
//! [`merge`] then walks the saved scan's root with the walk's own options,
//! but reads from disk only the directories it must: the root, every
//! directory the history reports, every one on the way down to them, their
//! subdirectories, every one under a subtree the history says to read
//! whole, and every one the saved scan does not hold or whose times no
//! longer match its row. Every other directory is listed by its parent, so
//! its own entry is read fresh, and its contents are taken from the saved
//! scan: its subtree's rows, read in id order, replace the walk's empty
//! read. The arena that results is rolled up and written as any scan's is,
//! a complete new index, so hard links and clones are charged again over
//! the whole merged tree.
//!
//! Before the write a sample of the directories taken from the saved scan
//! is read again and compared on `mtime` and `ctime` (the post-check), and
//! every file taken from it that shares its blocks or its inode is read
//! again: a link made, removed or written through elsewhere, or a clone
//! deleted, changes such a file without changing its directory.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use duscope_walk::fsevents::{self, REPLAY_TIMEOUT, Replay};
use duscope_walk::{
    Attrs, Boundary, DirHandle, DirId, Entry, Flags, Flush, FsEventsError, FsEventsMark, FullAttrs,
    Kind, NameRef, Provider, ScanError, ScanEvent, Share, Skip, VolumeClass, VolumeInfo, WalkError,
    WalkOptions, WalkSummary, stamp_dirs,
};
use rusqlite::{Connection, Row};

use super::ScanOptions;
use crate::CoreError;
use crate::index::Index;
use crate::index::reader::dir_ids;
use crate::index::writer::{EXCLUDED, EXCLUDED_CACHE, FIRMLINK, SKIPPED_CONTAINERS};
use crate::query::{TreeView, kind_name};

/// `--max-age`'s default: a saved scan older than 7 days is scanned whole.
pub const MAX_AGE_NS: i64 = 7 * 24 * 3600 * 1_000_000_000;

/// The share of the saved scan's directories the history may report before
/// a full scan is cheaper: 15%.
const MOST_CHANGED_PERCENT: usize = 15;

/// The post-check reads 1% of the directories taken from the saved scan,
/// and at least this many (all of them when there are fewer).
const POST_CHECK_MIN: usize = 1000;

/// Why a rescan read the whole tree: the scan block's `fallback_reason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Fallback {
    /// The saved scan is older than `--max-age`.
    IndexTooOld,
    /// A volume's history is not the one the scan recorded: another device,
    /// another `FSEvents` database, or none recorded.
    VolumeChanged,
    /// The history no longer reaches back to the scan, or cannot narrow it.
    HistoryGone,
    /// `FSEvents` logging is off on the volume (`.fseventsd/no_log`), or the
    /// volume is one whose history is never reused (a network, FAT or exFAT
    /// volume).
    FseventsDisabled,
    /// The options the rescan would scan with are not the scan's.
    OptionsChanged,
    /// The history reports more than 15% of the scan's directories.
    TooManyChanges,
    /// `FSEvents` dropped events since the scan.
    EventsDropped,
    /// A directory or file read again differs from what the history says.
    PostCheckFailed,
}

impl Fallback {
    /// Every reason, in the order the conventions list them.
    pub const ALL: [Fallback; 8] = [
        Fallback::IndexTooOld,
        Fallback::VolumeChanged,
        Fallback::HistoryGone,
        Fallback::FseventsDisabled,
        Fallback::OptionsChanged,
        Fallback::TooManyChanges,
        Fallback::EventsDropped,
        Fallback::PostCheckFailed,
    ];

    /// The `fallback_reason` value.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Fallback::IndexTooOld => "index_too_old",
            Fallback::VolumeChanged => "volume_changed",
            Fallback::HistoryGone => "history_gone",
            Fallback::FseventsDisabled => "fsevents_disabled",
            Fallback::OptionsChanged => "options_changed",
            Fallback::TooManyChanges => "too_many_changes",
            Fallback::EventsDropped => "events_dropped",
            Fallback::PostCheckFailed => "post_check_failed",
        }
    }

    /// The reason in words, for people: `options changed`.
    #[must_use]
    pub fn words(self) -> &'static str {
        match self {
            Fallback::IndexTooOld => "index too old",
            Fallback::VolumeChanged => "volume changed",
            Fallback::HistoryGone => "history gone",
            Fallback::FseventsDisabled => "FSEvents disabled",
            Fallback::OptionsChanged => "options changed",
            Fallback::TooManyChanges => "too many changes",
            Fallback::EventsDropped => "events dropped",
            Fallback::PostCheckFailed => "post-check failed",
        }
    }
}

impl From<fsevents::Fallback> for Fallback {
    fn from(reason: fsevents::Fallback) -> Fallback {
        match reason {
            fsevents::Fallback::VolumeChanged => Fallback::VolumeChanged,
            fsevents::Fallback::HistoryGone => Fallback::HistoryGone,
            fsevents::Fallback::FseventsDisabled => Fallback::FseventsDisabled,
            fsevents::Fallback::EventsDropped => Fallback::EventsDropped,
        }
    }
}

/// Why the incremental path stopped.
#[derive(Debug)]
pub enum Stop {
    /// The platform keeps no change history (Linux): every rescan is a full
    /// one, which is no failed check.
    NoHistory,
    /// A check failed: the reason, and what failed, in a sentence.
    Fallback(Fallback, String),
    /// The walk failed, as a full scan's would.
    Walk(WalkError),
    /// The saved scan could not be read.
    Index(CoreError),
}

impl Stop {
    fn fallback(reason: Fallback, detail: impl Into<String>) -> Stop {
        Stop::Fallback(reason, detail.into())
    }
}

/// Where the volumes' change history comes from: `FSEvents` ([`FsEvents`]),
/// or a script in the tests.
pub trait History {
    /// The latest event id, sampled before any replay; `None` where the
    /// platform keeps no history.
    fn now(&self) -> Option<u64>;
    /// What changed on the volume on `dev` below `under` since `mark`.
    ///
    /// # Errors
    ///
    /// The history cannot be read from `mark` ([`FsEventsError::fallback`]
    /// says why).
    fn replay(&self, dev: u64, mark: &FsEventsMark, under: &Path) -> Result<Replay, FsEventsError>;
    /// The mark of the volume on `dev` at the event id `now`, for the new
    /// scan's `scan_volumes`.
    fn mark(&self, dev: u64, now: u64) -> Option<FsEventsMark>;
    /// Whether the history of a volume of type `fs_type` may stand in for
    /// reading it again.
    fn reusable(&self, fs_type: &str) -> bool;
}

/// The system's `FSEvents` history.
#[derive(Debug, Clone, Copy, Default)]
pub struct FsEvents;

impl History for FsEvents {
    fn now(&self) -> Option<u64> {
        fsevents::current_event_id()
    }

    fn replay(&self, dev: u64, mark: &FsEventsMark, under: &Path) -> Result<Replay, FsEventsError> {
        fsevents::replay(dev, mark, under, REPLAY_TIMEOUT)
    }

    fn mark(&self, dev: u64, now: u64) -> Option<FsEventsMark> {
        fsevents::mark(dev, now)
    }

    fn reusable(&self, fs_type: &str) -> bool {
        fsevents::reusable(fs_type)
    }
}

/// What the checks measure against.
#[derive(Debug, Clone, Copy)]
pub struct Checks<'a> {
    /// Now, Unix nanoseconds, on the clock the scans are timed on.
    pub now_ns: i64,
    /// `--max-age`: how old the saved scan may be.
    pub max_age_ns: i64,
    /// This duscope's version: a scan another one wrote may have read its
    /// tree otherwise.
    pub tool_version: &'a str,
}

/// An incremental rescan, its checks passed: the saved tree and what its
/// history reports changed in it.
#[derive(Debug)]
pub struct Plan {
    root: PathBuf,
    /// The event id sampled before the replays, which the new scan's marks
    /// carry: a change after it is the next rescan's.
    event_id: u64,
    saved: Saved,
    changes: Changes,
    reported: usize,
}

impl Plan {
    /// The directories the history reports, a subtree to read whole counted
    /// as its directories.
    #[must_use]
    pub fn reported(&self) -> usize {
        self.reported
    }

    /// The saved scan's directories.
    #[must_use]
    pub fn saved_dirs(&self) -> usize {
        self.saved.dirs.len()
    }
}

/// Runs the checks of an incremental rescan of `index`'s scan.
///
/// # Errors
///
/// [`Stop::NoHistory`] where the platform keeps none, [`Stop::Fallback`]
/// for a failed check, [`Stop::Index`] when the saved scan cannot be read.
pub fn plan(index: &Index, checks: &Checks<'_>, history: &dyn History) -> Result<Plan, Stop> {
    // Sampled before any replay: whatever changes after it is the next
    // rescan's to read.
    let Some(event_id) = history.now() else {
        return Err(Stop::NoHistory);
    };
    let meta = index.meta();
    let age = checks.now_ns.saturating_sub(meta.started_at);
    if age > checks.max_age_ns {
        return Err(Stop::fallback(
            Fallback::IndexTooOld,
            format!(
                "saved scan {} is {} old, older than --max-age {}",
                meta.scan_id,
                days(age),
                days(checks.max_age_ns)
            ),
        ));
    }
    let (options, attrs) = super::options(index).map_err(Stop::Index)?;
    // FSEvents records no reads, so an incremental rescan keeps the access
    // times the saved scan read for what it takes from it. Under `--attrs
    // full` a saved scan that was itself incremental may hold access times
    // older than its own age; it is read whole, so they are never older than
    // --max-age (DEV-142).
    if attrs == Attrs::Full && meta.incremental {
        return Err(Stop::fallback(
            Fallback::IndexTooOld,
            format!(
                "saved scan {} is itself an incremental rescan: under --attrs full, the access \
                 times it took from its own saved scan may be older than --max-age {}",
                meta.scan_id,
                days(checks.max_age_ns)
            ),
        ));
    }
    options_unchanged(index, &options, checks)?;
    let saved = Saved::load(index).map_err(Stop::Index)?;
    let root = index.root_path().to_owned();
    let mut changes = Changes::default();
    replay(&saved, &root, history, &mut changes)?;
    let reported = changes.count(&saved);
    let total = saved.dirs.len().max(1);
    if reported.saturating_mul(100) > total.saturating_mul(MOST_CHANGED_PERCENT) {
        return Err(Stop::fallback(
            Fallback::TooManyChanges,
            format!(
                "{reported} of the saved scan's {total} directories changed, more than \
                 {MOST_CHANGED_PERCENT}%"
            ),
        ));
    }
    changes.close();
    Ok(Plan {
        root,
        event_id,
        saved,
        changes,
        reported,
    })
}

/// A duration in days, for a message.
#[allow(clippy::cast_precision_loss)]
fn days(ns: i64) -> String {
    format!("{:.1} days", ns as f64 / 86_400e9)
}

/// The options check: the scan was written by this duscope, takes no link
/// out of its root, and the files its options name are as they were.
fn options_unchanged(
    index: &Index,
    options: &ScanOptions,
    checks: &Checks<'_>,
) -> Result<(), Stop> {
    let meta = index.meta();
    if meta.tool_version != checks.tool_version {
        return Err(Stop::fallback(
            Fallback::OptionsChanged,
            format!(
                "saved scan {} was taken by duscope {}, not {}",
                meta.scan_id, meta.tool_version, checks.tool_version
            ),
        ));
    }
    // A scan that followed links read directories outside its root, and
    // read some inside it twice, under the link's name: the history of the
    // root speaks for neither.
    if options.follow_symlinks {
        return Err(Stop::fallback(
            Fallback::HistoryGone,
            "a scan that followed symbolic links read directories its root's history does not \
             cover",
        ));
    }
    let files = options
        .exclude_files
        .iter()
        .chain(&options.ignore_files)
        .chain(&options.caches_extra);
    for file in files {
        let changed = match std::fs::metadata(file) {
            Ok(meta_now) => mtime_ns(&meta_now) > meta.started_at,
            Err(_) => true,
        };
        if changed {
            return Err(Stop::fallback(
                Fallback::OptionsChanged,
                format!("{file}, which the scan's options name, changed since the scan"),
            ));
        }
    }
    Ok(())
}

/// A file's modification time, Unix nanoseconds.
fn mtime_ns(meta: &std::fs::Metadata) -> i64 {
    use std::os::unix::fs::MetadataExt;
    meta.mtime()
        .saturating_mul(1_000_000_000)
        .saturating_add(meta.mtime_nsec())
}

/// Replays each entered volume's history, at the root and at each
/// directory where the scan entered the volume below it, into `changes`.
/// A volume entered below the root whose history cannot be read is read
/// again whole; the root's volume without one fails the check.
fn replay(
    saved: &Saved,
    root: &Path,
    history: &dyn History,
    changes: &mut Changes,
) -> Result<(), Stop> {
    for volume in saved.volumes.iter().filter(|v| v.entered) {
        for point in saved.entry_points(volume.dev) {
            let at_root = point.is_empty();
            let under = absolute(root, &point);
            let read_whole = |changes: &mut Changes, reason: Fallback, detail: String| {
                if at_root {
                    Err(Stop::fallback(reason, detail))
                } else {
                    log::info!("{}: read whole: {detail}", under.display());
                    changes.deep.insert(point.clone());
                    Ok(())
                }
            };
            if !history.reusable(&volume.fs_type) {
                read_whole(
                    changes,
                    Fallback::FseventsDisabled,
                    format!(
                        "the volume at {} is {}, whose FSEvents history a rescan never reuses",
                        String::from_utf8_lossy(&volume.mount_point),
                        volume.fs_type
                    ),
                )?;
                continue;
            }
            let Some(mark) = &volume.mark else {
                read_whole(
                    changes,
                    Fallback::VolumeChanged,
                    format!(
                        "the saved scan recorded no FSEvents history for the volume at {}",
                        String::from_utf8_lossy(&volume.mount_point)
                    ),
                )?;
                continue;
            };
            let replayed = match history.replay(volume.dev, mark, &under) {
                Ok(replayed) => replayed,
                Err(e) => {
                    let reason = e.fallback().map_or(Fallback::VolumeChanged, Fallback::from);
                    read_whole(changes, reason, e.to_string())?;
                    continue;
                }
            };
            let lost = if replayed.dropped {
                Some(Fallback::EventsDropped)
            } else if replayed.ids_wrapped
                || !replayed.history_done
                || replayed.must_scan_subdirs.contains(&under)
            {
                Some(Fallback::HistoryGone)
            } else {
                None
            };
            if let Some(reason) = lost {
                let detail = match reason {
                    Fallback::EventsDropped => "FSEvents dropped events since the scan".to_owned(),
                    _ if !replayed.history_done => format!(
                        "the FSEvents history did not reach its end within {} s",
                        REPLAY_TIMEOUT.as_secs()
                    ),
                    _ => format!(
                        "the FSEvents history cannot narrow the rescan of {}",
                        under.display()
                    ),
                };
                read_whole(changes, reason, detail)?;
                continue;
            }
            for dir in &replayed.dirs {
                if let Some(rel) = relative(root, dir) {
                    changes.changed.insert(rel);
                }
            }
            for dir in &replayed.must_scan_subdirs {
                if let Some(rel) = relative(root, dir) {
                    changes.deep.insert(rel);
                }
            }
            // A change above the directory can be the directory itself
            // renamed, removed or replaced, which nothing below it records
            // (DEV-122): it must still be the directory the scan read. Where
            // the history reports its own entries changed, its ctime moved
            // with them; the walk then reads it and each of its
            // subdirectories, whose times must be the saved scan's for their
            // contents to be taken from it, which another directory put in
            // its place would not pass.
            if replayed.ancestors_changed && !replayed.dirs.contains(&under) {
                let same = saved
                    .at(&point)
                    .is_some_and(|dir| stamp(&under).is_some_and(|(_, ctime)| ctime == dir.ctime));
                if !same {
                    read_whole(
                        changes,
                        Fallback::PostCheckFailed,
                        format!(
                            "{} is not the directory the scan read: a directory above it changed, \
                             and its ctime is not the scan's",
                            under.display()
                        ),
                    )?;
                }
            }
        }
    }
    Ok(())
}

/// The modification and status-change times of the directory at `path`,
/// read as the walk reads them; `None` when it cannot be read.
fn stamp(path: &Path) -> Option<(i64, i64)> {
    let stamped = stamp_dirs(std::iter::once(path), false).pop()?.ok()?;
    Some((stamped.mtime_ns, stamped.ctime_ns))
}

/// `path` below `root`, as the bytes after `root/`; empty for `root`
/// itself, `None` outside it.
fn relative(root: &Path, path: &Path) -> Option<Vec<u8>> {
    path.strip_prefix(root)
        .ok()
        .map(|rel| rel.as_os_str().as_bytes().to_vec())
}

/// `rel` below `root`.
fn absolute(root: &Path, rel: &[u8]) -> PathBuf {
    if rel.is_empty() {
        root.to_owned()
    } else {
        root.join(OsStr::from_bytes(rel))
    }
}

/// `parent/name`, relative to the root.
fn join(parent: &[u8], name: &[u8]) -> Vec<u8> {
    let mut rel = Vec::with_capacity(parent.len() + 1 + name.len());
    rel.extend_from_slice(parent);
    if !parent.is_empty() {
        rel.push(b'/');
    }
    rel.extend_from_slice(name);
    rel
}

/// What the history reports changed, as paths relative to the root.
#[derive(Debug, Default)]
struct Changes {
    /// Directories whose own entries changed, or one of their entries'
    /// attributes: their listings are read again, and their
    /// subdirectories' too.
    changed: HashSet<Vec<u8>>,
    /// Directories whose whole subtree is read again.
    deep: HashSet<Vec<u8>>,
    /// Every directory on the way down to one of those, which the walk
    /// reads to reach them.
    above: HashSet<Vec<u8>>,
}

impl Changes {
    /// The directories reported, a subtree to read whole counted as the
    /// saved scan's directories in it.
    fn count(&self, saved: &Saved) -> usize {
        let deep: usize = self
            .deep
            .iter()
            .map(|rel| saved.at(rel).map_or(1, |dir| saved.subtree_dirs(dir)))
            .sum();
        self.changed.len() + deep
    }

    /// Fills `above` from the rest.
    fn close(&mut self) {
        for rel in self.changed.iter().chain(&self.deep) {
            let mut at = rel.len();
            while let Some(slash) = rel[..at].iter().rposition(|&c| c == b'/') {
                self.above.insert(rel[..slash].to_vec());
                at = slash;
            }
            if !rel.is_empty() {
                self.above.insert(Vec::new());
            }
        }
    }

    /// Whether the directory at `rel`, below `parent`, is read from disk
    /// whatever the saved scan says of it.
    fn reads(&self, parent: &[u8], rel: &[u8]) -> bool {
        self.changed.contains(parent)
            || self.changed.contains(rel)
            || self.above.contains(rel)
            || self.under_deep(rel)
    }

    /// Whether `rel` is at or below a subtree read whole.
    fn under_deep(&self, rel: &[u8]) -> bool {
        if self.deep.is_empty() {
            return false;
        }
        if self.deep.contains(rel) || self.deep.contains(&b""[..]) {
            return true;
        }
        rel.iter()
            .enumerate()
            .filter(|&(_, &c)| c == b'/')
            .any(|(i, _)| self.deep.contains(&rel[..i]))
    }
}

/// A directory of the saved scan, as the checks and the walk read it.
#[derive(Debug)]
struct SavedDir {
    id: u32,
    last_id: u32,
    /// The parent's id; the root's own.
    parent: u32,
    name: Box<[u8]>,
    mtime: i64,
    ctime: i64,
    dev: u64,
    flags: u32,
    errors: Option<i32>,
}

/// A volume the saved scan met.
#[derive(Debug)]
struct SavedVolume {
    dev: u64,
    mnt_id: u64,
    mount_point: Vec<u8>,
    fs_type: String,
    read_only: bool,
    entered: bool,
    reason: Option<String>,
    case_sensitive: Option<bool>,
    normalization: String,
    mark: Option<FsEventsMark>,
    total_bytes: Option<u64>,
    free_bytes: Option<u64>,
    options: Vec<String>,
}

/// The saved scan's directories and volumes, read once.
#[derive(Debug)]
struct Saved {
    /// In id order.
    dirs: Vec<SavedDir>,
    /// Each directory's subdirectories, by name: their places in `dirs`.
    children: HashMap<u32, HashMap<Box<[u8]>, usize>>,
    volumes: Vec<SavedVolume>,
    attrs: Attrs,
    /// Files the excludes left out, which the index does not hold.
    excluded_files: u64,
    clones_unavailable: bool,
}

impl Saved {
    fn load(index: &Index) -> Result<Saved, CoreError> {
        let corrupt = |e: rusqlite::Error| CoreError::IndexCorrupt {
            file: index.file().display().to_string(),
            message: e.to_string(),
        };
        let db = index.db();
        let dirs = load_dirs(db, index.len()).map_err(corrupt)?;
        let volumes = load_volumes(db).map_err(corrupt)?;
        let mut children: HashMap<u32, HashMap<Box<[u8]>, usize>> = HashMap::new();
        for (at, dir) in dirs.iter().enumerate().skip(1) {
            children
                .entry(dir.parent)
                .or_default()
                .insert(dir.name.clone(), at);
        }
        let excluded_dirs = dirs
            .iter()
            .filter(|d| d.flags & (EXCLUDED | EXCLUDED_CACHE) != 0)
            .count() as u64;
        let meta = index.meta();
        Ok(Saved {
            dirs,
            children,
            volumes,
            attrs: index.attrs(),
            excluded_files: meta.excluded_count.saturating_sub(excluded_dirs),
            clones_unavailable: meta.clones_unavailable,
        })
    }

    /// The directory with id `id`.
    fn dir(&self, id: u32) -> Option<&SavedDir> {
        self.dirs
            .binary_search_by_key(&id, |d| d.id)
            .ok()
            .map(|at| &self.dirs[at])
    }

    /// The subdirectory `name` of the directory `parent`.
    fn child(&self, parent: u32, name: &[u8]) -> Option<&SavedDir> {
        self.children
            .get(&parent)?
            .get(name)
            .map(|&at| &self.dirs[at])
    }

    /// The directory at `rel` below the root.
    fn at(&self, rel: &[u8]) -> Option<&SavedDir> {
        let mut dir = self.dirs.first()?;
        for name in rel.split(|&c| c == b'/').filter(|n| !n.is_empty()) {
            dir = self.child(dir.id, name)?;
        }
        Some(dir)
    }

    /// The path of `dir` below the root.
    fn rel(&self, dir: &SavedDir) -> Vec<u8> {
        let mut names = Vec::new();
        let mut at = dir;
        while at.id != 0 {
            names.push(&at.name[..]);
            match self.dir(at.parent) {
                Some(parent) => at = parent,
                None => break,
            }
        }
        names
            .iter()
            .rev()
            .fold(Vec::new(), |rel, name| join(&rel, name))
    }

    /// The directories of `dir`'s subtree, itself included.
    fn subtree_dirs(&self, dir: &SavedDir) -> usize {
        let from = self.dirs.partition_point(|d| d.id < dir.id);
        let to = self.dirs.partition_point(|d| d.id <= dir.last_id);
        to - from
    }

    fn volume(&self, dev: u64) -> Option<&SavedVolume> {
        self.volumes.iter().find(|v| v.dev == dev)
    }

    /// Whether `dir` is where the scan crossed onto another volume.
    fn crossing(&self, dir: &SavedDir) -> bool {
        dir.id != 0
            && self
                .dir(dir.parent)
                .is_some_and(|parent| parent.dev != dir.dev)
    }

    /// Whether the scan read `dir`'s entries: not excluded, not unreadable,
    /// not a mount it did not enter.
    fn entered(&self, dir: &SavedDir) -> bool {
        dir.flags & (EXCLUDED | EXCLUDED_CACHE) == 0
            && dir.errors.is_none()
            && (!self.crossing(dir) || self.volume(dir.dev).is_some_and(|v| v.entered))
    }

    /// Where the scan read the volume on `dev`: the root (an empty path)
    /// when it is the root's, and each directory where it entered the
    /// volume below the root.
    fn entry_points(&self, dev: u64) -> Vec<Vec<u8>> {
        let mut points = Vec::new();
        if self.dirs.first().is_some_and(|root| root.dev == dev) {
            points.push(Vec::new());
        }
        for dir in &self.dirs {
            if dir.dev == dev && self.crossing(dir) && self.entered(dir) {
                points.push(self.rel(dir));
            }
        }
        points
    }
}

/// The saved scan's directories, in id order, from a scan of `len`
/// entries. Each row is checked as the reader checks it ([`dir_ids`]):
/// [`Saved::rel`] climbs the parents to the root, and
/// [`Saved::subtree_dirs`] counts from a directory to its `last_id`.
fn load_dirs(db: &Connection, len: u32) -> rusqlite::Result<Vec<SavedDir>> {
    let mut stmt = db.prepare(
        "SELECT id, last_id, parent_id, name, mtime, ctime, dev, flags, errors, files_end \
         FROM dirs ORDER BY id",
    )?;
    let rows = stmt.query_map([], |row| {
        let id: u32 = row.get(0)?;
        let last_id: u32 = row.get(1)?;
        let parent: Option<u32> = row.get(2)?;
        dir_ids(id, parent, row.get(9)?, last_id, len)?;
        Ok(SavedDir {
            id,
            last_id,
            parent: parent.unwrap_or(id),
            name: row.get::<_, Vec<u8>>(3)?.into_boxed_slice(),
            mtime: row.get(4)?,
            ctime: row.get(5)?,
            dev: unsigned(row.get(6)?),
            flags: row.get(7)?,
            errors: row.get(8)?,
        })
    })?;
    rows.collect()
}

fn load_volumes(db: &Connection) -> rusqlite::Result<Vec<SavedVolume>> {
    let mut stmt = db.prepare(
        "SELECT dev, mnt_id, mount_point, fs_type, read_only, entered, reason, case_sensitive, \
         normalization, fsevents_uuid, fsevents_event_id, os_build, total_bytes, free_bytes, \
         options FROM scan_volumes ORDER BY rowid",
    )?;
    let rows = stmt.query_map([], |row| {
        let uuid: Option<String> = row.get(9)?;
        let event_id: Option<i64> = row.get(10)?;
        let os_build: Option<String> = row.get(11)?;
        let mark = match (uuid, event_id, os_build) {
            (Some(uuid), Some(event_id), Some(os_build)) => Some(FsEventsMark {
                uuid,
                event_id: unsigned(event_id),
                os_build,
            }),
            _ => None,
        };
        let options: Option<String> = row.get(14)?;
        Ok(SavedVolume {
            dev: unsigned(row.get(0)?),
            mnt_id: unsigned(row.get(1)?),
            mount_point: row.get(2)?,
            fs_type: row.get(3)?,
            read_only: row.get(4)?,
            entered: row.get(5)?,
            reason: row.get(6)?,
            case_sensitive: row.get(7)?,
            normalization: row.get(8)?,
            mark,
            total_bytes: row.get::<_, Option<i64>>(12)?.map(unsigned),
            free_bytes: row.get::<_, Option<i64>>(13)?.map(unsigned),
            options: options
                .filter(|o| !o.is_empty())
                .map(|o| o.split(',').map(str::to_owned).collect())
                .unwrap_or_default(),
        })
    })?;
    rows.collect()
}

/// An index's signed integer as the unsigned value it stores.
#[allow(clippy::cast_sign_loss)]
fn unsigned(value: i64) -> u64 {
    value as u64
}

/// What the merge did.
#[derive(Debug, Clone)]
pub struct Merged {
    /// The counts of the merged scan, as a full walk's would be: the
    /// directories read and those taken from the saved scan.
    pub summary: WalkSummary,
    /// The directories read from disk.
    pub rescanned_dirs: u64,
}

/// Walks `root` with `opts` through `provider`, reading from disk what
/// `plan` says changed and taking the rest from `index`, the saved scan it
/// was planned from, and delivers the merged tree's events to `sink`, as a
/// full walk would deliver its own. The volumes are marked with the event
/// id sampled before the replays, read by `history`.
///
/// # Errors
///
/// [`Stop::Walk`] when the walk fails, [`Stop::Index`] when the saved scan
/// cannot be read, [`Stop::Fallback`] (`post_check_failed`) when the
/// post-check finds a directory or a file the history did not account for.
pub fn merge(
    plan: &Plan,
    index: &Index,
    root: &Path,
    opts: &WalkOptions,
    provider: &dyn Provider,
    history: &dyn History,
    sink: &mut dyn FnMut(ScanEvent),
) -> Result<Merged, Stop> {
    if root != plan.root {
        return Err(Stop::fallback(
            Fallback::PostCheckFailed,
            format!(
                "the saved scan's root {} is {} now",
                plan.root.display(),
                root.display()
            ),
        ));
    }
    let pruning = Pruning {
        inner: provider,
        plan,
        decisions: Mutex::new(HashMap::from([(root.to_owned(), Decision::Read(Some(0)))])),
        reading: Mutex::new(HashMap::new()),
        grafts: Mutex::new(Vec::new()),
        reads: AtomicU64::new(0),
    };
    let mut tap = Tap {
        sink,
        history,
        event_id: plan.event_id,
        next_dir: 1,
        volumes: HashSet::new(),
        excluded_dirs: 0,
    };
    let started = Instant::now();
    let walked = duscope_walk::walk_with(&pruning, root, opts, |event| tap.push(event))
        .map_err(Stop::Walk)?;
    let rescanned_dirs = pruning.reads.load(Ordering::Relaxed);
    let mut grafts = pruning
        .grafts
        .into_inner()
        .unwrap_or_else(PoisonError::into_inner);
    grafts.sort_by_key(|g| g.saved);
    log::info!(
        "incremental walk: {rescanned_dirs} directories read, {} subtrees taken from the saved \
         scan in {:.3} s",
        grafts.len(),
        started.elapsed().as_secs_f64()
    );
    let grafted = Instant::now();
    let mut graft = Grafting::new(plan, index, provider, opts, &mut tap)?;
    for one in &grafts {
        graft.run(one)?;
    }
    let Grafting {
        counts, sampled, ..
    } = graft;
    log::info!(
        "incremental merge: {} entries taken from the saved scan in {:.3} s",
        counts.entries,
        grafted.elapsed().as_secs_f64()
    );
    post_check(&sampled)?;
    let mut summary = walked;
    summary.entries += counts.entries;
    summary.dirs += counts.dirs;
    summary.unreadable_dirs += counts.unreadable_dirs;
    summary.entry_errors += counts.entry_errors;
    summary.skipped_dirs += counts.skipped_dirs;
    summary.volumes += counts.volumes;
    summary.mounts_excluded += counts.mounts_excluded;
    // The files the excludes leave out are not in the index, which keeps no
    // count of them by directory: they are counted as the saved scan
    // counted them. The directories are counted as the merged tree holds
    // them, the walk's and the saved scan's alike, each once (the tap sees
    // both).
    summary.excluded = plan.saved.excluded_files + tap.excluded_dirs;
    summary.clones_unavailable |= plan.saved.clones_unavailable && counts.entries > 0;
    Ok(Merged {
        summary,
        rescanned_dirs,
    })
}

/// What becomes of a directory the walk is about to read.
#[derive(Debug, Clone, Copy)]
enum Decision {
    /// Read from disk; the saved directory it was, if the scan held it.
    Read(Option<u32>),
    /// Taken from the saved scan: the saved directory's id.
    Graft(u32),
}

/// A directory the walk listed and the saved scan fills.
#[derive(Debug)]
struct Grafted {
    dir: DirId,
    saved: u32,
    path: PathBuf,
}

/// The walk's provider for an incremental rescan: the platform's, which
/// reads every directory the plan says to read, and reads nothing of the
/// others, whose contents the saved scan gives.
struct Pruning<'a> {
    inner: &'a dyn Provider,
    plan: &'a Plan,
    /// What becomes of each directory the walk has listed, by its path.
    decisions: Mutex<HashMap<PathBuf, Decision>>,
    /// The saved directory of each directory being read, for the names its
    /// read left for later.
    reading: Mutex<HashMap<DirId, Option<u32>>>,
    grafts: Mutex<Vec<Grafted>>,
    reads: AtomicU64,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Pruning<'_> {
    /// Starts the read of `dir`, as decided when its parent was read: a
    /// directory the walk did not list (the root, one reached through a
    /// link) is read.
    fn start(&self, dir: &DirHandle) -> Decision {
        let decision = lock(&self.decisions)
            .remove(dir.path())
            .unwrap_or(Decision::Read(None));
        match decision {
            Decision::Graft(saved) => {
                lock(&self.grafts).push(Grafted {
                    dir: dir.id(),
                    saved,
                    path: dir.path().to_owned(),
                });
            }
            Decision::Read(saved) => {
                self.reads.fetch_add(1, Ordering::Relaxed);
                lock(&self.reading).insert(dir.id(), saved);
            }
        }
        decision
    }

    /// Decides for each subdirectory among `entries`, just read from `dir`
    /// (the saved directory `saved`), before the walk takes them.
    fn decide(&self, dir: &DirHandle, saved: Option<u32>, entries: &[Entry]) {
        let mut decided = Vec::new();
        for entry in entries.iter().filter(|e| e.kind == Kind::Dir) {
            let decision = match saved {
                Some(parent) => self.plan.decide(dir.path(), parent, entry),
                None => Decision::Read(None),
            };
            decided.push((dir.path().join(entry.name.as_os_str()), decision));
        }
        if !decided.is_empty() {
            lock(&self.decisions).extend(decided);
        }
    }
}

impl Plan {
    /// What becomes of `entry`, a subdirectory just read from the directory
    /// at `parent_path`, the saved directory `parent`.
    fn decide(&self, parent_path: &Path, parent: u32, entry: &Entry) -> Decision {
        let Some(saved) = self.saved.child(parent, entry.name.as_bytes()) else {
            return Decision::Read(None);
        };
        let read = Decision::Read(Some(saved.id));
        let Some(parent_rel) = relative(&self.root, parent_path) else {
            return read;
        };
        let rel = join(&parent_rel, entry.name.as_bytes());
        if self.changes.reads(&parent_rel, &rel) || !self.saved.entered(saved) {
            return read;
        }
        // Its times are its identity: another directory put in its place,
        // or its own entries changed, would differ.
        if (entry.mtime_ns, entry.ctime_ns, entry.dev) != (saved.mtime, saved.ctime, saved.dev) {
            return read;
        }
        Decision::Graft(saved.id)
    }
}

impl Provider for Pruning<'_> {
    fn read_dir(
        &self,
        dir: &DirHandle,
        opts: &WalkOptions,
        out: &mut Vec<Entry>,
        errs: &mut Vec<ScanError>,
    ) -> io::Result<()> {
        let Decision::Read(saved) = self.start(dir) else {
            return Ok(());
        };
        let from = out.len();
        let read = self.inner.read_dir(dir, opts, out, errs);
        self.decide(dir, saved, &out[from.min(out.len())..]);
        read
    }

    fn read_dir_split(
        &self,
        dir: &DirHandle,
        opts: &WalkOptions,
        out: &mut Vec<Entry>,
        errs: &mut Vec<ScanError>,
        later: &mut Vec<NameRef>,
    ) -> io::Result<()> {
        let Decision::Read(saved) = self.start(dir) else {
            return Ok(());
        };
        let from = out.len();
        let read = self.inner.read_dir_split(dir, opts, out, errs, later);
        self.decide(dir, saved, &out[from.min(out.len())..]);
        read
    }

    fn read_dir_streaming(
        &self,
        dir: &DirHandle,
        opts: &WalkOptions,
        out: &mut Vec<Entry>,
        errs: &mut Vec<ScanError>,
        later: &mut Vec<NameRef>,
        flush: &mut Flush<'_>,
    ) -> io::Result<()> {
        let Decision::Read(saved) = self.start(dir) else {
            return Ok(());
        };
        // The entries not yet decided start here: the walk takes every one
        // at a flush.
        let from = Cell::new(out.len());
        let mut decide_then_flush =
            |out: &mut Vec<Entry>, errs: &mut Vec<ScanError>, later: &mut Vec<NameRef>| {
                self.decide(dir, saved, &out[from.get().min(out.len())..]);
                flush(out, errs, later);
                from.set(out.len());
            };
        let read =
            self.inner
                .read_dir_streaming(dir, opts, out, errs, later, &mut decide_then_flush);
        self.decide(dir, saved, &out[from.get().min(out.len())..]);
        read
    }

    fn stat_names(
        &self,
        dir: &DirHandle,
        names: &[NameRef],
        opts: &WalkOptions,
        out: &mut Vec<Entry>,
        errs: &mut Vec<ScanError>,
    ) -> io::Result<()> {
        let from = out.len();
        let read = self.inner.stat_names(dir, names, opts, out, errs);
        let saved = lock(&self.reading).get(&dir.id()).copied().flatten();
        self.decide(dir, saved, &out[from.min(out.len())..]);
        read
    }

    fn times_listing(&self) -> bool {
        self.inner.times_listing()
    }

    fn skip(&self, dir: &DirHandle, entry: &Entry, opts: &WalkOptions) -> Option<Skip> {
        self.inner.skip(dir, entry, opts)
    }

    fn clones_unavailable(&self, opts: &WalkOptions) -> bool {
        self.inner.clones_unavailable(opts)
    }

    fn volume(&self, path: &Path) -> io::Result<VolumeInfo> {
        self.inner.volume(path)
    }

    fn mount_info(&self, path: &Path, dev: u64) -> io::Result<VolumeInfo> {
        self.inner.mount_info(path, dev)
    }

    fn root(&self, path: &Path, opts: &WalkOptions) -> io::Result<Entry> {
        self.inner.root(path, opts)
    }

    fn follow(&self, path: &Path, opts: &WalkOptions) -> io::Result<(PathBuf, Entry)> {
        self.inner.follow(path, opts)
    }

    fn cachedir_tag(&self, dir: &Path) -> bool {
        self.inner.cachedir_tag(dir)
    }

    fn default_threads(&self) -> usize {
        self.inner.default_threads()
    }
}

/// Where the merged scan's events go: the caller's sink, with each entered
/// volume marked at the plan's event id, and what the merge needs of them
/// counted on the way.
struct Tap<'s> {
    sink: &'s mut dyn FnMut(ScanEvent),
    history: &'s dyn History,
    event_id: u64,
    /// One past the highest directory id delivered.
    next_dir: u32,
    /// The mount ids of the volumes delivered.
    volumes: HashSet<u64>,
    /// Directories the excludes kept out, the walk's and the saved scan's.
    excluded_dirs: u64,
}

impl Tap<'_> {
    fn push(&mut self, mut event: ScanEvent) {
        match &mut event {
            ScanEvent::Entry(entry) => {
                if let Some(dir) = entry.dir {
                    self.next_dir = self.next_dir.max(dir.get().saturating_add(1));
                }
            }
            ScanEvent::Volume(volume) => {
                self.volumes.insert(volume.mnt_id);
                volume.fsevents = if volume.entered() {
                    self.history.mark(volume.dev, self.event_id)
                } else {
                    None
                };
            }
            ScanEvent::Skipped { reason, .. } => {
                if matches!(
                    reason,
                    Skip::Excluded | Skip::ExcludedCache | Skip::Containers
                ) {
                    self.excluded_dirs += 1;
                }
            }
            _ => {}
        }
        (self.sink)(event);
    }
}

/// What the saved scan gave the merge.
#[derive(Debug, Default)]
struct Counts {
    entries: u64,
    dirs: u64,
    unreadable_dirs: u64,
    entry_errors: u64,
    skipped_dirs: u64,
    volumes: u64,
    mounts_excluded: u64,
}

/// A grafted directory: its new id, path, device and mount id.
type Placed = (DirId, PathBuf, u64, u64);

/// Fills the directories the walk took from the saved scan with their
/// saved subtrees, in id order: pre-order, so a directory comes before
/// what it holds.
struct Grafting<'a, 's> {
    plan: &'a Plan,
    index: &'a Index,
    provider: &'a dyn Provider,
    opts: &'a WalkOptions,
    tap: &'a mut Tap<'s>,
    /// Each hard-linked file and clone member's id.
    shared: HashSet<u32>,
    /// What each directory listed but could not read, by its id.
    errors: HashMap<u32, Vec<(Vec<u8>, i32)>>,
    counts: Counts,
    /// The grafted directories the scan read, with their saved times, for
    /// the post-check.
    sampled: Vec<(PathBuf, i64, i64)>,
}

/// A `dirs` row below a grafted directory.
struct DirRow {
    id: u32,
    parent: u32,
    name: Vec<u8>,
    apparent: u64,
    allocated: u64,
    mtime: i64,
    ctime: i64,
    dev: u64,
    flags: u32,
    errors: Option<i32>,
    full: Option<FullAttrs>,
}

/// A `files` row below a grafted directory.
struct FileRow {
    id: u32,
    parent: u32,
    name: Vec<u8>,
    apparent: u64,
    allocated: u64,
    mtime: i64,
    ctime: i64,
    full: Option<FullAttrs>,
    nlink: u32,
    kind: String,
    flags: u32,
}

/// The `--attrs full` columns at `at` (atime, btime, mode, uid, gid): none
/// where the mode is null, as for an entry read without them.
fn full_at(row: &Row<'_>, at: usize) -> rusqlite::Result<Option<FullAttrs>> {
    let mode: Option<u32> = row.get(at + 2)?;
    let Some(mode) = mode else {
        return Ok(None);
    };
    Ok(Some(FullAttrs {
        atime_ns: row.get::<_, Option<i64>>(at)?.unwrap_or(0),
        btime_ns: row.get(at + 1)?,
        mode,
        uid: row.get::<_, Option<u32>>(at + 3)?.unwrap_or(0),
        gid: row.get::<_, Option<u32>>(at + 4)?.unwrap_or(0),
    }))
}

impl<'a, 's> Grafting<'a, 's> {
    fn new(
        plan: &'a Plan,
        index: &'a Index,
        provider: &'a dyn Provider,
        opts: &'a WalkOptions,
        tap: &'a mut Tap<'s>,
    ) -> Result<Grafting<'a, 's>, Stop> {
        let corrupt = |e: rusqlite::Error| {
            Stop::Index(CoreError::IndexCorrupt {
                file: index.file().display().to_string(),
                message: e.to_string(),
            })
        };
        let db = index.db();
        let shared = db
            .prepare("SELECT id FROM shared_members")
            .and_then(|mut stmt| {
                stmt.query_map([], |row| row.get::<_, u32>(0))?
                    .collect::<rusqlite::Result<HashSet<u32>>>()
            })
            .map_err(corrupt)?;
        let mut errors: HashMap<u32, Vec<(Vec<u8>, i32)>> = HashMap::new();
        db.prepare("SELECT dir_id, name, errno FROM scan_errors")
            .and_then(|mut stmt| {
                let rows = stmt.query_map([], |row| {
                    Ok((row.get::<_, u32>(0)?, row.get(1)?, row.get(2)?))
                })?;
                for row in rows {
                    let (dir, name, errno) = row?;
                    errors.entry(dir).or_default().push((name, errno));
                }
                Ok(())
            })
            .map_err(corrupt)?;
        Ok(Grafting {
            plan,
            index,
            provider,
            opts,
            tap,
            shared,
            errors,
            counts: Counts::default(),
            sampled: Vec::new(),
        })
    }

    fn corrupt(&self, e: &rusqlite::Error) -> Stop {
        Stop::Index(CoreError::IndexCorrupt {
            file: self.index.file().display().to_string(),
            message: e.to_string(),
        })
    }

    /// Delivers the saved subtree of one grafted directory.
    fn run(&mut self, grafted: &Grafted) -> Result<(), Stop> {
        let (plan, index): (&'a Plan, &'a Index) = (self.plan, self.index);
        let Some(top) = plan.saved.dir(grafted.saved) else {
            return Ok(());
        };
        let (first, last) = (top.id, top.last_id);
        let mut placed: HashMap<u32, Placed> = HashMap::new();
        let mnt_id = self.mnt_id(top.dev, None);
        placed.insert(first, (grafted.dir, grafted.path.clone(), top.dev, mnt_id));
        self.errors_of(first, grafted.dir);
        if last == first {
            return Ok(());
        }
        let dirs = self
            .dir_rows(first + 1, last)
            .map_err(|e| self.corrupt(&e))?;
        let db = index.db();
        let mut stmt = db
            .prepare(
                "SELECT id, parent_id, name, apparent, allocated, mtime, ctime, atime, btime, \
                 mode, uid, gid, nlink, kind, flags FROM files WHERE id BETWEEN ?1 AND ?2 \
                 ORDER BY id",
            )
            .map_err(|e| self.corrupt(&e))?;
        let mut rows = stmt
            .query([first + 1, last])
            .map_err(|e| self.corrupt(&e))?;
        let mut dirs = dirs.into_iter().peekable();
        while let Some(row) = rows.next().map_err(|e| self.corrupt(&e))? {
            let file = FileRow {
                id: row.get(0).map_err(|e| self.corrupt(&e))?,
                parent: row.get(1).map_err(|e| self.corrupt(&e))?,
                name: row.get(2).map_err(|e| self.corrupt(&e))?,
                apparent: row
                    .get::<_, i64>(3)
                    .map(unsigned)
                    .map_err(|e| self.corrupt(&e))?,
                allocated: row
                    .get::<_, i64>(4)
                    .map(unsigned)
                    .map_err(|e| self.corrupt(&e))?,
                mtime: row.get(5).map_err(|e| self.corrupt(&e))?,
                ctime: row.get(6).map_err(|e| self.corrupt(&e))?,
                full: full_at(row, 7).map_err(|e| self.corrupt(&e))?,
                nlink: row.get(12).map_err(|e| self.corrupt(&e))?,
                kind: row.get(13).map_err(|e| self.corrupt(&e))?,
                flags: row.get(14).map_err(|e| self.corrupt(&e))?,
            };
            while let Some(dir) = dirs.next_if(|d| d.id < file.id) {
                self.dir(&dir, &mut placed);
            }
            self.file(&file, &placed)?;
        }
        for dir in dirs {
            self.dir(&dir, &mut placed);
        }
        Ok(())
    }

    fn dir_rows(&self, first: u32, last: u32) -> rusqlite::Result<Vec<DirRow>> {
        let mut stmt = self.index.db().prepare(
            "SELECT id, parent_id, name, apparent, allocated, mtime, ctime, dev, flags, errors, \
             atime, btime, mode, uid, gid FROM dirs WHERE id BETWEEN ?1 AND ?2 ORDER BY id",
        )?;
        let rows = stmt.query_map([first, last], |row| {
            Ok(DirRow {
                id: row.get(0)?,
                parent: row.get(1)?,
                name: row.get(2)?,
                apparent: unsigned(row.get(3)?),
                allocated: unsigned(row.get(4)?),
                mtime: row.get(5)?,
                ctime: row.get(6)?,
                dev: unsigned(row.get(7)?),
                flags: row.get(8)?,
                errors: row.get(9)?,
                full: full_at(row, 10)?,
            })
        })?;
        rows.collect()
    }

    /// The mount id of the volume on `dev`: the saved scan's, else the
    /// parent's (`within`), else the device.
    fn mnt_id(&self, dev: u64, within: Option<u64>) -> u64 {
        self.plan
            .saved
            .volume(dev)
            .map_or_else(|| within.unwrap_or(dev), |v| v.mnt_id)
    }

    /// The entries `dir` listed but could not read.
    fn errors_of(&mut self, saved: u32, dir: DirId) {
        for (name, errno) in self.errors.remove(&saved).unwrap_or_default() {
            self.counts.entry_errors += 1;
            self.tap.push(ScanEvent::Error(ScanError {
                dir,
                name: NameRef::new(&name),
                errno,
            }));
        }
    }

    fn full(&self, full: Option<FullAttrs>) -> Option<FullAttrs> {
        (self.plan.saved.attrs == Attrs::Full)
            .then_some(full)
            .flatten()
    }

    /// Delivers a saved directory, and whatever its row says the scan did
    /// with it.
    fn dir(&mut self, row: &DirRow, placed: &mut HashMap<u32, Placed>) {
        let Some((parent, parent_path, parent_dev, parent_mnt)) = placed.get(&row.parent).cloned()
        else {
            return;
        };
        let path = parent_path.join(OsStr::from_bytes(&row.name));
        let crossing = row.dev != parent_dev;
        let boundary = match (crossing, row.flags & FIRMLINK != 0) {
            (false, _) => Boundary::None,
            (true, true) => Boundary::Firmlink,
            (true, false) => Boundary::Mount,
        };
        let mnt_id = if crossing {
            self.mnt_id(row.dev, None)
        } else {
            parent_mnt
        };
        let dir = DirId::new(self.tap.next_dir);
        self.tap.next_dir = self.tap.next_dir.saturating_add(1);
        self.counts.entries += 1;
        self.counts.dirs += 1;
        self.tap.push(ScanEvent::Entry(Entry {
            name: NameRef::new(&row.name),
            parent,
            dir: Some(dir),
            kind: Kind::Dir,
            apparent: row.apparent,
            allocated: row.allocated,
            mtime_ns: row.mtime,
            ctime_ns: row.ctime,
            nlink: 0,
            dev: row.dev,
            ino: 0,
            mnt_id,
            share: Share::None,
            boundary,
            flags: Flags::from_bits_truncate(row.flags & 0xff),
            full: self.full(row.full),
        }));
        let volume = crossing
            .then(|| self.volume(row.dev, mnt_id, &path))
            .flatten();
        let skip = if row.flags & SKIPPED_CONTAINERS != 0 {
            Some(Skip::Containers)
        } else if row.flags & EXCLUDED_CACHE != 0 {
            Some(Skip::ExcludedCache)
        } else if row.flags & EXCLUDED != 0 {
            Some(Skip::Excluded)
        } else {
            volume.filter(|v| !v.entered).map(|v| {
                v.reason
                    .as_deref()
                    .and_then(skip_named)
                    .unwrap_or(Skip::Kernel)
            })
        };
        if let Some(reason) = skip {
            self.counts.skipped_dirs += 1;
            self.tap.push(ScanEvent::Skipped { dir, reason });
            self.tap.push(ScanEvent::DirClosed { dir, errno: None });
        } else if let Some(errno) = row.errors {
            self.counts.unreadable_dirs += 1;
            self.tap.push(ScanEvent::DirClosed {
                dir,
                errno: Some(errno),
            });
        } else {
            self.sampled.push((path.clone(), row.mtime, row.ctime));
        }
        self.errors_of(row.id, dir);
        placed.insert(row.id, (dir, path, row.dev, mnt_id));
    }

    /// Delivers the volume on `dev` the first time a grafted directory
    /// crosses onto it, read as the walk reads a volume it meets; returns
    /// the saved scan's row of it.
    fn volume(&mut self, dev: u64, mnt_id: u64, path: &Path) -> Option<&'a SavedVolume> {
        let plan: &'a Plan = self.plan;
        let saved = plan.saved.volume(dev)?;
        if !self.tap.volumes.insert(mnt_id) {
            return Some(saved);
        }
        let read = if saved.entered {
            self.provider.volume(path)
        } else {
            self.provider.mount_info(path, dev)
        };
        let mut volume = read.unwrap_or_else(|_| VolumeInfo {
            dev,
            mnt_id,
            mount_point: PathBuf::from(OsStr::from_bytes(&saved.mount_point)),
            fs_type: saved.fs_type.clone(),
            class: VolumeClass::Local,
            read_only: saved.read_only,
            case_sensitive: saved.case_sensitive,
            normalization: if saved.normalization == "insensitive" {
                duscope_walk::Normalization::Insensitive
            } else {
                duscope_walk::Normalization::None
            },
            options: saved.options.clone(),
            total_bytes: saved.total_bytes.unwrap_or(0),
            free_bytes: saved.free_bytes.unwrap_or(0),
            reason: None,
            fsevents: None,
        });
        volume.dev = dev;
        volume.mnt_id = mnt_id;
        volume.reason = if saved.entered {
            None
        } else {
            Some(
                saved
                    .reason
                    .as_deref()
                    .and_then(skip_named)
                    .unwrap_or(Skip::Kernel),
            )
        };
        self.counts.volumes += 1;
        self.counts.mounts_excluded += u64::from(!saved.entered);
        self.tap.push(ScanEvent::Volume(volume));
        Some(saved)
    }

    /// Delivers a saved file: as the saved scan recorded it, or, for one
    /// that shares its inode or its blocks, read again.
    fn file(&mut self, row: &FileRow, placed: &HashMap<u32, Placed>) -> Result<(), Stop> {
        let Some((parent, parent_path, dev, mnt_id)) = placed.get(&row.parent) else {
            return Ok(());
        };
        let sharing = row.nlink > 1
            || self.shared.contains(&row.id)
            || row.flags & (Flags::MAY_SHARE | Flags::SHARES_ALL).bits() != 0;
        let entry = if sharing {
            let path = parent_path.join(OsStr::from_bytes(&row.name));
            let read = self.provider.root(&path, self.opts);
            match read {
                Ok(entry) if entry.kind != Kind::Dir && kind_name(entry.kind) == row.kind => {
                    Entry {
                        name: NameRef::new(&row.name),
                        parent: *parent,
                        dir: None,
                        ..entry
                    }
                }
                _ => {
                    return Err(Stop::fallback(
                        Fallback::PostCheckFailed,
                        format!(
                            "{} changed, but the history reports no change to its directory",
                            path.display()
                        ),
                    ));
                }
            }
        } else {
            Entry {
                name: NameRef::new(&row.name),
                parent: *parent,
                dir: None,
                kind: match row.kind.as_str() {
                    "file" => Kind::File,
                    "symlink" => Kind::Symlink,
                    _ => Kind::Unknown,
                },
                apparent: row.apparent,
                allocated: row.allocated,
                mtime_ns: row.mtime,
                ctime_ns: row.ctime,
                nlink: row.nlink,
                dev: *dev,
                ino: 0,
                mnt_id: *mnt_id,
                share: Share::None,
                boundary: Boundary::None,
                flags: Flags::from_bits_truncate(row.flags & 0xff),
                full: self.full(row.full),
            }
        };
        self.counts.entries += 1;
        self.tap.push(ScanEvent::Entry(entry));
        Ok(())
    }
}

/// The skip reason a `scan_volumes.reason` names.
fn skip_named(reason: &str) -> Option<Skip> {
    [
        Skip::Firmlinked,
        Skip::Kernel,
        Skip::Network,
        Skip::OneFileSystem,
        Skip::BindCycle,
        Skip::Excluded,
        Skip::ExcludedCache,
        Skip::Containers,
    ]
    .into_iter()
    .find(|skip| skip.as_str() == reason)
}

/// The post-check: a random 1% of the directories taken from the saved
/// scan, at least [`POST_CHECK_MIN`], read again and compared on `mtime`
/// and `ctime`. One that differs, or cannot be read, fails it.
fn post_check(sampled: &[(PathBuf, i64, i64)]) -> Result<(), Stop> {
    let started = Instant::now();
    let want = sampled
        .len()
        .div_ceil(100)
        .max(POST_CHECK_MIN.min(sampled.len()));
    let picked = sample(sampled.len(), want);
    let stamps = stamp_dirs(picked.iter().map(|&i| sampled[i].0.as_path()), false);
    for (&i, stamped) in picked.iter().zip(stamps) {
        let (path, mtime, ctime) = &sampled[i];
        let same = stamped.is_ok_and(|s| s.mtime_ns == *mtime && s.ctime_ns == *ctime);
        if !same {
            return Err(Stop::fallback(
                Fallback::PostCheckFailed,
                format!(
                    "{} changed, but the history reports no change to it",
                    path.display()
                ),
            ));
        }
    }
    log::info!(
        "post-check: {} of {} directories taken from the saved scan read again in {:.3} s",
        picked.len(),
        sampled.len(),
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

/// `want` distinct indexes below `len`, at random, in order.
fn sample(len: usize, want: usize) -> Vec<usize> {
    if want >= len {
        return (0..len).collect();
    }
    let seed = crate::index::random_bytes().map_or_else(
        |_| {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default();
            u64::from(now.subsec_nanos()) ^ now.as_secs()
        },
        u64::from_le_bytes,
    );
    // xorshift64*, enough to spread a sample.
    let mut state = seed | 1;
    let mut next = || {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        state.wrapping_mul(0x2545_f491_4f6c_dd1d)
    };
    // Floyd's algorithm: `want` distinct picks in `want` draws.
    let mut picked = HashSet::with_capacity(want);
    for j in len - want..len {
        let bound = u64::try_from(j + 1).unwrap_or(u64::MAX);
        let t = usize::try_from(next() % bound).unwrap_or(j);
        if !picked.insert(t) {
            picked.insert(j);
        }
    }
    let mut picked: Vec<usize> = picked.into_iter().collect();
    picked.sort_unstable();
    picked
}

#[cfg(test)]
mod tests;
