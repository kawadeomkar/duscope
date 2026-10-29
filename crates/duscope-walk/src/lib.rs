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
mod excludes;
mod mem;
mod options;
mod platform;
mod scheduler;
#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};
use std::io;
use std::num::NonZeroUsize;
use std::panic;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicUsize, Ordering};
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
pub use excludes::IgnoreRules;
pub use mem::MemProvider;
pub use options::{Attrs, CacheRule, WalkOptions};

use excludes::Excludes;
use scheduler::PoolShared;

/// Reads directories for a walk: one implementation per platform, plus the
/// fallback and [`MemProvider`].
pub trait Provider: Send + Sync {
    /// Reads `dir`, appending an entry to `out` for each child and an error to
    /// `errs` for each child that was listed but could not be read. The walk
    /// sets each entry's `parent` and `dir` and each error's `dir`.
    ///
    /// # Errors
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
    ///
    /// # Errors
    ///
    /// The error of reading the directory itself, as for
    /// [`read_dir`](Provider::read_dir).
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

    /// Reads `dir` as [`read_dir_split`](Provider::read_dir_split) does, and
    /// may hand the walk what it has read so far by calling `flush` before
    /// the directory is read whole, so a directory of a million entries is
    /// never held whole. At each call the walk takes everything in `out` and
    /// `errs`, and the names in `later` in whole batches of 1,024; the rest
    /// stay in `later` for the next call or the end of the read. An `Err`
    /// after a flush leaves what was flushed counted: the directory closes
    /// with the error, or is reported vanished, after some of its entries.
    /// The default reads the directory whole.
    fn read_dir_streaming(
        &self,
        dir: &DirHandle,
        opts: &WalkOptions,
        out: &mut Vec<Entry>,
        errs: &mut Vec<ScanError>,
        later: &mut Vec<NameRef>,
        flush: &mut Flush<'_>,
    ) -> io::Result<()> {
        let _ = flush;
        self.read_dir_split(dir, opts, out, errs, later)
    }

    /// Reads `names`, children of `dir` that
    /// [`read_dir_split`](Provider::read_dir_split) left for later, appending
    /// an entry or an error for each. An `Err` means `dir` itself could not be
    /// opened, and the walk records the error against every name.
    ///
    /// A provider that leaves nothing for later need not implement it; the
    /// default fails with `ErrorKind::Unsupported`.
    ///
    /// # Errors
    ///
    /// The error of opening `dir` again; a name that cannot be read is an
    /// error in `errs`, not an `Err`.
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
    ///
    /// # Errors
    ///
    /// The error of reading the volume of `path`.
    fn volume(&self, path: &Path) -> io::Result<VolumeInfo>;

    /// The volume mounted at `path`, whose entry reports the device `dev`,
    /// read where the platform can without touching the mounted filesystem:
    /// the walk classifies a mount with it before deciding whether to
    /// enter, so a dead network mount it leaves alone is never asked for
    /// its sizes. A provider may find the mount by `dev`, which holds
    /// however the walk spelled the path. The default is
    /// [`volume`](Provider::volume).
    ///
    /// # Errors
    ///
    /// The error of reading the mount table, or the mount point when the
    /// table does not have it.
    fn mount_info(&self, path: &Path, dev: u64) -> io::Result<VolumeInfo> {
        let _ = dev;
        self.volume(path)
    }

    /// The entry of the walk root itself, not following a final symlink. Its
    /// name is the path's last component, or the whole path when it has none
    /// (`/`).
    ///
    /// # Errors
    ///
    /// The error of reading `path`, which ends the walk before it starts.
    fn root(&self, path: &Path, opts: &WalkOptions) -> io::Result<Entry>;

    /// Where the symbolic link at `path` leads, every link on the way
    /// followed, and the entry there, for `--follow-symlinks`; a directory
    /// reached this way is read through that path, since directories are
    /// opened without following links. The default resolves the path
    /// (`realpath`) and reads the result as [`root`](Provider::root) does: an
    /// `ELOOP` from a link that leads to itself, an `ENOENT` from one that
    /// leads nowhere.
    fn follow(&self, path: &Path, opts: &WalkOptions) -> io::Result<(PathBuf, Entry)> {
        let real = std::fs::canonicalize(path)?;
        let entry = self.root(&real, opts)?;
        Ok((real, entry))
    }

    /// Whether the directory at `dir` holds a valid `CACHEDIR.TAG` (the
    /// 43-byte signature, in a regular file that is not a link), for
    /// `--exclude-caches`. The default reads the file system, the one read of
    /// a file's contents the scanner makes.
    fn cachedir_tag(&self, dir: &Path) -> bool {
        platform::cachedir_tag(dir)
    }

