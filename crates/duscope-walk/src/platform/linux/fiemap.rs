//! `explain --clones` on btrfs and XFS: a file's extent map through the
//! `FS_IOC_FIEMAP` ioctl, and which of its extents the filesystem shares
//! with another file or a snapshot (`FIEMAP_EXTENT_SHARED`: a reflink copy,
//! a deduplicated range, a snapshot's data).
//!
//! Read access only, on demand: the file is opened read-only and never read;
//! the ioctl reads the filesystem's own map of where the file's data lies,
//! not the data, and is asked without `FIEMAP_FLAG_SYNC`, so nothing is
//! written back first. The scan never calls it; `explain` does, for the
//! files under the path it is asked about, by the paths a saved scan
//! recorded. The tree may have changed since, and whoever owns a directory
//! in it may have put a symbolic link in its place: the path is therefore
//! resolved without following a link in any component, and what it names
//! is checked to be a regular file, on a filesystem whose map is read,
//! before anything is opened for reading. Opening a device, a FIFO or a
//! file on a network or FUSE filesystem can do something (arm a watchdog,
//! raise a serial line, block on a server); opening a regular file on a
//! local disk does nothing.

use std::ffi::CStr;
use std::fs::{File, Metadata, OpenOptions};
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Component, Path};
use std::sync::atomic::{AtomicBool, Ordering};

use super::magic;
use crate::Extents;
use crate::platform::posix;

/// `FS_IOC_FIEMAP`: `_IOWR('f', 11, struct fiemap)`, whose header is 32
/// bytes; the same number on every Linux architecture duscope builds for.
const FS_IOC_FIEMAP: u32 = 0xC020_660B;
/// The file's last extent.
const FIEMAP_EXTENT_LAST: u32 = 0x1;
/// The extent's blocks are shared with another file or a snapshot.
const FIEMAP_EXTENT_SHARED: u32 = 0x2000;
/// The extents asked for in one call.
const BATCH: u32 = 64;

/// `struct fiemap_extent`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct Extent {
    logical: u64,
    // The kernel's layout: written by the ioctl, not read here.
    #[allow(dead_code)]
    physical: u64,
    length: u64,
    #[allow(dead_code)]
    reserved64: [u64; 2],
    flags: u32,
    #[allow(dead_code)]
    reserved: [u32; 3],
}

const EMPTY: Extent = Extent {
    logical: 0,
    physical: 0,
    length: 0,
    reserved64: [0; 2],
    flags: 0,
    reserved: [0; 3],
};

/// `struct fiemap`, with room for [`BATCH`] extents.
#[repr(C)]
#[derive(Debug)]
struct Map {
    start: u64,
    length: u64,
    flags: u32,
    mapped_extents: u32,
    extent_count: u32,
    reserved: u32,
    extents: [Extent; BATCH as usize],
}

/// The filesystems whose files' extent maps are read, by their `statfs`
/// magic: btrfs, XFS and bcachefs, which share extents between files and
/// mark them shared; ext2, ext3 and ext4, F2FS and NTFS (ntfs3), which
/// share none, so that their files read as sharing nothing. Nothing is
/// opened elsewhere, and a file there is unchecked (an
/// [`io::ErrorKind::Unsupported`] error): on a network or FUSE filesystem,
/// where an open is a request to a server or a daemon; on an overlay,
/// whose files open in layers its own `statfs` does not name; on ZFS,
/// whose block clones FIEMAP does not show; on tmpfs, FAT and exFAT, which
/// answer it with no map.
const MAPPED: [u32; 6] = [
    magic::BTRFS,
    magic::XFS,
    magic::BCACHEFS,
    magic::EXT4,
    magic::F2FS,
    magic::NTFS3,
];

/// How the path, and each of its components on the walk, is opened: never
/// through a final symbolic link (`O_NOFOLLOW`), and as a path only
/// (`O_PATH`), which opens nothing (no driver's or filesystem's `open`
/// runs), whatever it names.
const LEAF: libc::c_int = libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC;

/// How a path is resolved: by `openat2` with `RESOLVE_NO_SYMLINKS`
/// (Linux 5.6), or one component at a time with `openat`, as on the kernels
/// before it (the floor is 5.4) and where a seccomp filter refuses it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Resolve {
    Openat2,
    Walk,
}

/// Set once `openat2` has failed with `ENOSYS` or `EPERM`: the process
/// walks from then on.
static NO_OPENAT2: AtomicBool = AtomicBool::new(false);

