//! Calls both platforms make the same way: opening a directory to read and
//! raising the descriptor limit.

pub(crate) mod signal;
pub(crate) mod winsize;

use std::ffi::{CStr, CString};
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, IntoRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Once};
use std::thread;
use std::time::Duration;

use crate::DirHandle;

/// The descriptor limit a scan asks for (spec: `min(hard, 65536)`).
const NOFILE_TARGET: libc::rlim_t = 65_536;

/// macOS refuses a soft limit above `OPEN_MAX` unless the kernel's
/// per-process maximum allows it; this is the retry when the target is
/// refused.
const OPEN_MAX: libc::rlim_t = 10_240;

/// How long a worker waits in total for a descriptor to free up before the
/// directory is recorded as unreadable with `EMFILE`.
const PARK_LIMIT: Duration = Duration::from_secs(2);

/// The most descriptors a walk keeps open (see [`DirFd`]).
const KEEP_MAX: usize = 1024;

/// The descriptor table a walk starts with: room for [`KEEP_MAX`] kept
/// descriptors beside the workers' own and the rest of the process.
const FD_TABLE: usize = 2 * KEEP_MAX;

/// Raises the soft `RLIMIT_NOFILE` to `min(hard, 65536)`, once per process,
/// lets the walk keep [`KEEP_MAX`] descriptors open, or half the limit it
/// ends at when that is less, and grows the descriptor table to hold
/// [`FD_TABLE`] of them. A refusal leaves the limit as it was: the walk then
/// parks workers on `EMFILE` instead.
///
/// Linux grows a process's table by doubling it from 64 entries, and when
/// the process has more than one thread each step waits for an RCU grace
/// period, during which no thread can open a descriptor: on bench-linux the
/// steps a walk took cost warm T1 18 ms (252.5 ms against 234.1 ms with the
/// table already grown). Grown here, before the first walk, the table never
/// grows during one.
pub(crate) fn raise_nofile_limit() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        raise();
        if let Some(limit) = nofile() {
            let soft = usize::try_from(limit.rlim_cur).unwrap_or(usize::MAX);
            KEEP_LIMIT.store(KEEP_MAX.min(soft / 2), Ordering::Relaxed);
            grow_fd_table(FD_TABLE.min(soft));
        }
    });
}

/// Grows the descriptor table to at least `entries`, by placing a
/// descriptor at `entries - 1` and closing it: the table never shrinks.
fn grow_fd_table(entries: usize) {
    let Ok(last) = libc::c_int::try_from(entries.saturating_sub(1)) else {
        return;
    };
    let Ok(root) = open_dir(Path::new("/")) else {
        return;
    };
    // SAFETY: `root` is open; F_DUPFD_CLOEXEC returns a new descriptor at or
    // above `last`, or -1.
    let high = unsafe { libc::fcntl(root.as_raw_fd(), libc::F_DUPFD_CLOEXEC, last) };
    if high >= 0 {
        // SAFETY: `high` was just returned by fcntl and is owned by nothing
        // else.
        drop(unsafe { OwnedFd::from_raw_fd(high) });
    }
}

/// The descriptor limit, soft and hard.
fn nofile() -> Option<libc::rlimit> {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `limit` is a valid, writable rlimit for getrlimit to fill.
    (unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) } == 0).then_some(limit)
}

/// Raises the soft limit, asking for [`OPEN_MAX`] where the target is
/// refused.
fn raise() {
    let Some(limit) = nofile() else {
        return;
    };
    for want in [NOFILE_TARGET, OPEN_MAX] {
        let want = want.min(limit.rlim_max);
        if limit.rlim_cur >= want {
            return;
        }
        let raised = libc::rlimit {
            rlim_cur: want,
            rlim_max: limit.rlim_max,
        };
        // SAFETY: `raised` is a valid rlimit that setrlimit only reads.
        if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raw const raised) } == 0 {
            return;
        }
    }
}

/// Descriptors the walk keeps open now, in every walk of the process.
static KEPT: AtomicUsize = AtomicUsize::new(0);

/// How many descriptors the walk may keep: [`KEEP_MAX`], or half the soft
/// limit [`raise_nofile_limit`] ends at when that is less, leaving the other
/// half to the workers' own opens and the rest of the process; none before
/// it has run.
static KEEP_LIMIT: AtomicUsize = AtomicUsize::new(0);

