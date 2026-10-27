//! The macOS calls the provider makes, as safe wrappers, and the constants
//! of `<sys/attr.h>`, `<sys/stat.h>`, `<sys/vnode.h>` and
//! `<sys/resource.h>` that the `libc` crate does not declare. Every `unsafe`
//! block of the macOS provider is in this file.

use std::ffi::CStr;
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, BorrowedFd};
use std::time::Duration;

use libc::{attrgroup_t, c_int};

/// `ATTR_CMN_ERROR`: a per-entry error in a bulk read (`<sys/attr.h>`).
pub(super) const ATTR_CMN_ERROR: attrgroup_t = 0x2000_0000;

/// `SF_DATALESS`: a placeholder whose data is not on the disk
/// (`<sys/stat.h>`).
pub(super) const SF_DATALESS: u32 = 0x4000_0000;

/// The vnode types of `ATTR_CMN_OBJTYPE` (`enum vtype`, `<sys/vnode.h>`).
pub(super) const VREG: u32 = 1;
pub(super) const VDIR: u32 = 2;
pub(super) const VBLK: u32 = 3;
pub(super) const VCHR: u32 = 4;
pub(super) const VLNK: u32 = 5;
pub(super) const VSOCK: u32 = 6;
pub(super) const VFIFO: u32 = 7;

/// `<sys/resource.h>`: the I/O policy that decides whether touching a
/// dataless file downloads it, set process-wide to off.
const IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES: c_int = 3;
const IOPOL_SCOPE_PROCESS: c_int = 0;
pub(super) const IOPOL_MATERIALIZE_DATALESS_FILES_OFF: c_int = 1;

// SAFETY: the declarations match `<sys/resource.h>`; both functions take
// and return plain integers.
unsafe extern "C" {
    fn setiopolicy_np(iotype: c_int, scope: c_int, policy: c_int) -> c_int;
    #[cfg(test)]
    fn getiopolicy_np(iotype: c_int, scope: c_int) -> c_int;
}

/// Stops this process from downloading dataless files when it reads their
/// attributes (TN3150), so a scan inside an iCloud, Dropbox or `OneDrive`
/// folder never materialises a placeholder.
pub(super) fn disable_dataless_materialization() -> io::Result<()> {
    // SAFETY: setiopolicy_np takes three integers and touches no memory of ours.
    let rc = unsafe {
        setiopolicy_np(
            IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES,
            IOPOL_SCOPE_PROCESS,
            IOPOL_MATERIALIZE_DATALESS_FILES_OFF,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// The process's dataless-materialisation policy.
#[cfg(test)]
pub(super) fn dataless_policy() -> io::Result<c_int> {
    // SAFETY: getiopolicy_np takes two integers and touches no memory of ours.
    let policy = unsafe {
        getiopolicy_np(
            IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES,
            IOPOL_SCOPE_PROCESS,
        )
    };
    if policy < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(policy)
    }
}

/// The CPU time the calling thread has used, user and system; `None` when
/// the clock cannot be read.
pub(super) fn thread_cpu_time() -> Option<Duration> {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `time` is a valid, writable timespec, and
    // CLOCK_THREAD_CPUTIME_ID names the calling thread.
    if unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &raw mut time) } != 0 {
        return None;
    }
    Some(Duration::new(
        time.tv_sec.try_into().ok()?,
        time.tv_nsec.try_into().ok()?,
    ))
}

/// A buffer for packed attribute records, which must start on an 8-byte
/// boundary: it is held as `u64`s and read as bytes.
pub(super) struct AttrBuf(Box<[u64]>);

impl AttrBuf {
    /// A zeroed buffer of `bytes` bytes, rounded up to a multiple of 8.
    pub(super) fn new(bytes: usize) -> AttrBuf {
        AttrBuf(vec![0; bytes.div_ceil(8)].into_boxed_slice())
    }

    /// The buffer's bytes.
    pub(super) fn bytes(&self) -> &[u8] {
        // SAFETY: the slice covers exactly the `u64` allocation, any bit
        // pattern is a valid `u8`, `u8` has no alignment requirement, and
        // the borrow of `self` keeps the allocation alive and unaliased.
        unsafe { std::slice::from_raw_parts(self.0.as_ptr().cast::<u8>(), self.0.len() * 8) }
    }

