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
//!   just read; a symbolic link is removed as an entry, never followed.
//!   Directories are emptied depth first and removed bottom up with
//!   `AT_REMOVEDIR`.
//! - **Never across a mount.** A directory on another device is never
//!   entered, nor a mount point of the same device (a bind mount), which
//!   only Linux has: its `statx` flags one from 5.8. Where it does not
//!   (an older kernel, or no `statx`), each directory's mount is compared
//!   with its parent's through their descriptors once it is opened, and
//!   before anything in it is removed ([`MountCheck`]); a directory whose
//!   mount nothing can tell is never entered either.
//! - **What it frees.** Each removal adds the bytes on disk of each inode
//!   whose last link it removed, and of each directory, as `du` counts them.
//!
//! The first failure stops a removal; what was removed before it stays
//! removed.
//!
//! - **The Trash, on Linux.** [`rename_checked`] moves an entry the same
//!   way: through the descriptor of a directory [`CheckedDir::open`]
//!   reached, only when the entry is the expected one, into a directory
//!   open on a descriptor of its own, never replacing what is there.
//!   Linux renames by name only, so what was moved is read again at its
//!   new name and moved back when it is not the entry expected.

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

    /// The directory's device, read through its descriptor when it was
    /// opened.
    #[cfg(target_os = "linux")]
    pub(crate) fn dev(&self) -> u64 {
        self.dev
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
/// directory above it; [`RemoveError::MountUnknown`] for a directory whose
/// mount nothing can tell, which is never entered either;
/// [`RemoveError::Io`] when an entry cannot be read or removed. What was
/// removed before the failure stays removed.
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
    if stat.mount == Some(true) || stat.dev != parent.dev {
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
    // Before anything in it is removed: where nothing can tell whether it
    // is a mount point, nothing is.
    let mut mounts = MountCheck::default();
    let mnt = match stat.mount {
        Some(_) => None,
        None => mounts.check(parent.fd.as_fd(), &mut None, fd.as_fd(), &path)?,
    };
    empty(fd, opened.dev, mnt, &path, &mut mounts, freed)?;
    super::with_c_name(name.as_bytes(), |c_name| {
        remove_at(parent.fd.as_fd(), c_name, libc::AT_REMOVEDIR)
    })
    .map_err(|e| classify(e, &path))?;
    *freed = freed.saturating_add(stat.bytes);
    Ok(())
}

/// Whether `name` in `parent` is the entry `expect`, read through
/// `parent`'s descriptor without following a link.
///
/// # Errors
///
/// [`RemoveError::Changed`] when the entry is gone or is another device and
/// inode; [`RemoveError::Io`] when it cannot be read, or `name` is not one
/// name in the directory.
#[cfg(target_os = "linux")]
pub(crate) fn check_entry(
    parent: &CheckedDir,
    name: &OsStr,
    expect: (u64, u64),
) -> Result<(), RemoveError> {
    let path = child_path(parent, name)?;
    checked(parent.fd.as_fd(), name.as_bytes(), &path, expect).map(|_| ())
}

/// Why [`rename_checked`] did not move an entry.
#[cfg(target_os = "linux")]
#[derive(Debug)]
pub(crate) enum RenameError {
    /// The entry is gone or is not the one expected
    /// ([`RemoveError::Changed`]), or cannot be read, or its name is not
    /// one name in its directory ([`RemoveError::Io`]): nothing was moved,
    /// or what was moved in its place has been moved back.
    Entry(RemoveError),
    /// Something is at the target name: nothing was moved.
    Taken,
    /// What was moved was not the entry expected, and moving it back
    /// failed (its name taken again, most likely): it is at the target
    /// name.
    Stranded(io::Error),
    /// The system refused the rename, or the target name is not one name
    /// in its directory: nothing was moved.
    Io(io::Error),
}

/// Moves `name`, any entry (a symbolic link itself, never what it leads
/// to), from `parent` to `target_name` in the directory open on `target`,
/// when its device and inode are `expect`, never replacing what is at
/// `target_name`. Linux renames by name only, so another entry may take the
/// name between the check and the rename: what was moved is read again at
/// its new name through `target` and, when it is not the entry expected,
/// moved back, again replacing nothing.
///
/// The rename is `renameat2(2)` with `RENAME_NOREPLACE`. Where the
/// filesystem refuses the flag (`EINVAL`: NFS, 9p and Ceph always, FUSE
/// where the server has no `RENAME2`) or the kernel has no `renameat2`
/// (`ENOSYS`, before 3.15), the name is first read through `target` and,
/// when nothing is there, `renameat(2)` follows, as gnulib's `renameatu`
/// (coreutils' `mv -n`) and systemd's `rename_noreplace` do: a name taken
/// between the two is replaced, which only a writer of that directory can
/// bring about.
///
/// # Errors
///
/// [`RenameError::Entry`] when the entry is gone or is not `expect`:
/// nothing moved, or what moved was moved back; [`RenameError::Taken`]
/// when something is at `target_name`; [`RenameError::Stranded`] when what
/// moved was not `expect` and could not be moved back; [`RenameError::Io`]
/// when the system refuses the rename.
#[cfg(target_os = "linux")]
pub(crate) fn rename_checked(
    parent: &CheckedDir,
    name: &OsStr,
    expect: (u64, u64),
    target: BorrowedFd<'_>,
    target_name: &OsStr,
) -> Result<(), RenameError> {
    let path = child_path(parent, name).map_err(RenameError::Entry)?;
    if !one_name(target_name.as_bytes()) {
        return Err(RenameError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not one name in the directory",
        )));
    }
    let from = (parent.fd.as_fd(), name.as_bytes());
    let to = (target, target_name.as_bytes());
    checked(from.0, from.1, &path, expect).map_err(RenameError::Entry)?;
    #[cfg(test)]
    hook::step(&path);
    match rename_noreplace(from, to) {
        Ok(()) => {}
        Err(e) if e.raw_os_error() == Some(libc::EEXIST) => return Err(RenameError::Taken),
        // The entry gone, or the target's directory: the entry says which.
        Err(e) if e.raw_os_error() == Some(libc::ENOENT) => {
            return Err(match checked(from.0, from.1, &path, expect) {
                Ok(_) => RenameError::Io(e),
                Err(changed) => RenameError::Entry(changed),
            });
        }
        Err(e) => return Err(RenameError::Io(e)),
    }
    #[cfg(test)]
    hook::moved();
    let moved = super::with_c_name(to.1, |c_name| stat_at(to.0, c_name));
    if moved.is_ok_and(|stat| stat.ident() == expect) {
        return Ok(());
    }
    match rename_noreplace(to, from) {
        Ok(()) => Err(RenameError::Entry(RemoveError::Changed { path })),
        Err(e) => Err(RenameError::Stranded(e)),
    }
}