/// The most descriptors kept at once, for the test that bounds it.
#[cfg(test)]
static KEPT_PEAK: AtomicUsize = AtomicUsize::new(0);

/// A directory's open descriptor. The walk keeps it in the directory's
/// handle while it reads the directory, so its stat batches reopen the
/// directory through it and each subdirectory opens relative to it: one
/// name looked up instead
/// of every directory from the root, and a directory whose path is longer
/// than `PATH_MAX` read. A subdirectory's handle holds it until that
/// subdirectory is opened, so it closes once the last one is. At most
/// [`KEEP_LIMIT`] are kept at once; a directory opened beyond it is not, and
/// its subdirectories open by path.
#[derive(Debug)]
pub(crate) struct DirFd {
    fd: OwnedFd,
    /// Whether it counts in [`KEPT`].
    kept: bool,
}

impl AsFd for DirFd {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

impl Drop for DirFd {
    fn drop(&mut self) {
        if self.kept {
            KEPT.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

/// The path as a C string. A path from a directory listing holds no NUL.
pub(crate) fn c_path(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes()).map_err(|_| io::ErrorKind::InvalidInput.into())
}

/// The first 43 bytes of a valid `CACHEDIR.TAG`.
const CACHEDIR_SIGNATURE: &[u8; 43] = b"Signature: 8a477f597d28d172789f06886806bc55";

/// Whether the directory at `dir` holds a valid `CACHEDIR.TAG`: a regular
/// file, not a link, that starts with the signature. One `lstat` answers
/// for a directory without a tag, the common case; a candidate is then
/// opened without blocking, so a FIFO of that name cannot stall the walk,
/// and checked again through the descriptor, so nothing swapped in after
/// the first check is read.
pub(crate) fn cachedir_tag(dir: &Path) -> bool {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;

    let path = dir.join("CACHEDIR.TAG");
    let long_enough =
        |m: &std::fs::Metadata| m.is_file() && m.len() >= CACHEDIR_SIGNATURE.len() as u64;
    if !std::fs::symlink_metadata(&path).is_ok_and(|m| long_enough(&m)) {
        return false;
    }
    let Ok(mut tag) = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&path)
    else {
        return false;
    };
    let mut head = [0u8; 43];
    tag.metadata().is_ok_and(|m| long_enough(&m))
        && tag.read_exact(&mut head).is_ok()
        && &head == CACHEDIR_SIGNATURE
}

/// Room for a name of up to `NAME_MAX` (255) bytes and its terminating NUL.
const NAME_BUF: usize = 256;

/// Calls `f` with `name` as a NUL-terminated C string, built on the stack:
/// the per-entry calls take a name for every entry they read, and a heap
/// allocation each would cost more than the copy. A name longer than any
/// filesystem allows still works, through the heap.
pub(crate) fn with_c_name<R>(
    name: &[u8],
    f: impl FnOnce(&std::ffi::CStr) -> io::Result<R>,
) -> io::Result<R> {
    let invalid = || io::Error::from(io::ErrorKind::InvalidInput);
    if name.len() >= NAME_BUF {
        return f(&CString::new(name).map_err(|_| invalid())?);
    }
    let mut buf = [0u8; NAME_BUF];
    buf[..name.len()].copy_from_slice(name);
    f(std::ffi::CStr::from_bytes_with_nul(&buf[..=name.len()]).map_err(|_| invalid())?)
}

/// How a directory is opened to read: never through a final symlink, so a
/// directory replaced by one after it was listed fails with `ELOOP` rather
/// than leading the walk elsewhere.
const OPEN_DIR: libc::c_int =
    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;

/// Opens `path` for reading its entries; see [`open_at`].
pub(crate) fn open_dir(path: &Path) -> io::Result<OwnedFd> {
    open_at(None, &c_path(path)?)
}

/// Opens `path` for reading its entries, relative to the directory open on
/// `dir` or else to the working directory. `openat`, not `open`: musl's
/// `open` follows every `O_CLOEXEC` open with an `fcntl`. On `EMFILE` or
/// `ENFILE` the worker parks, retrying with a growing pause, for up to two
/// seconds.
fn open_at(dir: Option<BorrowedFd<'_>>, path: &CStr) -> io::Result<OwnedFd> {
    let dir = dir.map_or(libc::AT_FDCWD, |dir| dir.as_raw_fd());
    // SAFETY: `dir` is open for the borrow or is AT_FDCWD, and `path` is a
    // NUL-terminated C string that outlives the call.
    parked(|| unsafe { libc::openat(dir, path.as_ptr(), OPEN_DIR) })
}

/// Opens `dir` to read it: relative to its parent's descriptor while the
/// walk keeps that, and by path otherwise (the root, a directory reached
/// through a link, a subdirectory of one opened beyond the limit). The
/// descriptor is kept in the handle while fewer than [`KEEP_LIMIT`] are.
pub(crate) fn open_handle(dir: &DirHandle) -> io::Result<Arc<DirFd>> {
    let fd = match (dir.take_parent_fd(), dir.path().file_name()) {
        (Some(parent), Some(name)) => {
            with_c_name(name.as_bytes(), |name| open_at(Some(parent.as_fd()), name))?
        }
        _ => open_dir(dir.path())?,
    };
    let limit = KEEP_LIMIT.load(Ordering::Relaxed);
    let kept = KEPT
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
            (n < limit).then_some(n + 1)
        })
        .is_ok();
    #[cfg(test)]
    KEPT_PEAK.fetch_max(KEPT.load(Ordering::Relaxed), Ordering::Relaxed);
    let fd = Arc::new(DirFd { fd, kept });
    if kept {
        dir.keep_fd(Arc::clone(&fd));
    }
    Ok(fd)
}

