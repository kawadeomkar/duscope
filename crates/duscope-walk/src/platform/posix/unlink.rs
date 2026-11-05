//! `clean --permanent`: removing an entry, or a directory and everything
//! under it, through descriptors, so that nothing swapped in after the plan
//! leads the removal outside what it was asked to remove.
//!
//! - **The way there.** [`CheckedDir::open`] opens `/`, then each component
//!   of the path relative to the one above with `O_NOFOLLOW | O_DIRECTORY`:
//!   a component replaced by a symbolic link fails the open rather than
//!   leading elsewhere.
//! - **The entry.** [`unlink_checked`] and [`remove_dir_checked`] read the
//!   entry's device and inode through the parent's descriptor, without
//!   following a link, and remove nothing unless they are the expected ones.
//! - **Under a directory.** Each subdirectory is opened relative to its
//!   parent with `O_NOFOLLOW | O_DIRECTORY` and checked to be the directory
//!   just read; a symbolic link is removed as an entry, never followed; a
//!   directory on another device, or a mount point (Linux reports a bind
//!   mount of the same device as one), is never entered. Directories are
//!   emptied depth first and removed bottom up with `AT_REMOVEDIR`.
//! - **What it frees.** Each removal adds the bytes on disk of each inode
//!   whose last link it removed, and of each directory, as `du` counts them.
//!
//! The first failure stops a removal; what was removed before it stays
//! removed.

use std::ffi::{CStr, OsStr};
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::os::raw::c_int;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

use crate::{NameRef, RemoveError};

/// A directory opened to remove entries from: reached from `/` one
/// component at a time, each opened relative to the one above it without
/// following a symbolic link.
#[derive(Debug)]
pub struct CheckedDir {
    fd: OwnedFd,
    path: PathBuf,
    dev: u64,
}

impl CheckedDir {
    /// Opens the directory `path`, absolute and without `..`: `/`, then
    /// each component relative to the one above with `O_NOFOLLOW |
    /// O_DIRECTORY`. Mounts on the way are crossed: it is the removal below
    /// that never crosses one.
    ///
    /// # Errors
    ///
    /// [`RemoveError::Changed`] when a component is gone or is no longer a
    /// directory (a symbolic link or a file in its place);
    /// [`RemoveError::Io`] when one cannot be opened, or the path is not
    /// absolute or holds `..`.
    pub fn open(path: &Path) -> Result<CheckedDir, RemoveError> {
        let invalid = || RemoveError::Io {
            path: path.to_owned(),
            source: io::Error::new(
                io::ErrorKind::InvalidInput,
                "not an absolute path without ..",
            ),
        };
        let mut components = path.components();
        if components.next() != Some(Component::RootDir) {
            return Err(invalid());
        }
        // A removal holds a descriptor for each level it is down: the limit
        // a walk raises, raised.
        super::raise_nofile_limit();
        let mut reached = PathBuf::from("/");
        let mut fd = super::open_dir(&reached).map_err(|source| RemoveError::Io {
            path: reached.clone(),
            source,
        })?;
        for component in components {
            let Component::Normal(name) = component else {
                return Err(invalid());
            };
            reached.push(name);
            fd = open_child(fd.as_fd(), name.as_bytes(), &reached)?;
        }
        let dev = stat_fd(fd.as_fd())
            .map_err(|source| RemoveError::Io {
                path: reached.clone(),
                source,
            })?
            .dev;
        Ok(CheckedDir {
            fd,
            path: reached,
            dev,
        })
    }

