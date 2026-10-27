//! Calls both platforms make the same way: opening a directory to read and
//! raising the descriptor limit.

use std::ffi::{CStr, CString};
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, IntoRawFd, OwnedFd};
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

/// Opens `path` for reading its entries. The final component is not
/// followed, so a directory replaced by a symlink after it was listed fails
/// with `ELOOP` rather than leading the walk elsewhere. On `EMFILE` or
/// `ENFILE` the worker parks, retrying with a growing pause, for up to two
/// seconds.
pub(crate) fn open_dir(path: &Path) -> io::Result<OwnedFd> {
    let path = c_path(path)?;
    let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    // SAFETY: `path` is a NUL-terminated C string that outlives the call.
    parked(|| unsafe { libc::open(path.as_ptr(), flags) })
}

/// The descriptor `open` returns, retrying on `EINTR` and parking on
/// `EMFILE` or `ENFILE` with a growing pause, for up to [`PARK_LIMIT`] in
/// all, until another worker closes one.
fn parked(mut open: impl FnMut() -> libc::c_int) -> io::Result<OwnedFd> {
    let mut parked = Duration::ZERO;
    let mut pause = Duration::from_millis(1);
    loop {
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

/// `fstatat(2)` of `name` in the directory open on `dir`, not following a
/// final symlink.
pub(crate) fn lstat_at(dir: BorrowedFd<'_>, name: &CStr) -> io::Result<libc::stat> {
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `dir` is open for the borrow, `name` is NUL-terminated, and
    // `stat` is writable for one `struct stat`.
    retry(|| unsafe {
        libc::fstatat(
            dir.as_raw_fd(),
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    })?;
    // SAFETY: fstatat succeeded, so it filled the struct.
    Ok(unsafe { stat.assume_init() })
}

/// `lstat(2)` of `path`.
pub(crate) fn lstat(path: &CStr) -> io::Result<libc::stat> {
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

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::io;
    use std::path::PathBuf;
    use std::thread;
    use std::time::{Duration, Instant};

    use duscope_testkit::TestEnv;

    use std::os::fd::AsFd;

    use super::{NOFILE_TARGET, OPEN_MAX, list_names, open_dir, raise_nofile_limit, with_c_name};

    /// The directory the child process opens under a full descriptor table.
    const CHILD_DIR: &str = "DUSCOPE_TEST_EMFILE_DIR";

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