/// Opens `dir` again, for a stat batch: as `.` relative to the descriptor
/// the walk keeps for it, so the batch reads the directory that was listed,
/// and by path when none is kept. Each batch reads through a descriptor of
/// its own: in a process with more than one thread, every call through a
/// descriptor takes and drops a reference on the open file behind it, and
/// the workers reading one directory's batches through the one kept
/// descriptor slowed warm T3, a directory of 1,000,000 files, by 4% on
/// bench-linux.
pub(crate) fn reopen(dir: &DirHandle) -> io::Result<OwnedFd> {
    match dir.kept_fd() {
        Some(fd) => open_at(Some(fd.as_fd()), c"."),
        None => open_dir(dir.path()),
    }
}

/// The descriptor `open` returns, retrying on `EINTR` and parking on
/// `EMFILE` or `ENFILE` with a growing pause, for up to [`PARK_LIMIT`] in
/// all, until another worker closes one.
fn parked(mut open: impl FnMut() -> libc::c_int) -> io::Result<OwnedFd> {
    let mut parked = Duration::ZERO;
    let mut pause = Duration::from_millis(1);
    loop {
        #[cfg(test)]
        calls::other();
        let fd = open();
        if fd >= 0 {
            // SAFETY: `fd` was just returned by `open` and is owned by
            // nothing else.
            return Ok(unsafe { OwnedFd::from_raw_fd(fd) });
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::EINTR) => {}
            Some(libc::EMFILE | libc::ENFILE) if parked < PARK_LIMIT => {
                thread::sleep(pause);
                parked += pause;
                pause = (pause * 2).min(Duration::from_millis(100));
            }
            _ => return Err(error),
        }
    }
}

