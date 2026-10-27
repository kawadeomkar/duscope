//! Calls both platforms make the same way: opening a directory to read and
//! raising the descriptor limit.

use std::ffi::CString;
use std::io;
use std::os::fd::{FromRawFd, OwnedFd};
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
pub(crate) fn statvfs(path: &std::ffi::CStr) -> io::Result<libc::statvfs> {
    let mut buf = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `path` is NUL-terminated and `buf` is writable for one
    // `struct statvfs`.
    if unsafe { libc::statvfs(path.as_ptr(), buf.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: statvfs succeeded, so it filled the struct.
    Ok(unsafe { buf.assume_init() })
}

#[cfg(test)]
mod tests {
    use std::io;

    use super::with_c_name;

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