/// The extents of the regular file at `path`: their bytes, and of those the
/// bytes the filesystem shares with another file or a snapshot.
///
/// No component of `path` is followed if it is a symbolic link (`ELOOP`).
/// What it names is opened as a path only and checked through that
/// descriptor: a symbolic link is `ELOOP`, anything but a regular file an
/// [`io::ErrorKind::InvalidInput`] error, a filesystem whose map is not
/// read (see [`MAPPED`]) an [`io::ErrorKind::Unsupported`] one, as is a
/// filesystem that answers `EOPNOTSUPP`. Only then is the file opened for
/// reading, through `/proc/self/fd`, so that the open is of the file
/// checked, with no second lookup of its name; without `/proc` the file
/// is not opened, and the error says why. The open does not block
/// (`O_NONBLOCK`), so a lease another process holds on the file cannot
/// stall `explain`.
pub(in crate::platform) fn extents(path: &Path) -> io::Result<Extents> {
    let how = if NO_OPENAT2.load(Ordering::Relaxed) {
        Resolve::Walk
    } else {
        Resolve::Openat2
    };
    extents_with(path, how)
}

fn extents_with(path: &Path, how: Resolve) -> io::Result<Extents> {
    let leaf = File::from(resolve(path, how)?);
    let found = leaf.metadata()?;
    if found.file_type().is_symlink() {
        return Err(io::Error::from_raw_os_error(libc::ELOOP));
    }
    if !found.is_file() {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    if !MAPPED.contains(&super::fs_magic(leaf.as_fd())?) {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "the filesystem keeps no extent map that is read",
        ));
    }
    map(&reopen(&leaf, &found)?)
}

/// The descriptor of `path`, opened as a path only ([`LEAF`]), with no
/// symbolic link followed in any component.
fn resolve(path: &Path, how: Resolve) -> io::Result<OwnedFd> {
    if how == Resolve::Openat2 {
        match openat2(&posix::c_path(path)?) {
            Err(e) if matches!(e.raw_os_error(), Some(libc::ENOSYS | libc::EPERM)) => {
                NO_OPENAT2.store(true, Ordering::Relaxed);
            }
            other => return other,
        }
    }
    walk(path)
}

/// `struct open_how` (`linux/openat2.h`), which libc marks non-exhaustive.
#[repr(C)]
struct OpenHow {
    flags: u64,
    mode: u64,
    resolve: u64,
}

/// `openat2(2)` of `path`, relative to the working directory, refusing a
/// symbolic link in any component (`RESOLVE_NO_SYMLINKS`, which refuses
/// `/proc`'s magic links too) and opening a path only.
fn openat2(path: &CStr) -> io::Result<OwnedFd> {
    let how = OpenHow {
        // The flags are positive: the cast keeps their bits.
        #[allow(clippy::cast_sign_loss)]
        flags: LEAF as u64,
        mode: 0,
        resolve: libc::RESOLVE_NO_SYMLINKS,
    };
    // The system call itself: neither glibc before 2.42 nor musl wraps it.
    // SAFETY: `path` is NUL-terminated and outlives the call, `how` is a
    // `struct open_how` whose size is passed with it, read by the kernel
    // during the call only; the arguments are the system call's, in order.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            libc::AT_FDCWD,
            path.as_ptr(),
            &raw const how,
            std::mem::size_of::<OpenHow>(),
        )
    };
    owned(rc)
}

/// `path` resolved one component at a time, each opened as a path only
/// relative to the one before, so that no symbolic link is followed: a
/// link is `ELOOP` and anything else but a directory above the last
/// component `ENOTDIR`, as `openat2` answers.
fn walk(path: &Path) -> io::Result<OwnedFd> {
    let mut parts = path.components().peekable();
    let mut at: Option<OwnedFd> = None;
    while let Some(part) = parts.next() {
        let name: &[u8] = match part {
            Component::RootDir => b"/",
            Component::CurDir => b".",
            Component::ParentDir => b"..",
            Component::Normal(name) => name.as_bytes(),
            Component::Prefix(_) => return Err(io::ErrorKind::InvalidInput.into()),
        };
        let fd = posix::with_c_name(name, |name| openat(at.as_ref().map(AsFd::as_fd), name))?;
        if parts.peek().is_none() {
            return Ok(fd);
        }
        let dir = File::from(fd);
        let kind = dir.metadata()?.file_type();
        if kind.is_symlink() {
            return Err(io::Error::from_raw_os_error(libc::ELOOP));
        }
        if !kind.is_dir() {
            return Err(io::Error::from_raw_os_error(libc::ENOTDIR));
        }
        at = Some(dir.into());
    }
    Err(io::ErrorKind::NotFound.into())
}