/// `statvfs(3)` of the filesystem holding `path`.
#[cfg(target_os = "linux")]
pub(crate) fn statvfs(path: &CStr) -> io::Result<libc::statvfs> {
    let mut buf = MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `path` is NUL-terminated and `buf` is writable for one
    // `struct statvfs`.
    if unsafe { libc::statvfs(path.as_ptr(), buf.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: statvfs succeeded, so it filled the struct.
    Ok(unsafe { buf.assume_init() })
}

/// Retries a call that returns -1 with `errno` on `EINTR`.
fn retry(mut call: impl FnMut() -> libc::c_int) -> io::Result<()> {
    loop {
        if call() == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

/// `fstatat(2)`'s flags: never follow a final symlink, and on Linux never
/// trigger an automount.
#[cfg(target_os = "linux")]
const STAT_FLAGS: libc::c_int = libc::AT_SYMLINK_NOFOLLOW | libc::AT_NO_AUTOMOUNT;
#[cfg(not(target_os = "linux"))]
const STAT_FLAGS: libc::c_int = libc::AT_SYMLINK_NOFOLLOW;

/// `fstatat(2)` of `name` in the directory open on `dir`, not following a
/// final symlink.
pub(crate) fn lstat_at(dir: BorrowedFd<'_>, name: &CStr) -> io::Result<libc::stat> {
    #[cfg(test)]
    calls::entry();
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `dir` is open for the borrow, `name` is NUL-terminated, and
    // `stat` is writable for one `struct stat`.
    retry(|| unsafe {
        libc::fstatat(
            dir.as_raw_fd(),
            name.as_ptr(),
            stat.as_mut_ptr(),
            STAT_FLAGS,
        )
    })?;
    // SAFETY: fstatat succeeded, so it filled the struct.
    Ok(unsafe { stat.assume_init() })
}

/// `lstat(2)` of `path`.
pub(crate) fn lstat(path: &CStr) -> io::Result<libc::stat> {
    #[cfg(test)]
    calls::entry();
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `path` is NUL-terminated and `stat` is writable for one
    // `struct stat`.
    retry(|| unsafe { libc::lstat(path.as_ptr(), stat.as_mut_ptr()) })?;
    // SAFETY: lstat succeeded, so it filled the struct.
    Ok(unsafe { stat.assume_init() })
}

/// The names in the directory open on `dir`, without `.` and `..`; see
/// [`each_name`].
pub(crate) fn list_names(dir: BorrowedFd<'_>) -> io::Result<Vec<crate::NameRef>> {
    let mut names = Vec::new();
    each_name(dir, &mut |name| names.push(name))?;
    Ok(names)
}

/// Calls `each` with the name of every entry in the directory open on
/// `dir`, without `.` and `..`, read with `readdir` through a duplicate of
/// the descriptor, so the listing is of the directory the descriptor holds,
/// not of whatever its path names by now. The listing starts from the
/// beginning, wherever an earlier read of the descriptor left off.
pub(crate) fn each_name(
    dir: BorrowedFd<'_>,
    each: &mut dyn FnMut(crate::NameRef),
) -> io::Result<()> {
    // SAFETY: `dir` is open for the borrow; F_DUPFD_CLOEXEC returns a new
    // descriptor or -1. The duplicate waits for a free descriptor as an
    // open does.
    let dup =
        parked(|| unsafe { libc::fcntl(dir.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) })?.into_raw_fd();
    // SAFETY: `dup` is a descriptor this function owns; fdopendir takes it
    // over on success, and on failure it is closed here.
    let stream = unsafe { libc::fdopendir(dup) };
    if stream.is_null() {
        let error = io::Error::last_os_error();
        // SAFETY: fdopendir failed, so `dup` is still ours to close.
        unsafe { libc::close(dup) };
        return Err(error);
    }
    // SAFETY: `stream` is live; the duplicate shares the descriptor's
    // offset, which a bulk read may have moved.
    unsafe { libc::rewinddir(stream) };
    let result = loop {
        set_errno(0);
        // SAFETY: `stream` is a live directory stream until closedir below.
        let entry = unsafe { libc::readdir(stream) };
        if entry.is_null() {
            let error = io::Error::last_os_error();
            break if error.raw_os_error() == Some(0) {
                Ok(())
            } else {
                Err(error)
            };
        }
        // SAFETY: readdir returned a valid entry whose d_name is
        // NUL-terminated and lives until the next readdir call.
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if name != b"." && name != b".." {
            each(crate::NameRef::new(name));
        }
    };
    // SAFETY: `stream` came from fdopendir and is closed once.
    unsafe { libc::closedir(stream) };
    result
}

/// Sets the calling thread's `errno`.
fn set_errno(value: libc::c_int) {
    // SAFETY: the errno location is the calling thread's own int.
    unsafe {
        #[cfg(target_os = "linux")]
        {
            *libc::__errno_location() = value;
        }
        #[cfg(target_os = "macos")]
        {
            *libc::__error() = value;
        }
    }
}

/// Per-thread counts of the system calls the providers make, for the tests
/// that count them: bulk reads, reads of one entry, and every other call
/// a walk makes per directory (opening it, duplicating its descriptor).
#[cfg(test)]
// Only the macOS tests count bulk reads.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) mod calls {
    use std::cell::Cell;

    thread_local! {
        pub(crate) static BULK: Cell<usize> = const { Cell::new(0) };
        pub(crate) static ENTRY: Cell<usize> = const { Cell::new(0) };
        pub(crate) static OTHER: Cell<usize> = const { Cell::new(0) };
    }

    pub(crate) fn bulk() {
        BULK.with(|n| n.set(n.get() + 1));
    }

    pub(crate) fn entry() {
        ENTRY.with(|n| n.set(n.get() + 1));
    }

    pub(crate) fn other() {
        OTHER.with(|n| n.set(n.get() + 1));
    }

    /// This thread's counts so far: (bulk, entry, other).
    pub(crate) fn now() -> (usize, usize, usize) {
        (
            BULK.with(Cell::get),
            ENTRY.with(Cell::get),
            OTHER.with(Cell::get),
        )
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::CStr;
    use std::fs::File;
    use std::io;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::Ordering;
    use std::thread;
    use std::time::{Duration, Instant};

    use duscope_testkit::TestEnv;

    use std::os::fd::AsFd;

    #[cfg(target_os = "linux")]
    use super::FD_TABLE;
    use super::{
        KEEP_LIMIT, KEPT, KEPT_PEAK, NOFILE_TARGET, OPEN_MAX, list_names, open_at, open_dir,
        raise_nofile_limit, with_c_name,
    };
    use crate::{DirHandle, NameRef, STAT_BATCH, ScanEvent, WalkOptions, walk};

    /// The directory the child process opens under a full descriptor table.
    const CHILD_DIR: &str = "DUSCOPE_TEST_EMFILE_DIR";

    /// The tree the child process walks under a low descriptor limit.
    const KEEP_DIR: &str = "DUSCOPE_TEST_KEEP_DIR";

    /// What a walk of `root` met: entries, entry errors, and directories
    /// closed with an error.
    fn walk_counts(root: &Path, opts: &WalkOptions) -> (usize, usize, Vec<i32>) {
        let (mut entries, mut errors, mut unreadable) = (0, 0, Vec::new());
        walk(root, opts, |event| match event {
            ScanEvent::Entry(_) => entries += 1,
            ScanEvent::Error(_) => errors += 1,
            ScanEvent::DirClosed {
                errno: Some(errno), ..
            } => unreadable.push(errno),
            _ => {}
        })
        .unwrap();
        (entries, errors, unreadable)
    }

    fn nofile() -> libc::rlimit {
        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: `limit` is a valid, writable rlimit for getrlimit to fill.
        assert_eq!(
            unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) },
            0
        );
        limit
    }

    #[test]
    fn p1_walk_02_the_descriptor_limit_is_raised() {
        raise_nofile_limit();
        let limit = nofile();
        // The raise asks for 65,536 and, where that is refused (macOS above
        // OPEN_MAX), for OPEN_MAX; never above the hard limit.
        let floor = limit.rlim_max.min(NOFILE_TARGET).min(OPEN_MAX);
        assert!(
            limit.rlim_cur >= floor,
            "soft limit {} below {floor}",
            limit.rlim_cur
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn p1_walk_02_the_descriptor_table_is_grown_before_a_walk() {
        raise_nofile_limit();
        // The table's size, which never shrinks.
        let status = std::fs::read_to_string("/proc/self/status").unwrap();
        let size: usize = status
            .lines()
            .find_map(|line| line.strip_prefix("FDSize:"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let soft = usize::try_from(nofile().rlim_cur).unwrap();
        assert!(size >= FD_TABLE.min(soft), "a table of {size} descriptors");
    }

    #[test]
    fn p1_walk_02_emfile_parks_until_a_descriptor_frees() {
        // The descriptor table is filled in a child process: in this one,
        // the tests running beside it would fail with EMFILE.
        let env = TestEnv::new();
        let child = "platform::posix::tests::p1_walk_02_emfile_child_process";
        let out = env
            .command(std::env::current_exe().unwrap())
            .args(["--exact", child, "--ignored", "--test-threads", "1"])
            .env(CHILD_DIR, env.tmp_dir())
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{stdout}{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            stdout.contains("1 passed"),
            "the child did not run: {stdout}"
        );
    }

    #[test]
    #[ignore = "run in a child process by p1_walk_02_emfile_parks_until_a_descriptor_frees"]
    fn p1_walk_02_emfile_child_process() {
        // `--ignored` runs (the platform job) reach it without its parent.
        let Some(dir) = std::env::var_os(CHILD_DIR).map(PathBuf::from) else {
            return;
        };
        let dir_fd = open_dir(&dir).unwrap();
        let mut limit = nofile();
        limit.rlim_cur = limit.rlim_cur.min(64);
        // SAFETY: `limit` is a valid rlimit that setrlimit only reads.
        assert_eq!(
            unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raw const limit) },
            0
        );
        let mut held = Vec::new();
        let full = loop {
            match File::open("/dev/null") {
                Ok(file) => held.push(file),
                Err(e) => break e,
            }
        };
        assert_eq!(full.raw_os_error(), Some(libc::EMFILE));
        let opener = thread::spawn(move || {
            let start = Instant::now();
            (open_dir(&dir).map(drop), start.elapsed())
        });
        thread::sleep(Duration::from_millis(200));
        held.pop();
        let (result, waited) = opener.join().unwrap();
        result.expect("the open parked until a descriptor freed, then succeeded");
        assert!(waited >= Duration::from_millis(150), "it waited {waited:?}");
        // Fill the table again: listing through a held descriptor needs a
        // duplicate, which parks the same way.
        let full = loop {
            match File::open("/dev/null") {
                Ok(file) => held.push(file),
                Err(e) => break e,
            }
        };
        assert_eq!(full.raw_os_error(), Some(libc::EMFILE));
        let listing = thread::spawn(move || {
            let start = Instant::now();
            (list_names(dir_fd.as_fd()).map(drop), start.elapsed())
        });
        thread::sleep(Duration::from_millis(200));
        held.pop();
        let (result, waited) = listing.join().unwrap();
        result.expect("the duplicate parked until a descriptor freed, then succeeded");
        assert!(waited >= Duration::from_millis(150), "it waited {waited:?}");
    }

    #[test]
    fn p1_walk_02_a_tree_deeper_than_path_max_is_read_whole() {
        // Each directory opens relative to its parent's descriptor, so the
        // length of its path never reaches the kernel; opened by path, the
        // levels past PATH_MAX would be unreadable with ENAMETOOLONG. The
        // tree is made the same way, as no path to its bottom can be opened.
        let env = TestEnv::new();
        let root = env.tmp_dir().join("deep");
        std::fs::create_dir(&root).unwrap();
        let name = [b'd'; 200]
            .iter()
            .chain(b"\0")
            .copied()
            .collect::<Vec<u8>>();
        let name = CStr::from_bytes_with_nul(&name).unwrap();
        let levels = usize::try_from(libc::PATH_MAX).unwrap() / 200 + 4;
        let mut dir = open_dir(&root).unwrap();
        for _ in 0..levels {
            // SAFETY: `dir` is open and `name` is NUL-terminated.
            assert_eq!(
                unsafe { libc::mkdirat(dir.as_raw_fd(), name.as_ptr(), 0o755) },
                0
            );
            dir = open_at(Some(dir.as_fd()), name).unwrap();
        }
        let flags = libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC;
        // SAFETY: `dir` is open and the name is NUL-terminated; the mode is
        // the variadic argument O_CREAT reads.
        let file = unsafe { libc::openat(dir.as_raw_fd(), c"bottom".as_ptr(), flags, 0o644) };
        assert!(file >= 0, "{}", io::Error::last_os_error());
        // SAFETY: `file` was just opened and is owned by nothing else.
        drop(unsafe { OwnedFd::from_raw_fd(file) });

        let (entries, errors, unreadable) = walk_counts(&root, &WalkOptions::default());
        assert!(
            unreadable.is_empty(),
            "directories closed with {unreadable:?}"
        );
        assert_eq!(errors, 0);
        // The root, each level and the file.
        assert_eq!(entries, levels + 2);
    }

    #[test]
    fn p1_walk_02_stat_batches_read_the_directory_that_was_listed() {
        // The directory is swapped for another after its read: a stat batch
        // still reads the one listed, through the descriptor kept for it.
        let env = TestEnv::new();
        let dir = env.tmp_dir().join("wide");
        std::fs::create_dir(&dir).unwrap();
        for i in 0..STAT_BATCH + 10 {
            File::create(dir.join(format!("f{i}"))).unwrap();
        }
        let provider = crate::platform::native().unwrap();
        let handle = DirHandle::detached(&dir, 0);
        // One worker: on macOS the probe that may switch to the stat queue
        // runs only with more.
        let opts = WalkOptions {
            threads: 1,
            ..WalkOptions::default()
        };
        let (mut out, mut errs, mut later) = (Vec::new(), Vec::new(), Vec::new());
        provider
            .read_dir_split(&handle, &opts, &mut out, &mut errs, &mut later)
            .unwrap();
        std::fs::rename(&dir, env.tmp_dir().join("moved")).unwrap();
        std::fs::create_dir(&dir).unwrap();
        let names: Vec<NameRef> = out.iter().take(10).map(|e| e.name.clone()).collect();
        let mut batch = Vec::new();
        provider
            .stat_names(&handle, &names, &opts, &mut batch, &mut errs)
            .unwrap();
        assert!(errs.is_empty(), "{errs:?}");
        assert_eq!(batch.len(), names.len());
    }

    #[test]
    fn p1_walk_02_kept_descriptors_stay_under_half_the_limit() {
        // A spine of 100 directories, each holding an empty one on either
        // side of the next: `a017`, `m017`, `z017`. One worker takes its
        // newest job first, so a level whose spine directory is not the
        // first listed keeps its descriptor for an empty directory left
        // until the spine below is read. The spine is listed between the
        // two in name order, creation order and its reverse, and in two of
        // three hash orders (each level's names differ), so most of the 100
        // levels keep one. Under a limit of 64 the walk keeps 32 and opens
        // the rest by path; it closes every one by its end.
        let env = TestEnv::new();
        let mut dir = env.tmp_dir().join("spine");
        let root = dir.clone();
        std::fs::create_dir(&dir).unwrap();
        for level in 0..100 {
            for side in ["a", "m", "z"] {
                std::fs::create_dir(dir.join(format!("{side}{level:03}"))).unwrap();
            }
            dir.push(format!("m{level:03}"));
        }
        // The child process: in this one, the tests running beside it keep
        // descriptors of their own.
        let child = "platform::posix::tests::p1_walk_02_keep_child_process";
        let out = env
            .command(std::env::current_exe().unwrap())
            .args(["--exact", child, "--ignored", "--test-threads", "1"])
            .env(KEEP_DIR, &root)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{stdout}{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            stdout.contains("1 passed"),
            "the child did not run: {stdout}"
        );
    }

    #[test]
    #[ignore = "run in a child process by p1_walk_02_kept_descriptors_stay_under_half_the_limit"]
    fn p1_walk_02_keep_child_process() {
        let Some(root) = std::env::var_os(KEEP_DIR).map(PathBuf::from) else {
            return;
        };
        // The hard limit too, so the raise at the walk's start stays at 64.
        let limit = libc::rlimit {
            rlim_cur: 64,
            rlim_max: 64,
        };
        // SAFETY: `limit` is a valid rlimit that setrlimit only reads.
        assert_eq!(
            unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raw const limit) },
            0
        );
        let opts = WalkOptions {
            threads: 1,
            ..WalkOptions::default()
        };
        let (entries, errors, unreadable) = walk_counts(&root, &opts);
        assert!(
            unreadable.is_empty(),
            "directories closed with {unreadable:?}"
        );
        assert_eq!(errors, 0);
        assert_eq!(entries, 1 + 100 * 3);
        assert_eq!(KEEP_LIMIT.load(Ordering::Relaxed), 32);
        assert_eq!(
            KEPT_PEAK.load(Ordering::Relaxed),
            32,
            "the most kept at once"
        );
        assert_eq!(KEPT.load(Ordering::Relaxed), 0, "kept after the walk");
    }

    #[test]
    fn p1_walk_02_names_pass_to_c_from_the_stack() {
        for len in [0, 1, 255, 256, 300] {
            let name = vec![b'n'; len];
            let back = with_c_name(&name, |c| Ok(c.to_bytes().to_vec())).unwrap();
            assert_eq!(back, name, "a name of {len} bytes");
        }
        let nul = with_c_name(b"a\0b", |_| Ok(()));
        assert_eq!(nul.unwrap_err().kind(), io::ErrorKind::InvalidInput);
    }
}