    fn as_mut_ptr(&mut self) -> *mut libc::c_void {
        self.0.as_mut_ptr().cast()
    }

    fn len(&self) -> usize {
        self.0.len() * 8
    }
}

impl std::fmt::Debug for AttrBuf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "AttrBuf({} bytes)", self.len())
    }
}

/// Retries a call that returns -1 with `errno` on `EINTR`.
fn retry(mut call: impl FnMut() -> c_int) -> io::Result<c_int> {
    loop {
        let rc = call();
        if rc >= 0 {
            return Ok(rc);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

/// `getattrlistbulk(2)`: fills `buf` with the next records of the directory
/// open on `dir`, returning how many; 0 at the end.
pub(super) fn getattrlistbulk(
    dir: BorrowedFd<'_>,
    attrs: &libc::attrlist,
    buf: &mut AttrBuf,
    options: u64,
) -> io::Result<usize> {
    let mut attrs = *attrs;
    let len = buf.len();
    let ptr = buf.as_mut_ptr();
    // SAFETY: `dir` is an open descriptor for the borrow, `attrs` is an
    // initialised attrlist the kernel only reads, and `ptr` is writable for
    // `len` bytes and 8-byte aligned.
    let count = retry(|| unsafe {
        libc::getattrlistbulk(dir.as_raw_fd(), (&raw mut attrs).cast(), ptr, len, options)
    })?;
    Ok(usize::try_from(count).unwrap_or(0))
}

/// `getattrlistat(2)`: the attributes of `name` in the directory open on
/// `dir`, as one record at the start of `buf`.
pub(super) fn getattrlistat(
    dir: BorrowedFd<'_>,
    name: &CStr,
    attrs: &libc::attrlist,
    buf: &mut AttrBuf,
    options: u32,
) -> io::Result<()> {
    let mut attrs = *attrs;
    let len = buf.len();
    let ptr = buf.as_mut_ptr();
    // SAFETY: as in getattrlistbulk; `name` is NUL-terminated and outlives
    // the call.
    retry(|| unsafe {
        libc::getattrlistat(
            dir.as_raw_fd(),
            name.as_ptr(),
            (&raw mut attrs).cast(),
            ptr,
            len,
            libc::c_ulong::from(options),
        )
    })?;
    Ok(())
}

/// `getattrlist(2)`: the attributes of `path`, as one record at the start
/// of `buf`.
pub(super) fn getattrlist(
    path: &CStr,
    attrs: &libc::attrlist,
    buf: &mut AttrBuf,
    options: u32,
) -> io::Result<()> {
    let mut attrs = *attrs;
    let len = buf.len();
    let ptr = buf.as_mut_ptr();
    // SAFETY: as in getattrlistat.
    retry(|| unsafe {
        libc::getattrlist(path.as_ptr(), (&raw mut attrs).cast(), ptr, len, options)
    })?;
    Ok(())
}

/// `fstatat(2)` of `name` in the directory open on `dir`, not following a
/// final symlink.
pub(super) fn lstat_at(dir: BorrowedFd<'_>, name: &CStr) -> io::Result<libc::stat> {
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
pub(super) fn lstat(path: &CStr) -> io::Result<libc::stat> {
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `path` is NUL-terminated and `stat` is writable for one
    // `struct stat`.
    retry(|| unsafe { libc::lstat(path.as_ptr(), stat.as_mut_ptr()) })?;
    // SAFETY: lstat succeeded, so it filled the struct.
    Ok(unsafe { stat.assume_init() })
}

/// `statfs(2)` of the volume holding `path`.
pub(super) fn statfs(path: &CStr) -> io::Result<libc::statfs> {
    let mut stat = MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: `path` is NUL-terminated and `stat` is writable for one
    // `struct statfs`.
    retry(|| unsafe { libc::statfs(path.as_ptr(), stat.as_mut_ptr()) })?;
    // SAFETY: statfs succeeded, so it filled the struct.
    Ok(unsafe { stat.assume_init() })
}