    /// The workers a walk runs when [`WalkOptions::threads`] is 0: twice the
    /// logical CPUs, clamped to 4..=32 (D40). bench-linux's thread sweep,
    /// `bench/results/thread-sweep-linux-x86_64.json` (P1-BENCH-01), holds it
    /// to the rule's bound: its 20 threads ran warm T1 within 5% of the best
    /// (0.243 s against 12 threads' 0.235 s), where the fixed count the rule
    /// rounds to, 8, ran 13% slower. Linux machines span 2 to 64 cores, so the
    /// count scales with them instead (DEV-54). The macOS provider's fixed
    /// count comes from bench-macos's sweep.
    fn default_threads(&self) -> usize {
        let cpus = thread::available_parallelism().map_or(1, NonZeroUsize::get);
        cpus.saturating_mul(2).clamp(4, 32)
    }
}

/// A directory the walk asks a provider to read. It keeps its parent's
/// handle, so the directories on its path can be walked back to the root,
/// and, while the walk keeps them, its parent's descriptor and its own (see
/// `platform::DirFd`).
#[derive(Debug)]
pub struct DirHandle {
    id: DirId,
    path: PathBuf,
    /// The path relative to the root, `/`-separated, as the directory was
    /// reached; empty for the root.
    rel: Vec<u8>,
    depth: u32,
    dev: u64,
    ino: u64,
    mnt_id: u64,
    parent: Option<Arc<DirHandle>>,
    /// The parent's descriptor, to open this directory relative to, until
    /// it is opened.
    parent_fd: Mutex<Option<Arc<platform::DirFd>>>,
    /// This directory's descriptor, from its first open until the walk has
    /// read the directory whole.
    fd: Mutex<Option<Arc<platform::DirFd>>>,
}

impl DirHandle {
    /// The handle of `entry`, a directory numbered `id` at `rel` below the
    /// root (empty when no exclude needs it), inside `parent`, whose
    /// descriptor it opens relative to when `parent_fd` holds it.
    fn new(
        id: DirId,
        path: PathBuf,
        rel: Vec<u8>,
        entry: &Entry,
        parent: Option<Arc<DirHandle>>,
        parent_fd: Option<Arc<platform::DirFd>>,
    ) -> DirHandle {
        DirHandle {
            id,
            path,
            rel,
            depth: parent.as_ref().map_or(0, |p| p.depth + 1),
            dev: entry.dev,
            ino: entry.ino,
            mnt_id: entry.mnt_id,
            parent,
            parent_fd: Mutex::new(parent_fd),
            fd: Mutex::new(None),
        }
    }

