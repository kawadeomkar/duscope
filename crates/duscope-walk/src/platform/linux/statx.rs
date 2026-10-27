//! Reading entries with `statx(2)` relative to their directory's descriptor:
//! `AT_SYMLINK_NOFOLLOW | AT_STATX_DONT_SYNC` and the basic mask.

use std::ffi::{CStr, CString};
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, BorrowedFd};
use std::os::raw::c_int;

use crate::{Attrs, Boundary, DirId, Entry, Flags, FullAttrs, Kind, NameRef, Share, WalkOptions};

/// The basic mask: type, size, blocks, inode, links, both times and the
/// mount id: the unique one where the kernel has it (6.8), the reusable one
/// before (5.8); older kernels report neither.
pub(super) const BASIC: u32 = libc::STATX_TYPE
    | libc::STATX_SIZE
    | libc::STATX_BLOCKS
    | libc::STATX_INO
    | libc::STATX_NLINK
    | libc::STATX_MTIME
    | libc::STATX_CTIME
    | libc::STATX_MNT_ID
    | libc::STATX_MNT_ID_UNIQUE;

/// What `--attrs full` adds: access and creation times, mode, owner and
/// group, in the same call.
pub(super) const FULL: u32 =
    libc::STATX_ATIME | libc::STATX_BTIME | libc::STATX_MODE | libc::STATX_UID | libc::STATX_GID;

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
    Ok(entry(
        name.clone(),
        &statx(Some(dir), &c_name, mask(opts))?,
        opts.attrs,
    ))
}

/// The mask for `opts`.
pub(super) fn mask(opts: &WalkOptions) -> u32 {
    match opts.attrs {
        Attrs::Basic => BASIC,
        Attrs::Full => BASIC | FULL,
    }
}

/// The kind of an `st_mode` / `stx_mode`.
fn kind(mode: u32) -> Kind {
    match mode & libc::S_IFMT {
        libc::S_IFDIR => Kind::Dir,
        libc::S_IFREG => Kind::File,
        libc::S_IFLNK => Kind::Symlink,
        libc::S_IFSOCK => Kind::Socket,
        libc::S_IFIFO => Kind::Fifo,
        libc::S_IFCHR => Kind::Char,
        libc::S_IFBLK => Kind::Block,
        _ => Kind::Unknown,
    }
}

/// The bytes allocated to an entry of `kind` with `blocks` 512-byte
/// blocks: devices, sockets and pipes hold none of their own.
fn allocated(kind: Kind, blocks: u64) -> u64 {
    match kind {
        Kind::Socket | Kind::Fifo | Kind::Char | Kind::Block => 0,
        _ => blocks.saturating_mul(512),
    }
}

/// Whether a non-directory with `nlink` links may share its blocks.
fn share(kind: Kind, nlink: u64) -> Share {
    if kind != Kind::Dir && nlink > 1 {
        Share::Hardlink
    } else {
        Share::None
    }
}

/// The entry a `statx` describes, with the `--attrs full` set under
/// `attrs`. A filesystem without creation times leaves `btime` empty.
pub(super) fn entry(name: NameRef, stx: &libc::statx, attrs: Attrs) -> Entry {
    let kind = kind(u32::from(stx.stx_mode));
    let attribute = |bit: c_int| {
        let bit = u64::from(bit.cast_unsigned());
        stx.stx_attributes_mask & bit != 0 && stx.stx_attributes & bit != 0
    };
    let mut flags = Flags::empty();
    flags.set(Flags::COMPRESSED, attribute(libc::STATX_ATTR_COMPRESSED));
    let mount_root = attribute(libc::STATX_ATTR_MOUNT_ROOT);
    flags.set(Flags::MOUNT_ROOT, mount_root);
    let mnt_id = if stx.stx_mask & (libc::STATX_MNT_ID | libc::STATX_MNT_ID_UNIQUE) != 0 {
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
        allocated: allocated(kind, stx.stx_blocks),
        mtime_ns: ns(stx.stx_mtime),
        ctime_ns: ns(stx.stx_ctime),
        nlink: stx.stx_nlink,
        dev: libc::makedev(stx.stx_dev_major, stx.stx_dev_minor),
        ino: stx.stx_ino,
        mnt_id,
        share: share(kind, u64::from(stx.stx_nlink)),
        boundary: if mount_root {
            Boundary::Mount
        } else {
            Boundary::None
        },
        flags,
        full: (attrs == Attrs::Full).then(|| FullAttrs {
            atime_ns: ns(stx.stx_atime),
            btime_ns: (stx.stx_mask & libc::STATX_BTIME != 0).then(|| ns(stx.stx_btime)),
            mode: u32::from(stx.stx_mode),
            uid: stx.stx_uid,
            gid: stx.stx_gid,
        }),
    }
}

/// The entry an `lstat` describes: what `statx` reports, less the mount id,
/// the attribute flags and the creation time, which `stat` does not carry.
pub(in crate::platform) fn from_stat(name: NameRef, stat: &libc::stat, attrs: Attrs) -> Entry {
    let kind = kind(stat.st_mode);
    // `st_nlink` is 64-bit on x86_64 and 32-bit on aarch64.
    #[allow(clippy::useless_conversion)]
    let nlink = u64::from(stat.st_nlink);
    Entry {
        name,
        parent: DirId::ROOT,
        dir: None,
        kind,
        apparent: u64::try_from(stat.st_size).unwrap_or(0),
        allocated: allocated(kind, u64::try_from(stat.st_blocks).unwrap_or(0)),
        mtime_ns: time_ns(stat.st_mtime, stat.st_mtime_nsec),
        ctime_ns: time_ns(stat.st_ctime, stat.st_ctime_nsec),
        nlink: u32::try_from(nlink).unwrap_or(u32::MAX),
        dev: stat.st_dev,
        ino: stat.st_ino,
        mnt_id: 0,
        share: share(kind, nlink),
        boundary: Boundary::None,
        flags: Flags::empty(),
        full: (attrs == Attrs::Full).then(|| FullAttrs {
            atime_ns: time_ns(stat.st_atime, stat.st_atime_nsec),
            btime_ns: None,
            mode: stat.st_mode,
            uid: stat.st_uid,
            gid: stat.st_gid,
        }),
    }
}

/// A `statx` time as Unix nanoseconds, saturating at the ends of `i64`.
fn ns(time: libc::statx_timestamp) -> i64 {
    time_ns(time.tv_sec, i64::from(time.tv_nsec))
}

/// Seconds and nanoseconds as Unix nanoseconds, saturating at the ends of
/// `i64`.
fn time_ns(sec: i64, nsec: i64) -> i64 {
    let ns = i128::from(sec) * 1_000_000_000 + i128::from(nsec);
    i64::try_from(ns).unwrap_or(if ns < 0 { i64::MIN } else { i64::MAX })
}
