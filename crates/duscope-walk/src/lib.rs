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
mod scheduler;
#[cfg(test)]
mod tests;

use std::io;
use std::num::NonZeroUsize;
use std::panic;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::thread;
use std::time::Duration;

use crossbeam_deque::Worker;

pub use entry::{
    Boundary, DirId, Entry, Flags, FullAttrs, Kind, NameRef, ScanEvent, Share, VolumeInfo,
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

/// A directory the walk asks a provider to read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirHandle {
    id: DirId,
    path: PathBuf,
    depth: u32,
}

impl DirHandle {
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
    /// The worker threads the walk ran.
    pub threads: usize,
}

/// Walks `root` with this platform's provider, delivering every entry to
/// `sink`.
///
/// No provider is built yet: the Linux and macOS providers arrive with
/// P1-WALK-02 and P1-WALK-03, and until then this returns
/// [`WalkError::NoProvider`]. [`walk_with`] walks with a given provider.
pub fn walk(
    root: &Path,
    opts: &WalkOptions,
    sink: impl FnMut(ScanEvent),
) -> Result<WalkSummary, WalkError> {
    let _ = (root, opts, sink);
    Err(WalkError::NoProvider)
}

/// The result queue holds this many directory batches per worker, as in
/// dua-core; a worker blocks on a full queue until the sink catches up.
const QUEUED_BATCHES_PER_WORKER: usize = 2;

/// How long the sink's thread waits for a batch before it checks the
/// cancellation flag and the workers again.
const POLL: Duration = Duration::from_millis(50);

/// Reported for a directory whose read failed without an OS error number.
/// `EIO` is 5 on Linux and macOS alike.
const EIO: i32 = 5;

/// A job for the pool: read one directory.
enum Job {
    ReadDir(DirHandle),
}

/// What workers send to the sink's thread.
enum Event {
    /// One directory's entries and errors, which also closes it.
    Batch(Batch),
    /// Every directory has been read.
    Finished,
}

struct Batch {
    dir: DirId,
    entries: Vec<Entry>,
    errors: Vec<ScanError>,
    errno: Option<i32>,
}

/// The state the workers share, beside the pool's own.
struct Walk<'a, P: ?Sized> {
    provider: &'a P,
    opts: &'a WalkOptions,
    /// Directories queued or being read; the worker that takes it to 0 sends
    /// [`Event::Finished`] (upstream's `Root::pending`).
    pending: AtomicUsize,
    /// The next directory id; the root has 0.
    next_dir: AtomicU32,
    /// Set when the ids ran out.
    out_of_ids: AtomicBool,
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
    sink(ScanEvent::Entry(entry));
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
    };
    let run = |job: Job, worker: &Worker<Job>, shared: &PoolShared<Job, Event>| {
        state.run(job, worker, shared);
    };
    thread::scope(|scope| {
        let pool = scheduler::start_pool(scope, threads, QUEUED_BATCHES_PER_WORKER, &run)
            .map_err(WalkError::Spawn)?;
        pool.inject(Job::ReadDir(DirHandle {
            id: DirId::ROOT,
            path: root.to_owned(),
            depth: 0,
        }));
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
    summary.unreadable_dirs += u64::from(batch.errno.is_some());
    for entry in batch.entries {
        summary.dirs += u64::from(entry.dir.is_some());
        sink(ScanEvent::Entry(entry));
    }
    for error in batch.errors {
        sink(ScanEvent::Error(error));
    }
    sink(ScanEvent::DirClosed {
        dir: batch.dir,
        errno: batch.errno,
    });
}

impl<P: Provider + ?Sized> Walk<'_, P> {
    /// Reads one directory, sends its batch, then makes its subdirectories
    /// stealable: parent first, as upstream's `Order::ParentFirst`, so the
    /// sink sees a directory's entry before anything inside it.
    fn run(&self, job: Job, worker: &Worker<Job>, shared: &PoolShared<Job, Event>) {
        let Job::ReadDir(dir) = job;
        if self.opts.cancel.load(Ordering::Relaxed) {
            shared.stop();
            return;
        }
        let (mut entries, mut errors) = (Vec::new(), Vec::new());
        let errno = match self
            .provider
            .read_dir(&dir, self.opts, &mut entries, &mut errors)
        {
            Ok(()) => None,
            Err(e) => {
                entries.clear();
                errors.clear();
                Some(e.raw_os_error().unwrap_or(EIO))
            }
        };
        let mut jobs = Vec::new();
        for entry in &mut entries {
            entry.parent = dir.id;
            entry.dir = None;
            if entry.kind == Kind::Dir {
                let Some(id) = self.allocate_dir() else {
                    self.out_of_ids.store(true, Ordering::Relaxed);
                    shared.stop();
                    return;
                };
                entry.dir = Some(id);
                jobs.push(Job::ReadDir(DirHandle {
                    id,
                    path: dir.path.join(entry.name.as_os_str()),
                    depth: dir.depth + 1,
                }));
            }
        }
        for error in &mut errors {
            error.dir = dir.id;
        }
        self.pending.fetch_add(jobs.len(), Ordering::Relaxed);
        let batch = Batch {
            dir: dir.id,
            entries,
            errors,
            errno,
        };
        if shared.events.send(Event::Batch(batch)).is_err() {
            shared.stop();
            return;
        }
        scheduler::schedule_jobs(jobs, worker, shared);
        if self.pending.fetch_sub(1, Ordering::AcqRel) == 1 {
            shared.events.send(Event::Finished).ok();
        }
    }

    fn allocate_dir(&self) -> Option<DirId> {
        self.next_dir
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .ok()
            .map(DirId::new)
    }
}
