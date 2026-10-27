//! Reading entries with `statx(2)` relative to their directory's descriptor:
//! `AT_SYMLINK_NOFOLLOW | AT_STATX_DONT_SYNC` and the basic mask.

use std::ffi::{CStr, CString};
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, BorrowedFd};
use std::os::raw::c_int;

use crate::{Boundary, DirId, Entry, Flags, Kind, NameRef, Share, WalkOptions};

/// The basic mask: type, size, blocks, inode, links, both times and the
/// mount id (5.8; older kernels leave it out of `stx_mask`).
pub(super) const BASIC: u32 = libc::STATX_TYPE
    | libc::STATX_SIZE
    | libc::STATX_BLOCKS
    | libc::STATX_INO
    | libc::STATX_NLINK
    | libc::STATX_MTIME
    | libc::STATX_CTIME
    | libc::STATX_MNT_ID;

/// Never follow a final symlink; on NFS and SMB, take the client's cached
/// attributes instead of revalidating each entry with the server.
const FLAGS: c_int = libc::AT_SYMLINK_NOFOLLOW | libc::AT_STATX_DONT_SYNC;

/// `statx(2)` of `name` relative to `dir` (`None`: the working directory).
pub(super) fn statx(
    dir: Option<BorrowedFd<'_>>,
    name: &CStr,
    mask: u32,
) -> io::Result<libc::statx> {
    let at = dir.map_or(libc::AT_FDCWD, |fd| fd.as_raw_fd());
    let mut buf = MaybeUninit::<libc::statx>::uninit();
    loop {
        // SAFETY: `at` is AT_FDCWD or a descriptor open for the borrow,
        // `name` is NUL-terminated, and `buf` is writable for one statx.
        let rc = unsafe { libc::statx(at, name.as_ptr(), FLAGS, mask, buf.as_mut_ptr()) };
        if rc == 0 {
            // SAFETY: statx succeeded, so it filled the struct.
            return Ok(unsafe { buf.assume_init() });
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

/// The entry `name` in the directory open on `dir`.
pub(super) fn entry_at(
    dir: BorrowedFd<'_>,
    name: &NameRef,
    opts: &WalkOptions,
) -> io::Result<Entry> {
    let c_name = CString::new(name.as_bytes()).map_err(|_| io::ErrorKind::InvalidInput)?;
    Ok(entry(name.clone(), &statx(Some(dir), &c_name, mask(opts))?))
}

/// The mask for `opts`.
pub(super) fn mask(_opts: &WalkOptions) -> u32 {
    BASIC
}

/// The entry a `statx` describes.
pub(super) fn entry(name: NameRef, stx: &libc::statx) -> Entry {
    let kind = match u32::from(stx.stx_mode) & libc::S_IFMT {
        libc::S_IFDIR => Kind::Dir,
        libc::S_IFREG => Kind::File,
        libc::S_IFLNK => Kind::Symlink,
        libc::S_IFSOCK => Kind::Socket,
        libc::S_IFIFO => Kind::Fifo,
        libc::S_IFCHR => Kind::Char,
        libc::S_IFBLK => Kind::Block,
        _ => Kind::Unknown,
    };
    // Devices, sockets and pipes hold no blocks of their own.
    let allocated = match kind {
        Kind::Socket | Kind::Fifo | Kind::Char | Kind::Block => 0,
        _ => stx.stx_blocks.saturating_mul(512),
    };
    let share = if kind != Kind::Dir && stx.stx_nlink > 1 {
        Share::Hardlink
    } else {
        Share::None
    };
    let attribute = |bit: c_int| {
        let bit = u64::from(bit.cast_unsigned());
        stx.stx_attributes_mask & bit != 0 && stx.stx_attributes & bit != 0
    };
    let mut flags = Flags::empty();
    flags.set(Flags::COMPRESSED, attribute(libc::STATX_ATTR_COMPRESSED));
    let mount_root = attribute(libc::STATX_ATTR_MOUNT_ROOT);
    flags.set(Flags::MOUNT_ROOT, mount_root);
    let mnt_id = if stx.stx_mask & libc::STATX_MNT_ID != 0 {
        stx.stx_mnt_id
    } else {
        0
    };
    Entry {
        name,
        parent: DirId::ROOT,
        dir: None,
        kind,
        apparent: stx.stx_size,
        allocated,
        mtime_ns: ns(stx.stx_mtime),
        ctime_ns: ns(stx.stx_ctime),
        nlink: stx.stx_nlink,
        dev: libc::makedev(stx.stx_dev_major, stx.stx_dev_minor),
        ino: stx.stx_ino,
        mnt_id,
        share,
        boundary: if mount_root {
            Boundary::Mount
        } else {
            Boundary::None
        },
        flags,
        full: None,
    }
}

/// A `statx` time as Unix nanoseconds, saturating at the ends of `i64`.
fn ns(time: libc::statx_timestamp) -> i64 {
    let ns = i128::from(time.tv_sec) * 1_000_000_000 + i128::from(time.tv_nsec);
    i64::try_from(ns).unwrap_or(if ns < 0 { i64::MIN } else { i64::MAX })
}