    /// The directory's path, as opened.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Removes `name`, any entry but a directory (a symbolic link itself, never
/// what it leads to), from `parent`, when its device and inode are
/// `expect`; adds to `freed` its bytes on disk when this was its last link.
///
/// # Errors
///
/// [`RemoveError::Changed`] when the entry is gone, is another device and
/// inode, or is a directory; [`RemoveError::Io`] when it cannot be removed.
pub fn unlink_checked(
    parent: &CheckedDir,
    name: &OsStr,
    expect: (u64, u64),
    freed: &mut u64,
) -> Result<(), RemoveError> {
    let path = child_path(parent, name)?;
    let stat = checked(parent.fd.as_fd(), name.as_bytes(), &path, expect)?;
    if stat.dir {
        return Err(RemoveError::Changed { path });
    }
    #[cfg(test)]
    hook::step(&path);
    super::with_c_name(name.as_bytes(), |c_name| {
        remove_at(parent.fd.as_fd(), c_name, 0)
    })
    .map_err(|e| classify(e, &path))?;
    if stat.nlink <= 1 {
        *freed = freed.saturating_add(stat.bytes);
    }
    Ok(())
}

/// Removes the directory `name` and everything under it from `parent`,
/// when its device and inode are `expect`: depth first, through each
/// directory's descriptor, never through a symbolic link or into another
/// device or a mount point; adds to `freed` the bytes on disk of each inode
/// whose last link it removed and of each directory it removed, as it goes.
///
/// # Errors
///
/// [`RemoveError::Changed`] when the directory, or one under it, is gone or
/// is not what was just read of it; [`RemoveError::Mount`] for a mount
/// point or a directory of another device under it, which stays with every
/// directory above it; [`RemoveError::Io`] when an entry cannot be read or
/// removed. What was removed before the failure stays removed.
pub fn remove_dir_checked(
    parent: &CheckedDir,
    name: &OsStr,
    expect: (u64, u64),
    freed: &mut u64,
) -> Result<(), RemoveError> {
    let path = child_path(parent, name)?;
    let stat = checked(parent.fd.as_fd(), name.as_bytes(), &path, expect)?;
    if !stat.dir {
        return Err(RemoveError::Changed { path });
    }
    if stat.mount || stat.dev != parent.dev {
        return Err(RemoveError::Mount { path });
    }
    let fd = open_child(parent.fd.as_fd(), name.as_bytes(), &path)?;
    let opened = stat_fd(fd.as_fd()).map_err(|source| RemoveError::Io {
        path: path.clone(),
        source,
    })?;
    if opened.ident() != expect {
        return Err(RemoveError::Changed { path });
    }
    empty(fd, opened.dev, &path, freed)?;
    super::with_c_name(name.as_bytes(), |c_name| {
        remove_at(parent.fd.as_fd(), c_name, libc::AT_REMOVEDIR)
    })
    .map_err(|e| classify(e, &path))?;
    *freed = freed.saturating_add(stat.bytes);
    Ok(())
}

/// A directory being emptied: its descriptor, its device, the names left
/// to remove, and its own name in the directory above (none for the
/// directory [`remove_dir_checked`] removes itself).
struct Level {
    fd: OwnedFd,
    dev: u64,
    bytes: u64,
    names: std::vec::IntoIter<NameRef>,
    path: PathBuf,
    name: Option<NameRef>,
}

/// Removes everything in the directory open on `fd`, at `path` on device
/// `dev`, depth first with a stack rather than recursion, so a deep tree
/// costs a descriptor a level and no stack frames. Each directory's names
/// are read whole before any is removed: a listing read while entries go
/// may skip some.
fn empty(fd: OwnedFd, dev: u64, path: &Path, freed: &mut u64) -> Result<(), RemoveError> {
    let names = list(fd.as_fd(), path)?;
    let mut stack = vec![Level {
        fd,
        dev,
        bytes: 0,
        names: names.into_iter(),
        path: path.to_owned(),
        name: None,
    }];
    loop {
        let Some(level) = stack.last_mut() else {
            return Ok(());
        };
        let Some(name) = level.names.next() else {
            // Emptied: remove it from the directory above, after closing it.
            let Some(done) = stack.pop() else {
                return Ok(());
            };
            let (Some(up), Some(name)) = (stack.last(), done.name) else {
                return Ok(());
            };
            drop(done.fd);
            match super::with_c_name(name.as_bytes(), |c_name| {
                remove_at(up.fd.as_fd(), c_name, libc::AT_REMOVEDIR)
            }) {
                Ok(()) => *freed = freed.saturating_add(done.bytes),
                // Removed meanwhile by someone else: gone either way.
                Err(e) if e.raw_os_error() == Some(libc::ENOENT) => {}
                Err(e) => return Err(classify(e, &done.path)),
            }
            continue;
        };
        let child = level.path.join(name.as_os_str());
        let stat =
            match super::with_c_name(name.as_bytes(), |c_name| stat_at(level.fd.as_fd(), c_name)) {
                Ok(stat) => stat,
                Err(e) if e.raw_os_error() == Some(libc::ENOENT) => continue,
                Err(source) => {
                    return Err(RemoveError::Io {
                        path: child,
                        source,
                    });
                }
            };
        if stat.dir {
            if stat.mount || stat.dev != level.dev {
                return Err(RemoveError::Mount { path: child });
            }
            let fd = match open_at(level.fd.as_fd(), name.as_bytes(), &child) {
                Ok(fd) => fd,
                // Removed meanwhile by someone else: gone either way.
                Err(e) if e.raw_os_error() == Some(libc::ENOENT) => continue,
                Err(e) => return Err(classify(e, &child)),
            };
            let opened = stat_fd(fd.as_fd()).map_err(|source| RemoveError::Io {
                path: child.clone(),
                source,
            })?;
            if opened.ident() != stat.ident() {
                return Err(RemoveError::Changed { path: child });
            }
            let names = list(fd.as_fd(), &child)?;
            stack.push(Level {
                fd,
                dev: opened.dev,
                bytes: opened.bytes,
                names: names.into_iter(),
                path: child,
                name: Some(name),
            });
        } else {
            #[cfg(test)]
            hook::step(&child);
            match super::with_c_name(name.as_bytes(), |c_name| {
                remove_at(level.fd.as_fd(), c_name, 0)
            }) {
                Ok(()) if stat.nlink <= 1 => *freed = freed.saturating_add(stat.bytes),
                Ok(()) => {}
                Err(e) if e.raw_os_error() == Some(libc::ENOENT) => {}
                Err(e) => return Err(classify(e, &child)),
            }
        }
    }
}

/// The names in the directory open on `fd`.
fn list(fd: BorrowedFd<'_>, path: &Path) -> Result<Vec<NameRef>, RemoveError> {
    super::list_names(fd).map_err(|source| RemoveError::Io {
        path: path.to_owned(),
        source,
    })
}

/// `parent`'s path joined with `name`, which must be one component: a name
/// with a `/`, or `.` or `..`, would reach past `parent`.
fn child_path(parent: &CheckedDir, name: &OsStr) -> Result<PathBuf, RemoveError> {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes == b"." || bytes == b".." || bytes.contains(&b'/') {
        return Err(RemoveError::Io {
            path: parent.path.join(name),
            source: io::Error::new(io::ErrorKind::InvalidInput, "not one name in the directory"),
        });
    }
    Ok(parent.path.join(name))
}

/// What is at `name` in the directory open on `dir`, read without
/// following a link, when its device and inode are `expect`.
fn checked(
    dir: BorrowedFd<'_>,
    name: &[u8],
    path: &Path,
    expect: (u64, u64),
) -> Result<Stat, RemoveError> {
    let stat =
        super::with_c_name(name, |c_name| stat_at(dir, c_name)).map_err(|e| classify(e, path))?;
    if stat.ident() != expect {
        return Err(RemoveError::Changed {
            path: path.to_owned(),
        });
    }
    Ok(stat)
}

/// Opens the directory `name`, at `path`, in the directory open on `dir`,
/// never through a symbolic link.
fn open_child(dir: BorrowedFd<'_>, name: &[u8], path: &Path) -> Result<OwnedFd, RemoveError> {
    open_at(dir, name, path).map_err(|e| classify(e, path))
}

/// Opens the directory `name`, at `path`, in the directory open on `dir`
/// with `O_NOFOLLOW | O_DIRECTORY`.
fn open_at(dir: BorrowedFd<'_>, name: &[u8], path: &Path) -> io::Result<OwnedFd> {
    #[cfg(test)]
    hook::step(path);
    #[cfg(not(test))]
    let _ = path;
    super::with_c_name(name, |c_name| super::open_at(Some(dir), c_name))
}

/// An error at `path`: [`RemoveError::Changed`] when what is there is gone
/// or of another kind than it was (`ENOENT`; `ELOOP` or `ENOTDIR` where a
/// directory was expected; `EISDIR` where it was not), else
/// [`RemoveError::Io`].
fn classify(error: io::Error, path: &Path) -> RemoveError {
    match error.raw_os_error() {
        Some(libc::ENOENT | libc::ELOOP | libc::ENOTDIR | libc::EISDIR) => RemoveError::Changed {
            path: path.to_owned(),
        },
        _ => RemoveError::Io {
            path: path.to_owned(),
            source: error,
        },
    }
}

/// `unlinkat(2)` of `name` in the directory open on `dir`.
fn remove_at(dir: BorrowedFd<'_>, name: &CStr, flags: c_int) -> io::Result<()> {
    // SAFETY: `dir` is open for the borrow and `name` is NUL-terminated;
    // unlinkat never follows a final symbolic link.
    super::retry(|| unsafe { libc::unlinkat(dir.as_raw_fd(), name.as_ptr(), flags) })
}

/// What a removal reads of an entry.
#[derive(Debug, Clone, Copy)]
struct Stat {
    dev: u64,
    ino: u64,
    dir: bool,
    nlink: u64,
    /// Bytes on disk, `st_blocks` × 512, as `du` counts them.
    bytes: u64,
    /// A mount point or an automount point (Linux, from `statx`): never
    /// entered, whatever its device.
    mount: bool,
}

impl Stat {
    fn ident(&self) -> (u64, u64) {
        (self.dev, self.ino)
    }

