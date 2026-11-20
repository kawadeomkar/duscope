//! The macOS calls the provider makes, as safe wrappers, and the constants
//! and types of `<sys/attr.h>`, `<sys/stat.h>`, `<sys/vnode.h>`,
//! `<sys/resource.h>` and `<sys/proc_info.h>` that the `libc` crate does
//! not declare. Every `unsafe`
//! block of the macOS provider is in this file; the `FSEvents`,
//! `CoreFoundation` and libdispatch calls, which `libc` does not declare,
//! are in `fsevents.rs`.

use std::ffi::CStr;
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, BorrowedFd};
use std::time::Duration;

use libc::{attrgroup_t, c_int};

/// `ATTR_CMN_ERROR`: a per-entry error in a bulk read (`<sys/attr.h>`).
pub(super) const ATTR_CMN_ERROR: attrgroup_t = 0x2000_0000;

/// `SF_FIRMLINK`: the entry is a firmlink into the Data volume
/// (`<sys/stat.h>`).
pub(super) const SF_FIRMLINK: u32 = 0x0080_0000;
/// `SF_DATALESS`: a placeholder whose data is not on the disk.
pub(super) const SF_DATALESS: u32 = 0x4000_0000;

/// The extended flags of `ATTR_CMNEXT_EXT_FLAGS` (`<sys/stat.h>`).
pub(super) const EF_MAY_SHARE_BLOCKS: u64 = 0x0000_0001;
pub(super) const EF_IS_PURGEABLE: u64 = 0x0000_0008;
pub(super) const EF_IS_SPARSE: u64 = 0x0000_0010;
pub(super) const EF_SHARES_ALL_BLOCKS: u64 = 0x0000_0040;

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
    #[cfg(test)]
    crate::platform::posix::calls::bulk();
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
    #[cfg(test)]
    crate::platform::posix::calls::entry();
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
    #[cfg(test)]
    crate::platform::posix::calls::entry();
    let mut attrs = *attrs;
    let len = buf.len();
    let ptr = buf.as_mut_ptr();
    // SAFETY: as in getattrlistat.
    retry(|| unsafe {
        libc::getattrlist(path.as_ptr(), (&raw mut attrs).cast(), ptr, len, options)
    })?;
    Ok(())
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

/// `pathconf(2)` of `path`; `None` when the value is unknown or the call
/// fails.
pub(super) fn pathconf(path: &CStr, name: c_int) -> Option<libc::c_long> {
    // SAFETY: `path` is NUL-terminated and outlives the call.
    let value = unsafe { libc::pathconf(path.as_ptr(), name) };
    (value >= 0).then_some(value)
}

/// The device a `statfs` record's `f_fsid` names: its first word, which
/// macOS fills with the volume's own device (the device bulk reads report
/// under `FSOPT_RETURN_REALDEV`).
pub(super) fn fsid_dev(fs: &libc::statfs) -> u64 {
    // SAFETY: fsid_t is `#[repr(C)] struct { [i32; 2] }` in <sys/mount.h>
    // and in libc, so it has the layout of two i32s; libc keeps the field
    // private, hence the copy.
    let words: [i32; 2] = unsafe { std::mem::transmute(fs.f_fsid) };
    super::bulk::dev(words[0])
}

/// Every mounted volume as the kernel last recorded it (`getfsstat(2)`
/// with `MNT_NOWAIT`): no volume is asked for fresh statistics, so a dead
/// network mount cannot block the call.
pub(super) fn mounted_volumes() -> io::Result<Vec<libc::statfs>> {
    loop {
        // SAFETY: a null buffer asks only for the count.
        let count = unsafe { libc::getfsstat(std::ptr::null_mut(), 0, libc::MNT_NOWAIT) };
        let count = usize::try_from(count).map_err(|_| io::Error::last_os_error())?;
        // Room for a few mounts appearing between the two calls.
        let capacity = count + 4;
        let mut buf: Vec<libc::statfs> = Vec::with_capacity(capacity);
        let bytes = libc::c_int::try_from(capacity * std::mem::size_of::<libc::statfs>())
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        // SAFETY: `buf` has room for `capacity` structs, `bytes` long.
        let filled = unsafe { libc::getfsstat(buf.as_mut_ptr(), bytes, libc::MNT_NOWAIT) };
        let filled = usize::try_from(filled).map_err(|_| io::Error::last_os_error())?;
        if filled < capacity {
            // SAFETY: getfsstat initialised the first `filled` structs.
            unsafe { buf.set_len(filled) };
            return Ok(buf);
        }
    }
}