/// `openat(2)` of `name` as a path only ([`LEAF`]), relative to the
/// directory open on `dir`, or to the working directory.
fn openat(dir: Option<BorrowedFd<'_>>, name: &CStr) -> io::Result<OwnedFd> {
    let dir = dir.map_or(libc::AT_FDCWD, |dir| dir.as_raw_fd());
    // SAFETY: `dir` is open for the borrow or is AT_FDCWD, and `name` is a
    // NUL-terminated C string that outlives the call.
    let rc = unsafe { libc::openat(dir, name.as_ptr(), LEAF) };
    owned(libc::c_long::from(rc))
}

/// The descriptor a system call returned, or the error it set.
fn owned(rc: libc::c_long) -> io::Result<OwnedFd> {
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    let fd = libc::c_int::try_from(rc).map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
    // SAFETY: `fd` was just returned by the kernel and is owned by nothing
    // else.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// The regular file open as a path on `leaf`, `found` by its `fstat`,
/// opened for reading through its `/proc/self/fd` link, which leads to that
/// very file, not to whatever its name now names; checked to be the same
/// file again.
fn reopen(leaf: &File, found: &Metadata) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY)
        .open(format!("/proc/self/fd/{}", leaf.as_raw_fd()))
        .map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => io::Error::new(
                io::ErrorKind::NotFound,
                "no /proc/self/fd to open the file through",
            ),
            _ => e,
        })?;
    let now = file.metadata()?;
    if (now.dev(), now.ino()) != (found.dev(), found.ino()) {
        return Err(io::Error::other("/proc/self/fd led to another file"));
    }
    Ok(file)
}

