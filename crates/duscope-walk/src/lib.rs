//! The duscope scanner: a work-stealing directory walker and the per-platform
//! providers that fill its entry records.
//!
//! [`walk`] reads a tree with this platform's provider and delivers
//! [`ScanEvent`]s to a sink on the calling thread; [`walk_with`] does the same
//! with any [`Provider`], such as the in-memory [`MemProvider`] tests use. One
//! directory read is the unit of parallel work. The worker pool is vendored
//! from dua-core (`scheduler.rs`); the providers are duscope's own.
//!
//! This crate is an implementation detail of the `duscope` binary and promises
//! no API stability before 1.0.

// Denied rather than forbidden so that `src/platform/` alone can opt back in
// for its FFI; every other crate forbids `unsafe` outright.
#![deny(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod entry;
mod errors;
mod mem;
mod options;
mod platform;
mod scheduler;
#[cfg(test)]
mod tests;

use std::collections::HashSet;
use std::io;
use std::num::NonZeroUsize;
use std::panic;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

use crossbeam_deque::Worker;

pub use entry::{
    Boundary, DirId, Entry, Flags, FullAttrs, Kind, NameRef, Normalization, ScanEvent, Share, Skip,
    VolumeClass, VolumeInfo,
};
pub use errors::{ScanError, WalkError};
pub use mem::MemProvider;
pub use options::{Attrs, CacheRule, WalkOptions};

use scheduler::PoolShared;

/// Reads directories for a walk: one implementation per platform, plus the
/// fallback and [`MemProvider`].
pub trait Provider: Send + Sync {
    /// Reads `dir`, appending an entry to `out` for each child and an error to
    /// `errs` for each child that was listed but could not be read. The walk
    /// sets each entry's `parent` and `dir` and each error's `dir`.
    ///
    /// An `Err` means the directory itself could not be read: the walk
    /// discards whatever was appended and closes the directory with the
    /// error's number.
    fn read_dir(
        &self,
        dir: &DirHandle,
        opts: &WalkOptions,
        out: &mut Vec<Entry>,
        errs: &mut Vec<ScanError>,
    ) -> io::Result<()>;

    /// Reads `dir` as [`read_dir`](Provider::read_dir) does, but may list
    /// some children by name alone and leave them in `later`. The walk reads
    /// those with [`stat_names`](Provider::stat_names) in batches of 1,024
    /// that any worker can take, so one wide directory keeps every worker
    /// busy. The default leaves nothing for later.
    fn read_dir_split(
        &self,
        dir: &DirHandle,
        opts: &WalkOptions,
        out: &mut Vec<Entry>,
        errs: &mut Vec<ScanError>,
        later: &mut Vec<NameRef>,
    ) -> io::Result<()> {
        let _ = later;
        self.read_dir(dir, opts, out, errs)
    }

    /// Reads `names`, children of `dir` that
    /// [`read_dir_split`](Provider::read_dir_split) left for later, appending
    /// an entry or an error for each. An `Err` means `dir` itself could not be
    /// opened, and the walk records the error against every name.
    ///
    /// A provider that leaves nothing for later need not implement it; the
    /// default fails with `ErrorKind::Unsupported`.
    fn stat_names(
        &self,
        dir: &DirHandle,
        names: &[NameRef],
        opts: &WalkOptions,
        out: &mut Vec<Entry>,
        errs: &mut Vec<ScanError>,
    ) -> io::Result<()> {
        let _ = (dir, names, opts, out, errs);
        Err(io::ErrorKind::Unsupported.into())
    }

    /// Whether the walk lists `entry`, a directory just read from `dir`,
    /// without entering it, and why. The default enters every directory.
    fn skip(&self, dir: &DirHandle, entry: &Entry, opts: &WalkOptions) -> Option<Skip> {
        let _ = (dir, entry, opts);
        None
    }