/// `fstatfs(2)` of the volume holding the file open on `fd`.
pub(super) fn fstatfs(fd: BorrowedFd<'_>) -> io::Result<libc::statfs> {
    let mut stat = MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: `fd` is open for the borrow and `stat` is writable for one
    // `struct statfs`.
    retry(|| unsafe { libc::fstatfs(fd.as_raw_fd(), stat.as_mut_ptr()) })?;
    // SAFETY: fstatfs succeeded, so it filled the struct.
    Ok(unsafe { stat.assume_init() })
}

/// The path of the file open on `fd` with no firmlink in it
/// (`F_GETPATH_NOFIRMLINK`): `/System/Volumes/Data/Users/a` for what
/// `/Users/a` reaches, the spelling `FSEvents` uses below a volume's mount
/// point.
pub(super) fn nofirmlink_path(fd: BorrowedFd<'_>) -> io::Result<Vec<u8>> {
    let mut buf = vec![0u8; libc::PATH_MAX as usize];
    // SAFETY: `fd` is open for the borrow, and F_GETPATH_NOFIRMLINK writes
    // at most MAXPATHLEN (PATH_MAX) bytes, a NUL included, into `buf`,
    // which is that long.
    retry(|| unsafe {
        libc::fcntl(
            fd.as_raw_fd(),
            libc::F_GETPATH_NOFIRMLINK,
            buf.as_mut_ptr().cast::<libc::c_char>(),
        )
    })?;
    let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    buf.truncate(len);
    Ok(buf)
}

/// The build of the running system (`kern.osversion`, such as `25G332`);
/// `None` when it cannot be read.
pub(super) fn os_build() -> Option<String> {
    let name = c"kern.osversion";
    let mut buf = [0u8; 64];
    let mut len = buf.len();
    // SAFETY: `name` is NUL-terminated; sysctlbyname writes at most `len`
    // bytes into `buf`, which is that long, and stores the length it wrote
    // in `len`; no new value is given.
    let rc = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            buf.as_mut_ptr().cast(),
            &raw mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return None;
    }
    let value = buf.get(..len)?;
    let value = value.split(|&b| b == 0).next()?;
    let build = std::str::from_utf8(value).ok()?;
    (!build.is_empty()).then(|| build.to_owned())
}

/// `PROC_UID_ONLY` (`<sys/proc_info.h>`): `proc_listpids` lists the
/// processes of one effective user.
const PROC_UID_ONLY: u32 = 4;
/// `PROC_PIDFDVNODEPATHINFO` (`<sys/proc_info.h>`): a vnode descriptor's
/// open flags, vnode and path.
const PROC_PIDFDVNODEPATHINFO: c_int = 2;
/// `FWRITE` (`<sys/fcntl.h>`): a descriptor open for writing, among the
/// kernel's open flags that `proc_fileinfo` reports.
pub(super) const FWRITE: u32 = 0x0002;

/// `struct proc_fileinfo` (`<sys/proc_info.h>`).
#[repr(C)]
#[allow(dead_code, clippy::struct_field_names)] // the kernel's layout and names, of which the open flags are read
struct ProcFileInfo {
    fi_openflags: u32,
    fi_status: u32,
    fi_offset: libc::off_t,
    fi_type: i32,
    fi_guardflags: u32,
}

/// `struct vnode_fdinfowithpath` (`<sys/proc_info.h>`), 1,200 bytes.
#[repr(C)]
pub(super) struct VnodeFdInfoWithPath {
    pfi: ProcFileInfo,
    pvip: libc::vnode_info_path,
}

/// The processes whose effective user is `uid`, as `proc_listpids` lists
/// them.
pub(super) fn pids_of_user(uid: libc::uid_t) -> io::Result<Vec<libc::pid_t>> {
    let width = size_of::<libc::pid_t>();
    // SAFETY: with no buffer, proc_listpids writes nothing and returns the
    // bytes the list needs.
    let need = unsafe { libc::proc_listpids(PROC_UID_ONLY, uid, std::ptr::null_mut(), 0) };
    let need = usize::try_from(need).map_err(|_| io::Error::last_os_error())?;
    // Room for the processes started since the size was asked.
    let mut pids: Vec<libc::pid_t> = vec![0; need / width + 64];
    let bytes = c_int::try_from(pids.len() * width).unwrap_or(c_int::MAX);
    // SAFETY: `pids` holds `bytes` bytes, the most proc_listpids writes.
    let got = unsafe { libc::proc_listpids(PROC_UID_ONLY, uid, pids.as_mut_ptr().cast(), bytes) };
    let got = usize::try_from(got).map_err(|_| io::Error::last_os_error())?;
    pids.truncate(got / width);
    pids.retain(|&pid| pid > 0);
    Ok(pids)
}