/// Renames `from` to `to`, each the descriptor of a directory and a name in
/// it, never replacing what is at `to`; see [`rename_checked`].
#[cfg(target_os = "linux")]
fn rename_noreplace(from: (BorrowedFd<'_>, &[u8]), to: (BorrowedFd<'_>, &[u8])) -> io::Result<()> {
    super::with_c_name(from.1, |old| {
        super::with_c_name(to.1, |new| {
            match renameat2_noreplace(from.0, old, to.0, new) {
                Err(e)
                    if matches!(
                        e.raw_os_error(),
                        Some(libc::EINVAL | libc::ENOSYS | libc::EOPNOTSUPP)
                    ) =>
                {
                    match super::lstat_at(to.0, new) {
                        Ok(_) => Err(io::Error::from_raw_os_error(libc::EEXIST)),
                        Err(e) if e.raw_os_error() == Some(libc::ENOENT) => {
                            renameat(from.0, old, to.0, new)
                        }
                        Err(e) => Err(e),
                    }
                }
                renamed => renamed,
            }
        })
    })
}

/// `renameat2(2)` of `old` in the directory open on `from` to `new` in the
/// one open on `to`, with `RENAME_NOREPLACE`.
#[cfg(target_os = "linux")]
fn renameat2_noreplace(
    from: BorrowedFd<'_>,
    old: &CStr,
    to: BorrowedFd<'_>,
    new: &CStr,
) -> io::Result<()> {
    #[cfg(test)]
    if hook::no_noreplace() {
        return Err(io::Error::from_raw_os_error(libc::EINVAL));
    }
    super::retry(|| {
        // The system call itself, as for statx: the musl that release
        // builds link statically may have no wrapper.
        // SAFETY: both descriptors are open for their borrows and both
        // names are NUL-terminated; the arguments are the system call's,
        // in its order.
        let rc = unsafe {
            libc::syscall(
                libc::SYS_renameat2,
                from.as_raw_fd(),
                old.as_ptr(),
                to.as_raw_fd(),
                new.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if rc == 0 { 0 } else { -1 }
    })
}

/// `renameat(2)` of `old` in the directory open on `from` to `new` in the
/// one open on `to`, replacing what is at `new`.
#[cfg(target_os = "linux")]
fn renameat(from: BorrowedFd<'_>, old: &CStr, to: BorrowedFd<'_>, new: &CStr) -> io::Result<()> {
    // SAFETY: both descriptors are open for their borrows and both names
    // are NUL-terminated.
    super::retry(|| unsafe {
        libc::renameat(from.as_raw_fd(), old.as_ptr(), to.as_raw_fd(), new.as_ptr())
    })
}

/// The mode a file [`create_at`] makes is given before the umask: read and
/// write for all, as `std::fs::File::create` asks.
#[cfg(target_os = "linux")]
const CREATE_MODE: libc::c_uint = 0o666;

/// The directory `name` in the directory open on `dir`, made with mode 0700
/// when missing, opened never through a symbolic link.
#[cfg(target_os = "linux")]
pub(crate) fn make_dir_at(dir: BorrowedFd<'_>, name: &CStr) -> io::Result<OwnedFd> {
    // SAFETY: `dir` is open for the borrow and `name` is NUL-terminated.
    match super::retry(|| unsafe { libc::mkdirat(dir.as_raw_fd(), name.as_ptr(), 0o700) }) {
        Err(e) if e.kind() != io::ErrorKind::AlreadyExists => return Err(e),
        _ => {}
    }
    super::open_at(Some(dir), name)
}

/// Creates the file `name` in the directory open on `dir`, to write it:
/// only where nothing is at the name, not even a symbolic link
/// (`O_CREAT | O_EXCL`), so the name is this caller's alone.
#[cfg(target_os = "linux")]
pub(crate) fn create_at(dir: BorrowedFd<'_>, name: &OsStr) -> io::Result<std::fs::File> {
    super::with_c_name(name.as_bytes(), |c_name| {
        // SAFETY: `dir` is open for the borrow and `c_name` is
        // NUL-terminated; with O_CREAT the mode is the third argument.
        super::parked(|| unsafe {
            libc::openat(
                dir.as_raw_fd(),
                c_name.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC,
                CREATE_MODE,
            )
        })
    })
    .map(std::fs::File::from)
}

/// Removes `name`, not a directory, from the directory open on `dir`.
#[cfg(target_os = "linux")]
pub(crate) fn unlink_at(dir: BorrowedFd<'_>, name: &OsStr) -> io::Result<()> {
    super::with_c_name(name.as_bytes(), |c_name| remove_at(dir, c_name, 0))
}

/// A directory being emptied: its descriptor, its device, its mount id
/// once read ([`MountCheck`]), the names left to remove, and its own name
/// in the directory above (none for the directory [`remove_dir_checked`]
/// removes itself).
struct Level {
    fd: OwnedFd,
    dev: u64,
    mnt: Option<u64>,
    bytes: u64,
    names: std::vec::IntoIter<NameRef>,
    path: PathBuf,
    name: Option<NameRef>,
}

/// Removes everything in the directory open on `fd`, at `path` on device
/// `dev` (on the mount `mnt`, when its id was read), depth first with a
/// stack rather than recursion, so a deep tree costs a descriptor a level
/// and no stack frames. Each directory's names are read whole before any
/// is removed: a listing read while entries go may skip some.
fn empty(
    fd: OwnedFd,
    dev: u64,
    mnt: Option<u64>,
    path: &Path,
    mounts: &mut MountCheck,
    freed: &mut u64,
) -> Result<(), RemoveError> {
    let names = list(fd.as_fd(), path)?;
    let mut stack = vec![Level {
        fd,
        dev,
        mnt,
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
            if stat.mount == Some(true) || stat.dev != level.dev {
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
            let mnt = match stat.mount {
                Some(_) => None,
                None => mounts.check(level.fd.as_fd(), &mut level.mnt, fd.as_fd(), &child)?,
            };
            let names = list(fd.as_fd(), &child)?;
            stack.push(Level {
                fd,
                dev: opened.dev,
                mnt,
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
    if !one_name(name.as_bytes()) {
        return Err(RemoveError::Io {
            path: parent.path.join(name),
            source: io::Error::new(io::ErrorKind::InvalidInput, "not one name in the directory"),
        });
    }
    Ok(parent.path.join(name))
}

/// Whether `name` is one name in a directory: not empty, not `.` or `..`,
/// and without a `/`.
fn one_name(name: &[u8]) -> bool {
    !name.is_empty() && name != b"." && name != b".." && !name.contains(&b'/')
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
    /// Whether it is a mount point or an automount point, where the read
    /// says (Linux's `statx` from 5.8; an automount point from its first
    /// version): one is never entered, whatever its device. `None` where
    /// it does not say (`statx` before 5.8, `fstatat`, macOS's `lstat`):
    /// a directory is then entered only once [`MountCheck`] has found it
    /// on its parent's mount.
    mount: Option<bool>,
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
            mount: None,
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
    #[cfg(test)]
    let kernel = hook::kernel();
    #[cfg(test)]
    if kernel.no_statx {
        return Err(io::Error::from_raw_os_error(libc::ENOSYS));
    }
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
            #[cfg_attr(not(test), allow(unused_mut))]
            let mut stx = unsafe { buf.assume_init() };
            // Before 5.8 the mask holds neither bit.
            #[cfg(test)]
            if kernel.no_mount_root {
                stx.stx_attributes_mask &= !u64::from(
                    (libc::STATX_ATTR_MOUNT_ROOT | libc::STATX_ATTR_AUTOMOUNT).cast_unsigned(),
                );
            }
            let bit = |attribute: c_int| u64::from(attribute.cast_unsigned());
            let set = |attribute: c_int| stx.stx_attributes & bit(attribute) != 0;
            // The kernel flags an automount point from statx's first version
            // (4.11), but puts the bit in the mask, with the mount root's,
            // only from 5.8: before, an unflagged directory may be either.
            let mount = if set(libc::STATX_ATTR_AUTOMOUNT) {
                Some(true)
            } else if stx.stx_attributes_mask & bit(libc::STATX_ATTR_MOUNT_ROOT) != 0 {
                Some(set(libc::STATX_ATTR_MOUNT_ROOT))
            } else {
                None
            };
            return Ok(Stat {
                dev: libc::makedev(stx.stx_dev_major, stx.stx_dev_minor),
                ino: stx.stx_ino,
                dir: u32::from(stx.stx_mode) & libc::S_IFMT == libc::S_IFDIR,
                nlink: u64::from(stx.stx_nlink),
                bytes: stx.stx_blocks.saturating_mul(512),
                mount,
            });
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

/// How a removal tells whether a directory it has just opened is a mount
/// point of its parent's device (a bind mount), where `statx` did not say.
///
/// Linux, in order:
/// - **The descriptors' mounts.** `name_to_handle_at(2)` names the mount
///   a descriptor is on (Linux 2.6.39): the directory is on its parent's
///   mount, or it is a mount point. The answer is the descriptor's own,
///   so no mount made meanwhile can change it.
/// - **The mount table.** Where that call fails (a filesystem without file
///   handles before 6.5, a sandbox that refuses the call), the process's
///   mount table (`/proc/self/mountinfo`), read once for the directory:
///   a mount point listed at it or under it fails the removal before
///   anything there is removed, and none listed lets the walk go on under
///   it without asking again.
/// - **Neither.** The directory is never entered:
///   [`RemoveError::MountUnknown`].
#[derive(Debug, Default)]
struct MountCheck {
    /// A directory the mount table held no mount point at or under when it
    /// was read: what is under it is on its mount.
    #[cfg(target_os = "linux")]
    clear: Option<PathBuf>,
}

#[cfg(target_os = "linux")]
impl MountCheck {
    /// Whether the directory open on `fd`, at `path`, just opened in the
    /// directory open on `up`, is on `up`'s mount: its mount id when it is
    /// and the ids were read (`up`'s kept in `up_mnt`), for the directories
    /// under it to compare with.
    ///
    /// # Errors
    ///
    /// [`RemoveError::Mount`] when it is a mount point, or the mount table
    /// lists one at it or under it; [`RemoveError::MountUnknown`] when
    /// neither the descriptors nor the mount table can tell.
    fn check(
        &mut self,
        up: BorrowedFd<'_>,
        up_mnt: &mut Option<u64>,
        fd: BorrowedFd<'_>,
        path: &Path,
    ) -> Result<Option<u64>, RemoveError> {
        if self
            .clear
            .as_deref()
            .is_some_and(|clear| path.starts_with(clear))
        {
            return Ok(None);
        }
        let up_id = match *up_mnt {
            Some(id) => Ok(id),
            None => mount_id(up),
        };
        if let Ok((up_id, id)) = up_id.and_then(|up_id| Ok((up_id, mount_id(fd)?))) {
            *up_mnt = Some(up_id);
            return if id == up_id {
                Ok(Some(id))
            } else {
                Err(RemoveError::Mount {
                    path: path.to_owned(),
                })
            };
        }
        match mount_point_under(path) {
            Ok(Some(point)) => Err(RemoveError::Mount { path: point }),
            Ok(None) => {
                self.clear = Some(path.to_owned());
                Ok(None)
            }
            Err(source) => Err(RemoveError::MountUnknown {
                path: path.to_owned(),
                source,
            }),
        }
    }
}

#[cfg(not(target_os = "linux"))]
impl MountCheck {
    /// Whether the directory just opened is on its parent's mount: on
    /// macOS, always, once its device is its parent's. Every mount there
    /// has a device of its own (its own `f_fsid`, which its entries'
    /// `st_dev` reports, nullfs's included), and there are no bind mounts.
    #[allow(clippy::unnecessary_wraps, clippy::unused_self)]
    fn check(
        &mut self,
        _up: BorrowedFd<'_>,
        _up_mnt: &mut Option<u64>,
        _fd: BorrowedFd<'_>,
        _path: &Path,
    ) -> Result<Option<u64>, RemoveError> {
        Ok(None)
    }
}

/// The id of the mount the descriptor `fd` is on, as `name_to_handle_at(2)`
/// gives it beside a handle (since Linux 2.6.39; the id mountinfo lists).
/// Fails where the call is refused (a seccomp sandbox) or the filesystem
/// has no file handles (`EOPNOTSUPP`, before Linux 6.5).
#[cfg(target_os = "linux")]
fn mount_id(fd: BorrowedFd<'_>) -> io::Result<u64> {
    /// `struct file_handle` with room for the longest handle,
    /// `MAX_HANDLE_SZ`.
    #[repr(C)]
    struct Handle {
        bytes: u32,
        kind: c_int,
        data: [u8; 128],
    }
    #[cfg(test)]
    if hook::kernel().no_handles {
        return Err(io::Error::from_raw_os_error(libc::EOPNOTSUPP));
    }
    let mut handle = Handle {
        bytes: 128,
        kind: 0,
        data: [0; 128],
    };
    // Mount ids are never negative: -1 is one the call did not write.
    let mut id: c_int = -1;
    loop {
        // The system call itself, as for statx: the musl that release
        // builds link statically may have no wrapper.
        // SAFETY: `fd` is open for the borrow, the empty name is
        // NUL-terminated, `handle` is writable for a `struct file_handle`
        // with the `bytes` of handle it declares, and `id` for one int; the
        // arguments are the system call's, in its order.
        let rc = unsafe {
            libc::syscall(
                libc::SYS_name_to_handle_at,
                fd.as_raw_fd(),
                c"".as_ptr(),
                &raw mut handle,
                &raw mut id,
                libc::AT_EMPTY_PATH,
            )
        };
        if rc != 0 {
            let error = io::Error::last_os_error();
            match error.raw_os_error() {
                Some(libc::EINTR) => continue,
                // No handle for this inode, or a longer one than the
                // buffer: the mount id is written all the same.
                Some(libc::EOVERFLOW) => {}
                _ => return Err(error),
            }
        }
        return u64::try_from(id).map_err(|_| io::Error::other("no mount id"));
    }
}

/// The mount point nearest `dir` at it or under it in the mount table, read
/// now; an error where the table cannot be read.
#[cfg(target_os = "linux")]
fn mount_point_under(dir: &Path) -> io::Result<Option<PathBuf>> {
    #[cfg(test)]
    if hook::kernel().no_mount_table {
        return Err(io::Error::from_raw_os_error(libc::ENOENT));
    }
    crate::platform::linux::mount_point_under(dir)
}

/// A test's hook into a removal or a move to the Trash, called with each
/// path just before it is opened, unlinked or renamed (and, on Linux, just
/// after a rename), so a test can change the tree at that moment: the race
/// they guard against, made to happen every time.
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

    #[cfg(target_os = "linux")]
    type Moved = Box<dyn FnMut()>;

    #[cfg(target_os = "linux")]
    thread_local! {
        static MOVED: RefCell<Option<Moved>> = const { RefCell::new(None) };
        static NO_NOREPLACE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    /// Calls `moved` on this thread just after a rename into the Trash has
    /// moved an entry, before what moved is read again, until the returned
    /// guard drops.
    #[cfg(target_os = "linux")]
    pub(crate) fn on_moved(moved: impl FnMut() + 'static) -> MovedGuard {
        MOVED.with(|m| *m.borrow_mut() = Some(Box::new(moved)));
        MovedGuard
    }

    /// Takes the hook away when dropped.
    #[cfg(target_os = "linux")]
    pub(crate) struct MovedGuard;

    #[cfg(target_os = "linux")]
    impl Drop for MovedGuard {
        fn drop(&mut self) {
            MOVED.with(|m| *m.borrow_mut() = None);
        }
    }

    #[cfg(target_os = "linux")]
    pub(super) fn moved() {
        MOVED.with(|m| {
            if let Some(moved) = m.borrow_mut().as_mut() {
                moved();
            }
        });
    }

    /// Makes `renameat2` refuse `RENAME_NOREPLACE` with `EINVAL` on this
    /// thread, as NFS, or FUSE without `RENAME2`, does, until the returned
    /// guard drops.
    #[cfg(target_os = "linux")]
    pub(crate) fn without_noreplace() -> NoreplaceGuard {
        NO_NOREPLACE.with(|n| n.set(true));
        NoreplaceGuard
    }

    /// Gives `renameat2` its flag back when dropped.
    #[cfg(target_os = "linux")]
    pub(crate) struct NoreplaceGuard;

    #[cfg(target_os = "linux")]
    impl Drop for NoreplaceGuard {
        fn drop(&mut self) {
            NO_NOREPLACE.with(|n| n.set(false));
        }
    }

    #[cfg(target_os = "linux")]
    pub(super) fn no_noreplace() -> bool {
        NO_NOREPLACE.with(std::cell::Cell::get)
    }

    /// What the kernel answers a removal, as a test makes it: an older or
    /// more confined kernel than the one the tests run on (Linux).
    #[cfg(target_os = "linux")]
    #[derive(Debug, Clone, Copy, Default)]
    #[allow(clippy::struct_excessive_bools)] // four answers, each on or off
    pub(crate) struct Kernel {
        /// `statx` reports no mount-root attribute in its mask, as before
        /// Linux 5.8.
        pub(crate) no_mount_root: bool,
        /// `statx` fails with `ENOSYS`, as in a sandbox that refuses it.
        pub(crate) no_statx: bool,
        /// `name_to_handle_at` fails with `EOPNOTSUPP`, as on a filesystem
        /// without file handles or in a sandbox that refuses the call.
        pub(crate) no_handles: bool,
        /// `/proc/self/mountinfo` cannot be read, as without `/proc`.
        pub(crate) no_mount_table: bool,
    }

    #[cfg(target_os = "linux")]
    thread_local! {
        static KERNEL: std::cell::Cell<Kernel> = const {
            std::cell::Cell::new(Kernel {
                no_mount_root: false,
                no_statx: false,
                no_handles: false,
                no_mount_table: false,
            })
        };
    }

    /// Makes the kernel answer as `kernel` says on this thread until the
    /// returned guard drops.
    #[cfg(target_os = "linux")]
    pub(crate) fn as_kernel(kernel: Kernel) -> KernelGuard {
        KERNEL.with(|k| k.set(kernel));
        KernelGuard
    }

    /// Gives the kernel back its own answers when dropped.
    #[cfg(target_os = "linux")]
    pub(crate) struct KernelGuard;

    #[cfg(target_os = "linux")]
    impl Drop for KernelGuard {
        fn drop(&mut self) {
            KERNEL.with(|k| k.set(Kernel::default()));
        }
    }

    #[cfg(target_os = "linux")]
    pub(super) fn kernel() -> Kernel {
        KERNEL.with(std::cell::Cell::get)
    }
}

#[cfg(test)]
mod tests;