    /// Whether `--clones` was asked of a filesystem that has no clone ids,
    /// for the `clones_unavailable` warning; asked once the walk is done.
    /// The default: never.
    fn clones_unavailable(&self, opts: &WalkOptions) -> bool {
        let _ = opts;
        false
    }

    /// The volume holding `path`.
    fn volume(&self, path: &Path) -> io::Result<VolumeInfo>;

    /// The entry of the walk root itself, not following a final symlink. Its
    /// name is the path's last component, or the whole path when it has none
    /// (`/`).
    fn root(&self, path: &Path, opts: &WalkOptions) -> io::Result<Entry>;

    /// The workers a walk runs when [`WalkOptions::threads`] is 0: twice the
    /// logical CPUs, clamped to 4..=32, until the phase-1 thread sweep sets
    /// each platform's default.
    fn default_threads(&self) -> usize {
        let cpus = thread::available_parallelism().map_or(1, NonZeroUsize::get);
        cpus.saturating_mul(2).clamp(4, 32)
    }
}

/// A directory the walk asks a provider to read. It keeps its parent's
/// handle, so the directories on its path can be walked back to the root.
#[derive(Debug)]
pub struct DirHandle {
    id: DirId,
    path: PathBuf,
    depth: u32,
    dev: u64,
    ino: u64,
    mnt_id: u64,
    parent: Option<Arc<DirHandle>>,
}

impl DirHandle {
    /// The handle of `entry`, a directory numbered `id`, inside `parent`.
    fn new(id: DirId, path: PathBuf, entry: &Entry, parent: Option<Arc<DirHandle>>) -> DirHandle {
        DirHandle {
            id,
            path,
            depth: parent.as_ref().map_or(0, |p| p.depth + 1),
            dev: entry.dev,
            ino: entry.ino,
            mnt_id: entry.mnt_id,
            parent,
        }
    }

    /// A handle for tests: `path`, `depth` levels below a root it does not
    /// link to.
    #[cfg(all(test, target_os = "macos"))]
    pub(crate) fn detached(path: impl Into<PathBuf>, depth: u32) -> DirHandle {
        DirHandle {
            id: DirId::ROOT,
            path: path.into(),
            depth,
            dev: 0,
            ino: 0,
            mnt_id: 0,
            parent: None,
        }
    }

    /// The directory's device, as its entry reported it.
    pub fn dev(&self) -> u64 {
        self.dev
    }

    /// The directory's inode number, as its entry reported it.
    pub fn ino(&self) -> u64 {
        self.ino
    }

    /// The directory's mount id, as its entry reported it.
    pub fn mnt_id(&self) -> u64 {
        self.mnt_id
    }

    /// The parent directory's handle; `None` for the root.
    pub fn parent(&self) -> Option<&DirHandle> {
        self.parent.as_deref()
    }

    /// This directory and every directory above it, up to the root.
    pub fn ancestors(&self) -> impl Iterator<Item = &DirHandle> {
        std::iter::successors(Some(self), |dir| dir.parent())
    }

    /// The directory's id.
    pub fn id(&self) -> DirId {
        self.id
    }

    /// The directory's path: the root as given, joined with the names below
    /// it.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// How far below the root the directory is; the root is 0.
    pub fn depth(&self) -> u32 {
        self.depth
    }
}

/// What a completed walk counted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WalkSummary {
    /// Entries delivered, the root included.
    pub entries: u64,
    /// Directories among them.
    pub dirs: u64,
    /// Directories that could not be read.
    pub unreadable_dirs: u64,
    /// Entries listed but not read ([`ScanEvent::Error`]).
    pub entry_errors: u64,
    /// Directories listed but not entered ([`ScanEvent::Skipped`]).
    pub skipped_dirs: u64,
    /// Volumes met ([`ScanEvent::Volume`]), the root's included.
    pub volumes: u64,
    /// Volumes met but not entered, for the `mounts_excluded` warning.
    pub mounts_excluded: u64,
    /// `--clones` met a filesystem without clone ids
    /// ([`Provider::clones_unavailable`]).
    pub clones_unavailable: bool,
    /// The worker threads the walk ran.
    pub threads: usize,
}