    fn from_stat(stat: &libc::stat) -> Stat {
        // `st_nlink` is 16-bit on macOS, 64-bit on x86_64 Linux and 32-bit
        // on aarch64 Linux.
        #[allow(clippy::useless_conversion)]
        let nlink = u64::from(stat.st_nlink);
        Stat {
            dev: dev_of(stat.st_dev),
            ino: stat.st_ino,
            dir: stat.st_mode & libc::S_IFMT == libc::S_IFDIR,
            nlink,
            bytes: u64::try_from(stat.st_blocks)
                .unwrap_or(0)
                .saturating_mul(512),
            mount: false,
        }
    }
}

/// A `dev_t` as `std::os::unix::fs::MetadataExt::dev` reports it, so it
/// compares with a plan's: macOS's signed 32-bit value, sign-extended.
#[cfg(target_os = "macos")]
fn dev_of(dev: libc::dev_t) -> u64 {
    i64::from(dev).cast_unsigned()
}

/// A `dev_t` as `std::os::unix::fs::MetadataExt::dev` reports it.
#[cfg(not(target_os = "macos"))]
fn dev_of(dev: libc::dev_t) -> u64 {
    dev
}

/// `fstat(2)` of the descriptor `fd`.
fn fstat(fd: BorrowedFd<'_>) -> io::Result<Stat> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `fd` is open for the borrow and `stat` is writable for one
    // `struct stat`.
    super::retry(|| unsafe { libc::fstat(fd.as_raw_fd(), stat.as_mut_ptr()) })?;
    // SAFETY: fstat succeeded, so it filled the struct.
    Ok(Stat::from_stat(&unsafe { stat.assume_init() }))
}

/// `name` in the directory open on `dir`, not following a final link.
#[cfg(not(target_os = "linux"))]
fn stat_at(dir: BorrowedFd<'_>, name: &CStr) -> io::Result<Stat> {
    super::lstat_at(dir, name).map(|stat| Stat::from_stat(&stat))
}

/// The directory open on `fd`.
#[cfg(not(target_os = "linux"))]
fn stat_fd(fd: BorrowedFd<'_>) -> io::Result<Stat> {
    fstat(fd)
}

/// `name` in the directory open on `dir`, not following a final link nor
/// triggering an automount: `statx`, for the mount-root attribute (5.8) that
/// tells a bind mount of the same device, which `st_dev` cannot; `fstatat`
/// on a kernel without `statx`.
#[cfg(target_os = "linux")]
fn stat_at(dir: BorrowedFd<'_>, name: &CStr) -> io::Result<Stat> {
    match statx(dir, name, libc::AT_SYMLINK_NOFOLLOW | libc::AT_NO_AUTOMOUNT) {
        Err(e) if e.raw_os_error() == Some(libc::ENOSYS) => {
            super::lstat_at(dir, name).map(|stat| Stat::from_stat(&stat))
        }
        read => read,
    }
}

/// The directory open on `fd`.
#[cfg(target_os = "linux")]
fn stat_fd(fd: BorrowedFd<'_>) -> io::Result<Stat> {
    match statx(fd, c"", libc::AT_EMPTY_PATH) {
        Err(e) if e.raw_os_error() == Some(libc::ENOSYS) => fstat(fd),
        read => read,
    }
}

/// `statx(2)` of `name` relative to `dir` with `flags`.
#[cfg(target_os = "linux")]
fn statx(dir: BorrowedFd<'_>, name: &CStr, flags: c_int) -> io::Result<Stat> {
    const MASK: u32 = libc::STATX_TYPE | libc::STATX_INO | libc::STATX_NLINK | libc::STATX_BLOCKS;
    let mut buf = std::mem::MaybeUninit::<libc::statx>::uninit();
    loop {
        // The system call itself, as the provider makes it: the musl that
        // release builds link statically has no wrapper.
        // SAFETY: `dir` is open for the borrow, `name` is NUL-terminated,
        // and `buf` is writable for one statx; the arguments are the system
        // call's, in its order.
        let rc = unsafe {
            libc::syscall(
                libc::SYS_statx,
                dir.as_raw_fd(),
                name.as_ptr(),
                flags,
                MASK,
                buf.as_mut_ptr(),
            )
        };
        if rc == 0 {
            // SAFETY: statx succeeded, so it filled the struct.
            let stx = unsafe { buf.assume_init() };
            let attribute = |bit: c_int| {
                let bit = u64::from(bit.cast_unsigned());
                stx.stx_attributes_mask & bit != 0 && stx.stx_attributes & bit != 0
            };
            return Ok(Stat {
                dev: libc::makedev(stx.stx_dev_major, stx.stx_dev_minor),
                ino: stx.stx_ino,
                dir: u32::from(stx.stx_mode) & libc::S_IFMT == libc::S_IFDIR,
                nlink: u64::from(stx.stx_nlink),
                bytes: stx.stx_blocks.saturating_mul(512),
                mount: attribute(libc::STATX_ATTR_MOUNT_ROOT)
                    || attribute(libc::STATX_ATTR_AUTOMOUNT),
            });
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

/// A test's hook into a removal, called with each path just before it is
/// opened or unlinked, so a test can change the tree at that moment: the
/// race a removal guards against, made to happen every time.
#[cfg(test)]
pub(crate) mod hook {
    use std::cell::RefCell;
    use std::path::Path;

    type Step = Box<dyn FnMut(&Path)>;

    thread_local! {
        static STEP: RefCell<Option<Step>> = const { RefCell::new(None) };
    }

    /// Calls `step` on this thread at each step of a removal until the
    /// returned guard drops.
    pub(crate) fn on_step(step: impl FnMut(&Path) + 'static) -> Guard {
        STEP.with(|s| *s.borrow_mut() = Some(Box::new(step)));
        Guard
    }

    /// Takes the hook away when dropped.
    pub(crate) struct Guard;

    impl Drop for Guard {
        fn drop(&mut self) {
            STEP.with(|s| *s.borrow_mut() = None);
        }
    }

    pub(super) fn step(path: &Path) {
        STEP.with(|s| {
            if let Some(step) = s.borrow_mut().as_mut() {
                step(path);
            }
        });
    }
}

#[cfg(test)]
mod tests;