/// The extent map of the regular file open on `file`, 64 extents a call.
fn map(file: &File) -> io::Result<Extents> {
    let mut map = Map {
        start: 0,
        length: 0,
        flags: 0,
        mapped_extents: 0,
        extent_count: 0,
        reserved: 0,
        extents: [EMPTY; BATCH as usize],
    };
    let mut out = Extents::default();
    let mut start = 0u64;
    loop {
        map.start = start;
        map.length = u64::MAX - start;
        map.flags = 0;
        map.mapped_extents = 0;
        map.extent_count = BATCH;
        // The request is an `unsigned long` to glibc and an `int` to musl,
        // with the same bits.
        #[allow(clippy::cast_possible_wrap, clippy::cast_lossless)]
        let request = FS_IOC_FIEMAP as _;
        // SAFETY: the descriptor is open for the call, and `map` is a
        // `struct fiemap` whose `fm_extent_count` says how many extents its
        // array holds, which the kernel writes no further than; it lives on
        // this stack frame for the call.
        let rc = unsafe { libc::ioctl(file.as_raw_fd(), request, &raw mut map) };
        if rc < 0 {
            let error = io::Error::last_os_error();
            match error.raw_os_error() {
                Some(libc::EINTR) => continue,
                Some(libc::EOPNOTSUPP) => {
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        "the filesystem keeps no extent map",
                    ));
                }
                _ => return Err(error),
            }
        }
        let mapped = map.mapped_extents.min(BATCH);
        let mut last = false;
        let before = start;
        for extent in &map.extents[..mapped as usize] {
            out.mapped = out.mapped.saturating_add(extent.length);
            if extent.flags & FIEMAP_EXTENT_SHARED != 0 {
                out.shared = out.shared.saturating_add(extent.length);
            }
            last |= extent.flags & FIEMAP_EXTENT_LAST != 0;
            start = extent.logical.saturating_add(extent.length);
        }
        // Fewer extents than asked for: the range holds no more. A map
        // that does not move on would never end.
        if last || mapped < BATCH || start <= before {
            return Ok(out);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    use std::path::PathBuf;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    use duscope_testkit::TestEnv;

    use super::{Resolve, extents, extents_with};

    #[test]
    fn p5_walk_02_a_plain_file_shares_nothing_and_a_link_is_not_followed() {
        let env = TestEnv::new();
        // Resolved: no component of the path may be a link.
        let dir = fs::canonicalize(env.tmp_dir()).unwrap().join("fiemap");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("plain");
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(&vec![7u8; 64 * 1024]).unwrap();
        file.sync_all().unwrap();
        match extents(&path) {
            Ok(found) => {
                assert!(found.mapped >= 64 * 1024, "{found:?}");
                assert_eq!(found.shared, 0, "{found:?}");
            }
            // tmpfs keeps no extent map.
            Err(e) if e.kind() == std::io::ErrorKind::Unsupported => {
                eprintln!("skipped: no extent map on the temporary directory's filesystem");
            }
            Err(e) => panic!("{e}"),
        }
        std::os::unix::fs::symlink(&path, dir.join("link")).unwrap();
        assert_eq!(
            extents(&dir.join("link")).unwrap_err().raw_os_error(),
            Some(libc::ELOOP)
        );
        assert_eq!(
            extents(&dir).unwrap_err().kind(),
            std::io::ErrorKind::InvalidInput
        );
    }

    /// A FIFO and a writer blocked opening it, which wakes only once the
    /// FIFO is opened for reading: the witness of an open.
    struct Witness {
        fifo: PathBuf,
        woke: mpsc::Receiver<()>,
        writer: Option<thread::JoinHandle<()>>,
    }

    impl Witness {
        fn new(env: &TestEnv, fifo: PathBuf) -> Witness {
            let made = env.command("mkfifo").arg(&fifo).status().unwrap();
            assert!(made.success(), "mkfifo {}", fifo.display());
            let (tx, woke) = mpsc::channel();
            let path = fifo.clone();
            let writer = thread::spawn(move || {
                let _open = OpenOptions::new().write(true).open(&path);
                let _ = tx.send(());
            });
            // Time for the writer to block in open(2).
            thread::sleep(Duration::from_millis(100));
            Witness {
                fifo,
                woke,
                writer: Some(writer),
            }
        }

        /// Whether the FIFO was opened for reading since the writer
        /// blocked.
        fn opened(&self) -> bool {
            self.woke.recv_timeout(Duration::from_millis(200)).is_ok()
        }

        fn name(&self) -> &std::ffi::OsStr {
            self.fifo.file_name().unwrap()
        }
    }

    impl Drop for Witness {
        /// Opens the FIFO for reading, which wakes the writer, and waits
        /// for it.
        fn drop(&mut self) {
            let _read = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&self.fifo);
            if let Some(writer) = self.writer.take() {
                let _ = writer.join();
            }
        }
    }

    #[test]
    fn p5_walk_02_extents_open_nothing_but_a_regular_file_reached_without_links() {
        let env = TestEnv::new();
        let dir = fs::canonicalize(env.tmp_dir()).unwrap().join("links");
        let real = dir.join("real");
        fs::create_dir_all(&real).unwrap();
        fs::write(real.join("file"), vec![1u8; 16 * 1024]).unwrap();
        // A directory a saved scan listed, replaced by a link since.
        std::os::unix::fs::symlink("real", dir.join("link")).unwrap();
        let link = |name: &std::ffi::OsStr| -> PathBuf { dir.join("link").join(name) };

        // The witness sees an open for reading, without blocking.
        let check = Witness::new(&env, real.join("check"));
        drop(
            OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&check.fifo)
                .unwrap(),
        );
        assert!(check.opened(), "the witness sees an open");
        drop(check);

        for how in [Resolve::Openat2, Resolve::Walk] {
            let fifo = Witness::new(&env, real.join(format!("fifo-{how:?}")));
            // Anything but a regular file is refused, and never opened.
            assert_eq!(
                extents_with(&fifo.fifo, how).unwrap_err().kind(),
                std::io::ErrorKind::InvalidInput,
                "{how:?}"
            );
            assert!(!fifo.opened(), "{how:?}: the FIFO was opened");
            // A link in a parent component is not followed: neither to the
            // FIFO nor to the regular file beside it.
            assert_eq!(
                extents_with(&link(fifo.name()), how)
                    .unwrap_err()
                    .raw_os_error(),
                Some(libc::ELOOP),
                "{how:?}"
            );
            assert!(
                !fifo.opened(),
                "{how:?}: the FIFO was opened through a link"
            );
            assert_eq!(
                extents_with(&link("file".as_ref()), how)
                    .unwrap_err()
                    .raw_os_error(),
                Some(libc::ELOOP),
                "{how:?}"
            );
            // The last component a link, and a file above the last: refused.
            assert_eq!(
                extents_with(&dir.join("link"), how)
                    .unwrap_err()
                    .raw_os_error(),
                Some(libc::ELOOP),
                "{how:?}"
            );
            assert_eq!(
                extents_with(&real.join("file/below"), how)
                    .unwrap_err()
                    .raw_os_error(),
                Some(libc::ENOTDIR),
                "{how:?}"
            );
            // The file itself, by its own path: read, or a filesystem
            // whose map is not read (tmpfs).
            match extents_with(&real.join("file"), how) {
                Ok(found) => assert_eq!(found.shared, 0, "{how:?}: {found:?}"),
                Err(e) => assert_eq!(e.kind(), std::io::ErrorKind::Unsupported, "{how:?}: {e}"),
            }
        }
    }
}