/// Walks `root` with this platform's provider, or the fallback provider
/// under [`WalkOptions::fallback`], delivering every entry to `sink`.
/// [`walk_with`] walks with a given provider.
///
/// Returns [`WalkError::NoProvider`] on a platform other than Linux and
/// macOS.
pub fn walk(
    root: &Path,
    opts: &WalkOptions,
    sink: impl FnMut(ScanEvent),
) -> Result<WalkSummary, WalkError> {
    let provider = if opts.fallback {
        platform::fallback()
    } else {
        platform::native()
    };
    let provider = provider.ok_or(WalkError::NoProvider)?;
    walk_with(&*provider, root, opts, sink)
}

/// The result queue holds this many directory batches per worker, as in
/// dua-core; a worker blocks on a full queue until the sink catches up.
const QUEUED_BATCHES_PER_WORKER: usize = 2;

/// How long the sink's thread waits for a batch before it checks the
/// cancellation flag and the workers again.
const POLL: Duration = Duration::from_millis(50);

/// Reported for a directory whose read failed without an OS error number.
/// `EIO` is 5 on Linux and macOS alike.
pub(crate) const EIO: i32 = 5;

/// The names one [`Job::Stat`] reads: the Linux floor's batch size, which
/// spreads a wide directory's per-entry calls over every worker.
pub(crate) const STAT_BATCH: usize = 1024;

/// A job for the pool.
enum Job {
    /// Read one directory.
    ReadDir(Arc<DirHandle>),
    /// Read a batch of names a directory's read left for later.
    Stat(Arc<Split>, Vec<NameRef>),
}

/// A directory whose read left names for [`Job::Stat`]s.
struct Split {
    dir: Arc<DirHandle>,
    /// The directory's read plus its stat jobs not yet done; the part that
    /// takes it to 0 closes the directory.
    parts: AtomicUsize,
}

/// What workers send to the sink's thread.
enum Event {
    /// Entries and errors of one directory, which may also close it.
    Batch(Batch),
    /// Every directory has been read.
    Finished,
}

struct Batch {
    dir: DirId,
    entries: Vec<Entry>,
    errors: Vec<ScanError>,
    /// Volumes first met among `entries`.
    volumes: Vec<VolumeInfo>,
    /// Directories among `entries` the walk does not enter.
    skipped: Vec<(DirId, Skip)>,
    /// Whether this is the directory's last batch.
    closes: bool,
    /// Why the directory could not be read; only on a closing batch.
    errno: Option<i32>,
}

/// The state the workers share, beside the pool's own.
struct Walk<'a, P: ?Sized> {
    provider: &'a P,
    opts: &'a WalkOptions,
    /// Jobs queued or running; the worker that takes it to 0 sends
    /// [`Event::Finished`] (upstream's `Root::pending`).
    pending: AtomicUsize,
    /// The next directory id; the root has 0.
    next_dir: AtomicU32,
    /// Set when the ids ran out.
    out_of_ids: AtomicBool,
    /// The root's mount id, which `-x` confines the walk to.
    root_mnt_id: u64,
    /// The mount ids of the volumes met so far, each reported once.
    volumes: Mutex<HashSet<u64>>,
}

