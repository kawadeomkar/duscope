//! Calls both platforms make the same way: opening a directory to read and
//! raising the descriptor limit.

pub(crate) mod signal;
pub(crate) mod winsize;

use std::ffi::{CStr, CString};
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::Once;
use std::thread;
use std::time::Duration;

/// The descriptor limit a scan asks for (spec: `min(hard, 65536)`).
const NOFILE_TARGET: libc::rlim_t = 65_536;

/// macOS refuses a soft limit above `OPEN_MAX` unless the kernel's
/// per-process maximum allows it; this is the retry when the target is
/// refused.
const OPEN_MAX: libc::rlim_t = 10_240;

/// How long a worker waits in total for a descriptor to free up before the
/// directory is recorded as unreadable with `EMFILE`.
const PARK_LIMIT: Duration = Duration::from_secs(2);

/// Raises the soft `RLIMIT_NOFILE` to `min(hard, 65536)`, once per process.
/// A refusal leaves the limit as it was: the walk then parks workers on
/// `EMFILE` instead.
pub(crate) fn raise_nofile_limit() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: `limit` is a valid, writable rlimit for getrlimit to fill.
        if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) } != 0 {
            return;
        }
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
    });
}

/// The path as a C string. A path from a directory listing holds no NUL.
pub(crate) fn c_path(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes()).map_err(|_| io::ErrorKind::InvalidInput.into())
}

/// Opens `path` for reading its entries. The final component is not
/// followed, so a directory replaced by a symlink after it was listed fails
/// with `ELOOP` rather than leading the walk elsewhere. On `EMFILE` or
/// `ENFILE` the worker parks, retrying with a growing pause, for up to two
/// seconds.
pub(crate) fn open_dir(path: &Path) -> io::Result<OwnedFd> {
    let path = c_path(path)?;
    let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    let mut parked = Duration::ZERO;
    let mut pause = Duration::from_millis(1);
    loop {
        // SAFETY: `path` is a NUL-terminated C string that outlives the call.
        let fd = unsafe { libc::open(path.as_ptr(), flags) };
        if fd >= 0 {
            // SAFETY: `fd` was just returned by open and is owned by nothing else.
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

/// The names in the directory open on `dir`, without `.` and `..`, read
/// with `readdir` through a duplicate of the descriptor, so the listing is
/// of the directory the descriptor holds, not of whatever its path names
/// by now. The listing starts from the beginning, wherever an earlier read
/// of the descriptor left off.
pub(crate) fn list_names(dir: BorrowedFd<'_>) -> io::Result<Vec<crate::NameRef>> {
    // SAFETY: `dir` is open for the borrow; F_DUPFD_CLOEXEC returns a new
    // descriptor or -1.
    let dup = unsafe { libc::fcntl(dir.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
    if dup < 0 {
        return Err(io::Error::last_os_error());
    }
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
    let mut names = Vec::new();
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
            names.push(crate::NameRef::new(name));
        }
    };
    // SAFETY: `stream` came from fdopendir and is closed once.
    unsafe { libc::closedir(stream) };
    result.map(|()| names)
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

/// Per-thread counts of the calls that read entries, for the tests that
/// count them: bulk reads, and reads of one entry.
#[cfg(test)]
// Only the macOS tests count bulk reads.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) mod calls {
    use std::cell::Cell;

    thread_local! {
        pub(crate) static BULK: Cell<usize> = const { Cell::new(0) };
        pub(crate) static ENTRY: Cell<usize> = const { Cell::new(0) };
    }

    pub(crate) fn bulk() {
        BULK.with(|n| n.set(n.get() + 1));
    }

    pub(crate) fn entry() {
        ENTRY.with(|n| n.set(n.get() + 1));
    }

    /// This thread's counts so far: (bulk, entry).
    pub(crate) fn now() -> (usize, usize) {
        (BULK.with(Cell::get), ENTRY.with(Cell::get))
    }
}