    /// Takes the parent's descriptor, which only the first open uses.
    pub(crate) fn take_parent_fd(&self) -> Option<Arc<platform::DirFd>> {
        self.parent_fd
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    /// The directory's descriptor, while the walk keeps it.
    pub(crate) fn kept_fd(&self) -> Option<Arc<platform::DirFd>> {
        self.fd
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Keeps `fd`, the directory's descriptor, until it has been read whole.
    pub(crate) fn keep_fd(&self, fd: Arc<platform::DirFd>) {
        *self.fd.lock().unwrap_or_else(PoisonError::into_inner) = Some(fd);
    }

    /// Lets go of the directory's descriptor once it has been read whole;
    /// a subdirectory not yet opened still holds it.
    fn release_fd(&self) {
        let fd = self
            .fd
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        drop(fd);
    }

    /// A handle for tests: `path`, `depth` levels below a root, with a
    /// handle for each directory between them.
    #[cfg(test)]
    pub(crate) fn detached(path: impl Into<PathBuf>, depth: u32) -> DirHandle {
        let path = path.into();
        let parent = depth
            .checked_sub(1)
            .map(|up| Arc::new(DirHandle::detached(path.parent().unwrap_or(&path), up)));
        DirHandle {
            id: DirId::ROOT,
            path,
            rel: Vec::new(),
            depth,
            dev: 0,
            ino: 0,
            mnt_id: 0,
            parent,
            parent_fd: Mutex::new(None),
            fd: Mutex::new(None),
        }
    }

    /// The path relative to the root of this directory's child `name`.
    fn rel_of(&self, name: &[u8]) -> Vec<u8> {
        let mut rel = Vec::with_capacity(self.rel.len() + 1 + name.len());
        rel.extend_from_slice(&self.rel);
        if !rel.is_empty() {
            rel.push(b'/');
        }
        rel.extend_from_slice(name);
        rel
    }

    /// The directory's device, as its entry reported it.
    #[must_use]
    pub fn dev(&self) -> u64 {
        self.dev
    }

    /// The directory's inode number, as its entry reported it.
    #[must_use]
    pub fn ino(&self) -> u64 {
        self.ino
    }

    /// The directory's mount id, as its entry reported it.
    #[must_use]
    pub fn mnt_id(&self) -> u64 {
        self.mnt_id
    }

    /// The parent directory's handle; `None` for the root.
    #[must_use]
    pub fn parent(&self) -> Option<&DirHandle> {
        self.parent.as_deref()
    }

    /// This directory and every directory above it, up to the root.
    pub fn ancestors(&self) -> impl Iterator<Item = &DirHandle> {
        std::iter::successors(Some(self), |dir| dir.parent())
    }

    /// The directory's id.
    #[must_use]
    pub fn id(&self) -> DirId {
        self.id
    }

    /// The directory's path, to open: the root as given joined with the
    /// names below it; below a link followed under `--follow-symlinks`, the
    /// link's resolved target joined with the names below that.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// How far below the root the directory is; the root is 0.
    #[must_use]
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
    /// Entries and directories that disappeared between being listed and
    /// being read ([`ScanEvent::Vanished`]).
    pub vanished: u64,
    /// Files left out and directories not entered by the excludes.
    pub excluded: u64,
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
/// # Errors
///
/// As [`walk_with`], and [`WalkError::NoProvider`] on a platform other
/// than Linux and macOS.
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

/// Sets the process up for walking: raises the descriptor limit, grows the
/// descriptor table to hold the descriptors a walk keeps open, and on macOS
/// keeps dataless files from downloading. A walk with this platform's
/// provider does it itself; a program that starts a thread before its first
/// walk calls it first, while it has one thread, because Linux makes a
/// process with more than one wait for an RCU grace period to grow the
/// table.
pub fn prepare() {
    platform::setup();
}

/// Makes SIGINT set `flag`, the [`WalkOptions::cancel`] of the walks it is
/// given to: a running walk then stops within a directory per worker and
/// returns [`WalkError::Cancelled`]. A later call replaces the flag.
/// Returns `false`, installing nothing, when the process inherited SIGINT
/// ignored (a background job): Ctrl-C was not meant for it.
pub fn cancel_on_interrupt(flag: &Arc<AtomicBool>) -> io::Result<bool> {
    platform::cancel_on_interrupt(flag)
}

/// Blocks the calling thread until SIGINT arrives (`true`), or until
/// [`stand_down_interrupt`] is called or no handler was installed
/// (`false`). A thread that ends the process when a walk does not stop in
/// time waits here instead of polling the flag.
#[must_use]
pub fn wait_for_interrupt() -> bool {
    platform::wait_for_interrupt()
}

/// Wakes a thread waiting in [`wait_for_interrupt`], which returns
/// `false`, once the walk it watched is over. The handler stays installed:
/// a later SIGINT sets the flag, which no walk reads any more.
pub fn stand_down_interrupt() {
    platform::stand_down_interrupt();
}

/// The columns and rows of the terminal on stdout (`TIOCGWINSZ`); `None`
/// when stdout is not a terminal. The static treemap sizes itself by it.
pub fn terminal_size() -> Option<(u16, u16)> {
    platform::terminal_size()
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

/// What a failed read means, by its error number: the error matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Failed {
    /// `EACCES`, `EPERM` and the rest: the directory is listed empty, with
    /// the error number.
    Unreadable,
    /// `ENOENT`, `ESTALE`, and `ENOTDIR` for a directory replaced by a file:
    /// it disappeared, as listed, after it was listed.
    Vanished,
    /// `ELOOP`, `ENAMETOOLONG`, `EIO`: an error of the entry, recorded on
    /// the directory that listed it with the entry dropped, for a link
    /// followed under `--follow-symlinks`. A directory that fails this way
    /// when it is read has had its entry delivered already, so it is listed
    /// empty with the error number, as an unreadable one.
    OnParent,
}

impl Failed {
    fn of(errno: i32) -> Failed {
        match errno {
            libc::ENOENT | libc::ESTALE | libc::ENOTDIR => Failed::Vanished,
            libc::ELOOP | libc::ENAMETOOLONG | libc::EIO => Failed::OnParent,
            _ => Failed::Unreadable,
        }
    }
}

/// The names one [`Job::Stat`] reads: the Linux floor's batch size, which
/// spreads a wide directory's per-entry calls over every worker.
pub(crate) const STAT_BATCH: usize = 1024;

/// How many entries a streaming read holds before it flushes them to the
/// walk ([`Provider::read_dir_streaming`]).
pub const STREAM_CHUNK: usize = 4096;

/// What [`Provider::read_dir_streaming`] calls with the entries, errors and
/// names left for later read so far; it takes the entries and errors, and
/// the names in whole stat batches.
pub type Flush<'a> = dyn FnMut(&mut Vec<Entry>, &mut Vec<ScanError>, &mut Vec<NameRef>) + 'a;

/// Cuts the names a directory's read left for later into [`Job::Stat`]
/// batches of at most [`STAT_BATCH`], each in a Vec of its own size.
/// Splitting one Vec repeatedly would leave every batch holding the capacity
/// of all the names after it: a million names would hold about 8 GB.
fn stat_batches(later: Vec<NameRef>) -> impl Iterator<Item = Vec<NameRef>> {
    let mut names = later.into_iter();
    std::iter::from_fn(move || {
        let batch: Vec<NameRef> = names.by_ref().take(STAT_BATCH).collect();
        (!batch.is_empty()).then_some(batch)
    })
}

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
    /// Every name the directory listed, for the cache rules' markers.
    listing: Option<Arc<HashSet<NameRef>>>,
    /// The directory's read plus its stat jobs not yet done; the part that
    /// takes it to 0 closes the directory.
    parts: AtomicUsize,
    /// The error the directory's read ended with after it had flushed, for
    /// the closing part; 0 for none.
    errno: AtomicI32,
    /// Whether the directory vanished during its read, for the closing part.
    vanished: AtomicBool,
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
    /// Entries of `dir` that disappeared after they were listed.
    vanished: Vec<NameRef>,
    /// Whether `dir` itself disappeared after it was listed; only on a
    /// closing batch.
    dir_vanished: bool,
    /// Entries left out or not entered by the excludes.
    excluded: u64,
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
    /// Each mount id met so far: its class (`None` when it could not be
    /// read), and whether its volume has been reported.
    mounts: Mutex<HashMap<u64, Met>>,
    /// What the walk leaves out, when it leaves anything out.
    excludes: Option<Excludes<'a>>,
}

/// What adopting a directory's entries found beside them.
#[derive(Default)]
struct Adopted {
    /// Volumes first met.
    volumes: Vec<VolumeInfo>,
    /// Directories listed but not entered, and why.
    skipped: Vec<(DirId, Skip)>,
    /// Names that vanished after they were listed.
    vanished: Vec<NameRef>,
    /// Entries left out or not entered by the excludes.
    excluded: u64,
}

/// A mount the walk has met.
#[derive(Debug, Clone)]
struct Met {
    /// What [`Provider::mount_info`] said, read once per mount; `None`
    /// when it could not be read.
    info: Option<VolumeInfo>,
    reported: bool,
}

/// Walks `root` with `provider`, delivering every entry to `sink` on this
/// thread. See [`ScanEvent`] for the order of events.
///
/// Workers poll [`WalkOptions::cancel`] before each directory; once it is set
/// the walk stops and returns [`WalkError::Cancelled`]. A panic in a worker
/// is resumed on this thread.
///
/// # Errors
///
/// The root cannot be read, a worker thread cannot start, the walk was
/// cancelled ([`WalkError::Cancelled`]), or directory ids ran out.
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
    let root_dir = DirHandle::new(DirId::ROOT, root.to_owned(), Vec::new(), &entry, None, None);
    let (root_dev, root_mnt_id) = (entry.dev, entry.mnt_id);
    sink(ScanEvent::Entry(entry));
    if let Ok(mut volume) = provider.volume(root) {
        // The root's own identity, as every other volume row takes its
        // entry's: on macOS a volume's statfs device can be its volume
        // group's, shared by every volume in the group.
        volume.dev = root_dev;
        volume.mnt_id = root_mnt_id;
        volume.reason = None;
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
        mounts: Mutex::new(HashMap::from([(
            root_mnt_id,
            Met {
                info: None,
                reported: true,
            },
        )])),
        excludes: Excludes::new(opts),
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
        // A walk cancelled as it finished is cancelled: Ctrl-C during the
        // last directory still stops the command.
        let finished = finished && !opts.cancel.load(Ordering::Relaxed);
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
    summary.vanished += batch.vanished.len() as u64;
    summary.excluded += batch.excluded;
    for name in batch.vanished {
        sink(ScanEvent::Vanished {
            dir: batch.dir,
            name: Some(name),
        });
    }
    for volume in batch.volumes {
        summary.volumes += 1;
        summary.mounts_excluded += u64::from(!volume.entered());
        sink(ScanEvent::Volume(volume));
    }
    summary.skipped_dirs += batch.skipped.len() as u64;
    for (dir, reason) in batch.skipped {
        sink(ScanEvent::Skipped { dir, reason });
        sink(ScanEvent::DirClosed { dir, errno: None });
    }
    if batch.closes {
        summary.unreadable_dirs += u64::from(batch.errno.is_some());
        if batch.dir_vanished {
            summary.vanished += 1;
            sink(ScanEvent::Vanished {
                dir: batch.dir,
                name: None,
            });
        }
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
    /// anything inside it. A wide directory is sent in chunks as it is read
    /// ([`Provider::read_dir_streaming`]), unless the cache rules need its
    /// whole listing first.
    fn read_dir(
        &self,
        dir: &Arc<DirHandle>,
        worker: &Worker<Job>,
        shared: &PoolShared<Job, Event>,
    ) {
        let (mut entries, mut errors, mut later) = (Vec::new(), Vec::new(), Vec::new());
        let whole = self.excludes.as_ref().is_some_and(Excludes::wants_listing);
        // The stat jobs made while reading, and whether the ids ran out.
        let mut split: Option<Arc<Split>> = None;
        let mut stopped = false;
        let read = if whole {
            self.provider
                .read_dir_split(dir, self.opts, &mut entries, &mut errors, &mut later)
        } else {
            let mut flush = |entries: &mut Vec<Entry>,
                             errors: &mut Vec<ScanError>,
                             later: &mut Vec<NameRef>| {
                // After a cancel nothing more is sent: the walk is ending.
                if stopped
                    || self.opts.cancel.load(Ordering::Relaxed)
                    || !self.send_chunk(dir, &mut split, entries, errors, later, worker, shared)
                {
                    stopped = true;
                    entries.clear();
                    errors.clear();
                    later.clear();
                }
            };
            self.provider.read_dir_streaming(
                dir,
                self.opts,
                &mut entries,
                &mut errors,
                &mut later,
                &mut flush,
            )
        };
        if stopped {
            return;
        }
        let failed = match read {
            Ok(()) => None,
            Err(e) => {
                entries.clear();
                errors.clear();
                later.clear();
                Some(e.raw_os_error().unwrap_or(EIO))
            }
        };
        let mut listing = self.listing(&entries, &errors, &later);
        let mut jobs = Vec::new();
        let Some(adopted) = self.adopt(
            dir,
            listing.as_deref(),
            &mut entries,
            &mut errors,
            &mut jobs,
            shared,
        ) else {
            return;
        };
        // A directory that could not be read, by the error matrix: one that
        // disappeared is vanished, and any other failure, ELOOP,
        // ENAMETOOLONG and EIO included, lists it empty with the error
        // number (its entry was delivered with its parent, so it cannot be
        // dropped, and the unreadable flag and the partial read cover it).
        let (mut errno, mut dir_vanished) = (None, false);
        match failed.map(|e| (e, Failed::of(e))) {
            None => {}
            Some((_, Failed::Vanished)) => dir_vanished = true,
            Some((e, _)) => errno = Some(e),
        }
        Self::split_off(dir, &mut split, &mut listing, &mut later, true, &mut jobs);
        // Stat jobs still running close the directory, with the read's error
        // or its vanishing.
        if let Some(split) = &split {
            if let Some(e) = errno.take() {
                split.errno.store(e, Ordering::Release);
            }
            if std::mem::take(&mut dir_vanished) {
                split.vanished.store(true, Ordering::Release);
            }
        }
        let batch = Batch {
            dir: dir.id,
            entries,
            errors,
            vanished: adopted.vanished,
            dir_vanished,
            excluded: adopted.excluded,
            volumes: adopted.volumes,
            skipped: adopted.skipped,
            closes: split.is_none(),
            errno,
        };
        self.send(batch, jobs, worker, shared);
        match split {
            Some(split) => Self::finish_part(&split, shared),
            None => dir.release_fd(),
        }
    }

    /// Sends what a streaming read of `dir` has read so far, before the
    /// directory is read whole: its entries in a batch that does not close
    /// the directory, and its names left for later as stat jobs of whole
    /// [`STAT_BATCH`]es. `false` when the ids ran out, which stops the walk.
    #[allow(clippy::too_many_arguments)]
    fn send_chunk(
        &self,
        dir: &Arc<DirHandle>,
        split: &mut Option<Arc<Split>>,
        entries: &mut Vec<Entry>,
        errors: &mut Vec<ScanError>,
        later: &mut Vec<NameRef>,
        worker: &Worker<Job>,
        shared: &PoolShared<Job, Event>,
    ) -> bool {
        let mut jobs = Vec::new();
        let Some(adopted) = self.adopt(dir, None, entries, errors, &mut jobs, shared) else {
            return false;
        };
        Self::split_off(dir, split, &mut None, later, false, &mut jobs);
        let batch = Batch {
            dir: dir.id,
            entries: std::mem::take(entries),
            errors: std::mem::take(errors),
            vanished: adopted.vanished,
            dir_vanished: false,
            excluded: adopted.excluded,
            volumes: adopted.volumes,
            skipped: adopted.skipped,
            closes: false,
            errno: None,
        };
        self.send(batch, jobs, worker, shared);
        true
    }

    /// Every name a directory's read listed, when the cache rules need it:
    /// only when a child directory could be a cache by its name.
    fn listing(
        &self,
        entries: &[Entry],
        errors: &[ScanError],
        later: &[NameRef],
    ) -> Option<Arc<HashSet<NameRef>>> {
        let excludes = self.excludes.as_ref().filter(|e| e.wants_listing())?;
        let dirs = entries.iter().filter(|e| e.kind == Kind::Dir);
        if !excludes.may_mark(dirs.map(|e| &e.name).chain(later)) {
            return None;
        }
        let names = entries.iter().map(|e| e.name.clone());
        let names = names.chain(errors.iter().map(|e| e.name.clone()));
        Some(Arc::new(names.chain(later.iter().cloned()).collect()))
    }

    /// Turns `later`, names `dir`'s read left for later, into stat jobs of
    /// [`STAT_BATCH`] names in `jobs`, making `split` (with `listing`) on
    /// the first; a last batch of fewer names only with `all`, else it
    /// stays in `later`.
    fn split_off(
        dir: &Arc<DirHandle>,
        split: &mut Option<Arc<Split>>,
        listing: &mut Option<Arc<HashSet<NameRef>>>,
        later: &mut Vec<NameRef>,
        all: bool,
        jobs: &mut Vec<Job>,
    ) {
        let taken = if all {
            later.len()
        } else {
            later.len() / STAT_BATCH * STAT_BATCH
        };
        if taken == 0 {
            return;
        }
        let rest = later.split_off(taken);
        let names = std::mem::replace(later, rest);
        let split = split.get_or_insert_with(|| {
            Arc::new(Split {
                dir: Arc::clone(dir),
                listing: listing.take(),
                parts: AtomicUsize::new(1),
                errno: AtomicI32::new(0),
                vanished: AtomicBool::new(false),
            })
        });
        for batch in stat_batches(names) {
            split.parts.fetch_add(1, Ordering::AcqRel);
            jobs.push(Job::Stat(Arc::clone(split), batch));
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
        let Some(adopted) = self.adopt(
            &split.dir,
            split.listing.as_deref(),
            &mut entries,
            &mut errors,
            &mut jobs,
            shared,
        ) else {
            return;
        };
        let batch = Batch {
            dir: split.dir.id,
            entries,
            errors,
            vanished: adopted.vanished,
            dir_vanished: false,
            excluded: adopted.excluded,
            volumes: adopted.volumes,
            skipped: adopted.skipped,
            closes: false,
            errno: None,
        };
        self.send(batch, jobs, worker, shared);
        Self::finish_part(split, shared);
    }

    /// Sets the parent of `dir`'s new entries and errors, follows its
    /// symbolic links under `--follow-symlinks`, leaves out what the
    /// excludes match, numbers its subdirectories and queues a read of each
    /// the walk enters. Entries that vanished after they were listed leave
    /// `errors`. `listing` is every name `dir` listed, for the cache rules.
    /// Returns `None` when the ids ran out, which stops the walk.
    fn adopt(
        &self,
        dir: &Arc<DirHandle>,
        listing: Option<&HashSet<NameRef>>,
        entries: &mut Vec<Entry>,
        errors: &mut Vec<ScanError>,
        jobs: &mut Vec<Job>,
        shared: &PoolShared<Job, Event>,
    ) -> Option<Adopted> {
        let mut adopted = Adopted::default();
        // Paths relative to the root, built only for globs and ignore rules.
        let by_path = self.excludes.as_ref().filter(|ex| ex.wants_rel());
        let rel_of =
            |name: &NameRef| by_path.map_or_else(Vec::new, |_| dir.rel_of(name.as_bytes()));
        // An entry the excludes leave out reports nothing, not even that it
        // could not be read or vanished.
        errors.retain(|error| {
            if by_path.is_some_and(|ex| ex.matches(&rel_of(&error.name), None)) {
                return false;
            }
            let gone = Failed::of(error.errno) == Failed::Vanished;
            if gone {
                adopted.vanished.push(error.name.clone());
            }
            !gone
        });
        // Where each directory reached through a link is read from.
        let resolved = if self.opts.follow_symlinks {
            self.follow_links(dir, entries, errors)
        } else {
            Vec::new()
        };
        let mut left_out = Vec::new();
        for (i, entry) in entries.iter_mut().enumerate() {
            entry.parent = dir.id;
            entry.dir = None;
            let is_dir = entry.kind == Kind::Dir;
            let rel = rel_of(&entry.name);
            let excluded = self
                .excludes
                .as_ref()
                .and_then(|ex| ex.excluded(&rel, entry, listing));
            if excluded.is_some() {
                adopted.excluded += 1;
                if !is_dir {
                    left_out.push(i);
                    continue;
                }
            }
            if is_dir {
                let Some(id) = self.allocate_dir() else {
                    self.out_of_ids.store(true, Ordering::Relaxed);
                    shared.stop();
                    return None;
                };
                entry.dir = Some(id);
                // The path only a directory needs: a file costs no copy. A
                // directory reached through a link opens by its path; any
                // other relative to this one's descriptor, while it is kept.
                let (path, parent_fd) = match resolved.get(i) {
                    Some(Some(target)) => (target.clone(), None),
                    _ => (dir.path.join(entry.name.as_os_str()), dir.kept_fd()),
                };
                let mut skip = excluded;
                if skip.is_none() {
                    let met = adopted.volumes.len();
                    skip = self.decide(dir, entry, &path, &mut adopted.volumes);
                    // The tag is read only in a directory the walk would
                    // enter, so a skipped mount is never touched.
                    if skip.is_none()
                        && self.excludes.as_ref().is_some_and(Excludes::wants_tags)
                        && self.provider.cachedir_tag(&path)
                    {
                        adopted.excluded += 1;
                        for volume in &mut adopted.volumes[met..] {
                            volume.reason = Some(Skip::ExcludedCache);
                        }
                        skip = Some(Skip::ExcludedCache);
                    }
                }
                if let Some(reason) = skip {
                    adopted.skipped.push((id, reason));
                    continue;
                }
                jobs.push(Job::ReadDir(Arc::new(DirHandle::new(
                    id,
                    path,
                    rel,
                    entry,
                    Some(Arc::clone(dir)),
                    parent_fd,
                ))));
            }
        }
        if !left_out.is_empty() {
            let mut i = 0;
            entries.retain(|_| {
                let keep = left_out.binary_search(&i).is_err();
                i += 1;
                keep
            });
        }
        for error in errors {
            error.dir = dir.id;
        }
        Some(adopted)
    }

    /// Replaces each symbolic link among `entries` with the entry it leads
    /// to, under its own name, and returns, for each entry left, the path a
    /// directory reached through a link is read from. A link that loops, one
    /// to a directory on its own path, and one whose target cannot be named
    /// (`ENAMETOOLONG`) or read (`EIO`) become errors of `dir`; one that
    /// leads nowhere stays a link, and so does one the globs or ignore rules
    /// leave out, which is left out unread.
    fn follow_links(
        &self,
        dir: &DirHandle,
        entries: &mut Vec<Entry>,
        errors: &mut Vec<ScanError>,
    ) -> Vec<Option<PathBuf>> {
        let mut resolved = Vec::with_capacity(entries.len());
        let mut kept = Vec::with_capacity(entries.len());
        for entry in entries.drain(..) {
            let excluded = || {
                self.excludes.as_ref().is_some_and(|ex| {
                    ex.wants_rel() && ex.matches(&dir.rel_of(entry.name.as_bytes()), Some(false))
                })
            };
            if entry.kind != Kind::Symlink || excluded() {
                kept.push(entry);
                resolved.push(None);
                continue;
            }
            let path = dir.path.join(entry.name.as_os_str());
            let errno = match self.provider.follow(&path, self.opts) {
                Ok((real, target)) => {
                    let is_dir = target.kind == Kind::Dir;
                    let on_path = is_dir
                        && dir
                            .ancestors()
                            .any(|a| (a.dev, a.ino) == (target.dev, target.ino));
                    if !on_path {
                        resolved.push(is_dir.then_some(real));
                        kept.push(Entry {
                            name: entry.name,
                            ..target
                        });
                        continue;
                    }
                    libc::ELOOP
                }
                Err(e) => match e.raw_os_error() {
                    Some(errno) if Failed::of(errno) == Failed::OnParent => errno,
                    _ => {
                        kept.push(entry);
                        resolved.push(None);
                        continue;
                    }
                },
            };
            errors.push(ScanError {
                dir: dir.id,
                name: entry.name,
                errno,
            });
        }
        *entries = kept;
        resolved
    }

    /// Decides whether the walk enters `entry`, a directory of `dir` at
    /// `path`. Never a directory already on the path (a bind mount of an
    /// ancestor, or one the kernel did not report as a mount); at a mount
    /// crossing, never a kernel pseudo-filesystem, a network one only under
    /// `--include-network`, and under `-x` no mount other than the root's (a
    /// firmlink leads into the volume the root's own is paired with, so it
    /// is no crossing for `-x`); then whatever the provider skips. A mount's
    /// volume is added to `volumes` the first time it is met, read in full
    /// only when the walk enters it. `None` enters.
    ///
    /// The bind-cycle test walks the directory's ancestors, one pointer per
    /// level (bounded by `PATH_MAX`); a mount is classified once, and the
    /// classification is kept for the volume row of a mount that is not
    /// entered.
    fn decide(
        &self,
        dir: &DirHandle,
        entry: &Entry,
        path: &Path,
        volumes: &mut Vec<VolumeInfo>,
    ) -> Option<Skip> {
        let cycle = dir
            .ancestors()
            .any(|a| (a.dev, a.ino) == (entry.dev, entry.ino));
        let crossing = entry.mnt_id != dir.mnt_id || entry.boundary == Boundary::Mount;
        if !crossing {
            return if cycle {
                Some(Skip::BindCycle)
            } else {
                self.provider.skip(dir, entry, self.opts)
            };
        }
        let met = self.met(entry, path);
        let class = met.as_ref().map(|info| info.class);
        let skip = match class {
            _ if cycle => Some(Skip::BindCycle),
            Some(VolumeClass::Kernel) => Some(Skip::Kernel),
            Some(VolumeClass::Network) if !self.opts.include_network => Some(Skip::Network),
            _ if self.opts.one_file_system
                && entry.boundary != Boundary::Firmlink
                && entry.mnt_id != self.root_mnt_id =>
            {
                Some(Skip::OneFileSystem)
            }
            _ => self.provider.skip(dir, entry, self.opts),
        };
        // A directory the provider skips is reached another way (the Data
        // volume through its firmlinks), so it reports no volume of its own.
        let provider_skip = skip.is_some_and(|s| s == Skip::Firmlinked);
        if !provider_skip && self.claim(entry.mnt_id) {
            let volume = match skip {
                None => self.provider.volume(path).ok(),
                Some(_) => met,
            };
            if let Some(mut volume) = volume {
                volume.dev = entry.dev;
                volume.mnt_id = entry.mnt_id;
                volume.reason = skip;
                volumes.push(volume);
            }
        }
        skip
    }

    /// What [`Provider::mount_info`] says of the mount `entry` crossed into
    /// at `path`, read the first time the mount is met; `None` when it
    /// cannot be read.
    fn met(&self, entry: &Entry, path: &Path) -> Option<VolumeInfo> {
        if let Some(met) = self
            .mounts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&entry.mnt_id)
        {
            return met.info.clone();
        }
        let info = self.provider.mount_info(path, entry.dev).ok();
        self.mounts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(entry.mnt_id)
            .or_insert(Met {
                info,
                reported: false,
            })
            .info
            .clone()
    }

    /// Claims the report of the volume of `mnt_id`: true for the one caller
    /// that is to report it.
    fn claim(&self, mnt_id: u64) -> bool {
        let mut mounts = self.mounts.lock().unwrap_or_else(PoisonError::into_inner);
        let met = mounts.entry(mnt_id).or_insert(Met {
            info: None,
            reported: false,
        });
        !std::mem::replace(&mut met.reported, true)
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
            split.dir.release_fd();
            let close = Batch {
                dir: split.dir.id,
                entries: Vec::new(),
                errors: Vec::new(),
                vanished: Vec::new(),
                dir_vanished: split.vanished.load(Ordering::Acquire),
                excluded: 0,
                volumes: Vec::new(),
                skipped: Vec::new(),
                closes: true,
                errno: Some(split.errno.load(Ordering::Acquire)).filter(|&e| e != 0),
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