/// Walks `root` with `provider`, delivering every entry to `sink` on this
/// thread. See [`ScanEvent`] for the order of events.
///
/// Workers poll [`WalkOptions::cancel`] before each directory; once it is set
/// the walk stops and returns [`WalkError::Cancelled`]. A panic in a worker
/// is resumed on this thread.
pub fn walk_with<P: Provider + ?Sized>(
    provider: &P,
    root: &Path,
    opts: &WalkOptions,
    mut sink: impl FnMut(ScanEvent),
) -> Result<WalkSummary, WalkError> {
    let mut entry = provider
        .root(root, opts)
        .map_err(|source| WalkError::Root {
            path: root.to_owned(),
            source,
        })?;
    let is_dir = entry.kind == Kind::Dir;
    entry.parent = DirId::ROOT;
    entry.dir = is_dir.then_some(DirId::ROOT);
    let threads = match opts.threads {
        0 => provider.default_threads().max(1),
        n => n,
    };
    let mut summary = WalkSummary {
        entries: 1,
        dirs: u64::from(is_dir),
        threads,
        ..WalkSummary::default()
    };
    let root_dir = DirHandle::new(DirId::ROOT, root.to_owned(), &entry, None);
    let root_mnt_id = entry.mnt_id;
    sink(ScanEvent::Entry(entry));
    if let Ok(mut volume) = provider.volume(root) {
        volume.mnt_id = root_mnt_id;
        volume.entered = true;
        volume.skip = None;
        summary.volumes += 1;
        sink(ScanEvent::Volume(volume));
    }
    if !is_dir {
        return Ok(summary);
    }
    if opts.cancel.load(Ordering::Relaxed) {
        return Err(WalkError::Cancelled);
    }

    let state = Walk {
        provider,
        opts,
        pending: AtomicUsize::new(1),
        next_dir: AtomicU32::new(1),
        out_of_ids: AtomicBool::new(false),
        root_mnt_id,
        volumes: Mutex::new(HashSet::from([root_mnt_id])),
    };
    let run = |job: Job, worker: &Worker<Job>, shared: &PoolShared<Job, Event>| {
        state.run(job, worker, shared);
    };
    thread::scope(|scope| {
        let pool = scheduler::start_pool(scope, threads, QUEUED_BATCHES_PER_WORKER, &run)
            .map_err(WalkError::Spawn)?;
        pool.inject(Job::ReadDir(Arc::new(root_dir)));
        let finished = loop {
            match pool.events.recv_timeout(POLL) {
                Ok(Event::Batch(batch)) => deliver(batch, &mut summary, &mut sink),
                Ok(Event::Finished) => break true,
                Err(RecvTimeoutError::Timeout) if !pool.any_finished() => {}
                // A worker that exits in a running pool has panicked; `finish` resumes it.
                Err(_) => break false,
            }
            if opts.cancel.load(Ordering::Relaxed) || state.out_of_ids.load(Ordering::Relaxed) {
                break false;
            }
        };
        if let Err(worker_panic) = pool.finish() {
            panic::resume_unwind(worker_panic);
        }
        if finished {
            summary.clones_unavailable = opts.clones && provider.clones_unavailable(opts);
            Ok(summary)
        } else if state.out_of_ids.load(Ordering::Relaxed) {
            Err(WalkError::TooManyDirectories)
        } else {
            Err(WalkError::Cancelled)
        }
    })
}

/// Hands one directory's batch to the sink: its entries, its errors, then
/// its close.
fn deliver(batch: Batch, summary: &mut WalkSummary, sink: &mut impl FnMut(ScanEvent)) {
    summary.entries += batch.entries.len() as u64;
    summary.entry_errors += batch.errors.len() as u64;
    for entry in batch.entries {
        summary.dirs += u64::from(entry.dir.is_some());
        sink(ScanEvent::Entry(entry));
    }
    for error in batch.errors {
        sink(ScanEvent::Error(error));
    }
    for volume in batch.volumes {
        summary.volumes += 1;
        summary.mounts_excluded += u64::from(!volume.entered);
        sink(ScanEvent::Volume(volume));
    }
    summary.skipped_dirs += batch.skipped.len() as u64;
    for (dir, reason) in batch.skipped {
        sink(ScanEvent::Skipped { dir, reason });
        sink(ScanEvent::DirClosed { dir, errno: None });
    }
    if batch.closes {
        summary.unreadable_dirs += u64::from(batch.errno.is_some());
        sink(ScanEvent::DirClosed {
            dir: batch.dir,
            errno: batch.errno,
        });
    }
}