/// The descriptors process `pid` holds, as `PROC_PIDLISTFDS` lists them.
pub(super) fn fds_of(pid: libc::pid_t) -> io::Result<Vec<libc::proc_fdinfo>> {
    let width = size_of::<libc::proc_fdinfo>();
    // SAFETY: with no buffer, proc_pidinfo writes nothing and returns the
    // bytes the list needs, or 0 with errno set.
    let need =
        unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, std::ptr::null_mut(), 0) };
    if need <= 0 {
        return Err(io::Error::last_os_error());
    }
    let need = usize::try_from(need).unwrap_or(0);
    // Room for the descriptors opened since the size was asked.
    let empty = libc::proc_fdinfo {
        proc_fd: 0,
        proc_fdtype: 0,
    };
    let mut fds = vec![empty; need / width + 16];
    let bytes = c_int::try_from(fds.len() * width).unwrap_or(c_int::MAX);
    // SAFETY: `fds` holds `bytes` bytes, the most proc_pidinfo writes.
    let got = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDLISTFDS,
            0,
            fds.as_mut_ptr().cast(),
            bytes,
        )
    };
    if got <= 0 {
        return Err(io::Error::last_os_error());
    }
    fds.truncate(usize::try_from(got).unwrap_or(0) / width);
    Ok(fds)
}

/// What a vnode descriptor of a process is.
#[derive(Debug)]
pub(super) struct VnodeFd {
    /// The kernel's open flags: [`FWRITE`] and the rest.
    pub(super) open_flags: u32,
    /// The vnode's type: [`VREG`] for a regular file.
    pub(super) vtype: u32,
    /// The path the kernel builds for the vnode, firmlinks taken.
    pub(super) path: Vec<u8>,
}

/// The descriptor `fd` of process `pid`, a vnode, as
/// `PROC_PIDFDVNODEPATHINFO` reports it.
pub(super) fn vnode_fd(pid: libc::pid_t, fd: i32) -> io::Result<VnodeFd> {
    let size = c_int::try_from(size_of::<VnodeFdInfoWithPath>()).unwrap_or(c_int::MAX);
    // SAFETY: every field of the struct is an integer or an array of them,
    // for which zero bytes are a value; proc_pidfdinfo writes at most `size`
    // bytes, the struct's own, into it.
    let (got, info) = unsafe {
        let mut info = MaybeUninit::<VnodeFdInfoWithPath>::zeroed().assume_init();
        let got = libc::proc_pidfdinfo(
            pid,
            fd,
            PROC_PIDFDVNODEPATHINFO,
            (&raw mut info).cast(),
            size,
        );
        (got, info)
    };
    if got != size {
        return Err(io::Error::last_os_error());
    }
    let path: Vec<u8> = info
        .pvip
        .vip_path
        .as_flattened()
        .iter()
        .map(|&c| c.cast_unsigned())
        .take_while(|&b| b != 0)
        .collect();
    Ok(VnodeFd {
        open_flags: info.pfi.fi_openflags,
        vtype: info.pvip.vip_vi.vi_type.cast_unsigned(),
        path,
    })
}

/// Asks for `QOS_CLASS_USER_INITIATED` for the calling thread.
pub(super) fn set_user_initiated_qos() -> io::Result<()> {
    // SAFETY: pthread_set_qos_class_self_np takes a class and a relative
    // priority by value and changes the calling thread's scheduling alone.
    let rc = unsafe {
        libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INITIATED, 0)
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(rc))
    }
}

/// The calling thread's `QoS` class, as its `qos_class_t` value.
#[cfg(test)]
pub(super) fn qos_class() -> io::Result<u32> {
    let mut class = 0u32;
    let mut priority: c_int = 0;
    // SAFETY: pthread_self names the calling thread, which is running; the
    // call writes a qos_class_t, a u32, into `class` and an int into
    // `priority`, both ours and live for the call. The class is read as the
    // u32 it is, never as the enum.
    let rc = unsafe {
        libc::pthread_get_qos_class_np(
            libc::pthread_self(),
            (&raw mut class).cast(),
            &raw mut priority,
        )
    };
    if rc == 0 {
        Ok(class)
    } else {
        Err(io::Error::from_raw_os_error(rc))
    }
}