impl<P: Provider + ?Sized> Walk<'_, P> {
    fn run(&self, job: Job, worker: &Worker<Job>, shared: &PoolShared<Job, Event>) {
        if self.opts.cancel.load(Ordering::Relaxed) {
            shared.stop();
            return;
        }
        match job {
            Job::ReadDir(dir) => self.read_dir(&dir, worker, shared),
            Job::Stat(split, names) => self.stat(&split, names, worker, shared),
        }
        if self.pending.fetch_sub(1, Ordering::AcqRel) == 1 {
            shared.events.send(Event::Finished).ok();
        }
    }

    /// Reads one directory, sends its batch, then makes its subdirectories
    /// and any stat jobs stealable: parent first, as upstream's
    /// `Order::ParentFirst`, so the sink sees a directory's entry before
    /// anything inside it.
    fn read_dir(
        &self,
        dir: &Arc<DirHandle>,
        worker: &Worker<Job>,
        shared: &PoolShared<Job, Event>,
    ) {
        let (mut entries, mut errors, mut later) = (Vec::new(), Vec::new(), Vec::new());
        let errno = match self.provider.read_dir_split(
            dir,
            self.opts,
            &mut entries,
            &mut errors,
            &mut later,
        ) {
            Ok(()) => None,
            Err(e) => {
                entries.clear();
                errors.clear();
                later.clear();
                Some(e.raw_os_error().unwrap_or(EIO))
            }
        };
        let mut jobs = Vec::new();
        let Some((volumes, skipped)) =
            self.adopt(dir, &mut entries, &mut errors, &mut jobs, shared)
        else {
            return;
        };
        let split = (!later.is_empty()).then(|| {
            let split = Arc::new(Split {
                dir: Arc::clone(dir),
                parts: AtomicUsize::new(later.len().div_ceil(STAT_BATCH) + 1),
            });
            while !later.is_empty() {
                let rest = later.split_off(later.len().min(STAT_BATCH));
                jobs.push(Job::Stat(Arc::clone(&split), later));
                later = rest;
            }
            split
        });
        let batch = Batch {
            dir: dir.id,
            entries,
            errors,
            volumes,
            skipped,
            closes: split.is_none(),
            errno,
        };
        self.send(batch, jobs, worker, shared);
        if let Some(split) = split {
            Self::finish_part(&split, shared);
        }
    }

    /// Reads a batch of names a directory's read left for later.
    fn stat(
        &self,
        split: &Split,
        names: Vec<NameRef>,
        worker: &Worker<Job>,
        shared: &PoolShared<Job, Event>,
    ) {
        let (mut entries, mut errors) = (Vec::new(), Vec::new());
        if let Err(e) =
            self.provider
                .stat_names(&split.dir, &names, self.opts, &mut entries, &mut errors)
        {
            entries.clear();
            let errno = e.raw_os_error().unwrap_or(EIO);
            errors = names
                .into_iter()
                .map(|name| ScanError {
                    dir: split.dir.id,
                    name,
                    errno,
                })
                .collect();
        }
        let mut jobs = Vec::new();
        let Some((volumes, skipped)) =
            self.adopt(&split.dir, &mut entries, &mut errors, &mut jobs, shared)
        else {
            return;
        };
        let batch = Batch {
            dir: split.dir.id,
            entries,
            errors,
            volumes,
            skipped,
            closes: false,
            errno: None,
        };
        self.send(batch, jobs, worker, shared);
        Self::finish_part(split, shared);
    }

    /// Sets the parent of `dir`'s new entries and errors, numbers its
    /// subdirectories and queues a read of each the walk enters. Returns the
    /// volumes first met and the directories not entered, or `None` when the
    /// ids ran out, which stops the walk.
    #[allow(clippy::type_complexity)]
    fn adopt(
        &self,
        dir: &Arc<DirHandle>,
        entries: &mut [Entry],
        errors: &mut [ScanError],
        jobs: &mut Vec<Job>,
        shared: &PoolShared<Job, Event>,
    ) -> Option<(Vec<VolumeInfo>, Vec<(DirId, Skip)>)> {
        let (mut volumes, mut skipped) = (Vec::new(), Vec::new());
        for entry in entries {
            entry.parent = dir.id;
            entry.dir = None;
            if entry.kind == Kind::Dir {
                let Some(id) = self.allocate_dir() else {
                    self.out_of_ids.store(true, Ordering::Relaxed);
                    shared.stop();
                    return None;
                };
                entry.dir = Some(id);
                let path = dir.path.join(entry.name.as_os_str());
                let skip = self
                    .cross(dir, entry, &path, &mut volumes)
                    .or_else(|| self.provider.skip(dir, entry, self.opts));
                if let Some(reason) = skip {
                    skipped.push((id, reason));
                    continue;
                }
                jobs.push(Job::ReadDir(Arc::new(DirHandle::new(
                    id,
                    path,
                    entry,
                    Some(Arc::clone(dir)),
                ))));
            }
        }
        for error in errors {
            error.dir = dir.id;
        }
        Some((volumes, skipped))
    }

    /// Decides whether the walk enters `entry`, a directory of `dir` at
    /// `path`, when it lies on another mount: a kernel pseudo-filesystem
    /// never, a network one only under `--include-network`, any other mount
    /// than the root's not under `-x`, and a mount of a directory already on
    /// the path never. The volume is added to `volumes` the first time it is
    /// met. `None` enters.
    fn cross(
        &self,
        dir: &DirHandle,
        entry: &Entry,
        path: &Path,
        volumes: &mut Vec<VolumeInfo>,
    ) -> Option<Skip> {
        if entry.mnt_id == dir.mnt_id && entry.boundary != Boundary::Mount {
            return None;
        }
        let cycle = dir
            .ancestors()
            .any(|a| (a.dev, a.ino) == (entry.dev, entry.ino));
        let volume = self.provider.volume(path).ok();
        let skip = match volume.as_ref().map(|v| v.class) {
            _ if cycle => Some(Skip::BindCycle),
            Some(VolumeClass::Kernel) => Some(Skip::Kernel),
            Some(VolumeClass::Network) if !self.opts.include_network => Some(Skip::Network),
            _ if self.opts.one_file_system && entry.mnt_id != self.root_mnt_id => {
                Some(Skip::OneFileSystem)
            }
            _ => None,
        };
        if let Some(mut volume) = volume {
            let first = self
                .volumes
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(entry.mnt_id);
            if first {
                volume.mnt_id = entry.mnt_id;
                volume.entered = skip.is_none();
                volume.skip = skip;
                volumes.push(volume);
            }
        }
        skip
    }

    /// Sends a batch, then makes the jobs it found stealable.
    fn send(
        &self,
        batch: Batch,
        jobs: Vec<Job>,
        worker: &Worker<Job>,
        shared: &PoolShared<Job, Event>,
    ) {
        self.pending.fetch_add(jobs.len(), Ordering::Relaxed);
        if shared.events.send(Event::Batch(batch)).is_err() {
            shared.stop();
            return;
        }
        scheduler::schedule_jobs(jobs, worker, shared);
    }

    /// Marks one part of a split directory done, after its batch was sent;
    /// the last part closes the directory, so the close follows every entry.
    fn finish_part(split: &Split, shared: &PoolShared<Job, Event>) {
        if split.parts.fetch_sub(1, Ordering::AcqRel) == 1 {
            let close = Batch {
                dir: split.dir.id,
                entries: Vec::new(),
                errors: Vec::new(),
                volumes: Vec::new(),
                skipped: Vec::new(),
                closes: true,
                errno: None,
            };
            if shared.events.send(Event::Batch(close)).is_err() {
                shared.stop();
            }
        }
    }

    fn allocate_dir(&self) -> Option<DirId> {
        self.next_dir
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .ok()
            .map(DirId::new)
    }
}
