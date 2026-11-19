//! The files the scanning user's processes hold open for writing (P5-CORE-11,
//! DEV-214). `FSEvents` reports a change to a file's contents when the file
//! is closed, not when it is written, so an incremental rescan reads again
//! the directory of each file still open for writing. libproc lists them
//! with no privilege and opens no file: the processes of one effective user
//! (`proc_listpids`), each one's descriptors (`PROC_PIDLISTFDS`), and each
//! vnode descriptor's open flags and path (`PROC_PIDFDVNODEPATHINFO`). A
//! process that exits, or a descriptor closed, while they are read is
//! passed over, as is a process the user may not read.
//!
//! The kernel spells a path on the Data volume through its firmlinks
//! (`/Users/a/f`), where a walk that enters the volume at its mount point
//! reaches the file as `/System/Volumes/Data/Users/a/f`: both spellings are
//! given when both lie below the root, and the rescan keeps the one its
//! saved scan holds.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use super::{DATA_MOUNT, ffi, firmlinks};

/// The regular files below `under` that the processes whose effective user
/// is `uid` hold open for writing, each once, in order.
pub(super) fn open_for_write(under: &Path, uid: u32) -> Vec<PathBuf> {
    let pids = match ffi::pids_of_user(uid) {
        Ok(pids) => pids,
        Err(e) => {
            log::debug!("cannot list the processes of user {uid}: {e}");
            return Vec::new();
        }
    };
    let vnode = u32::try_from(libc::PROX_FDTYPE_VNODE).unwrap_or(u32::MAX);
    let mut found = BTreeSet::new();
    for pid in pids {
        let Ok(fds) = ffi::fds_of(pid) else {
            continue;
        };
        for fd in fds.iter().filter(|fd| fd.proc_fdtype == vnode) {
            let Ok(file) = ffi::vnode_fd(pid, fd.proc_fd) else {
                continue;
            };
            if file.open_flags & ffi::FWRITE == 0 || file.vtype != ffi::VREG {
                continue;
            }
            let path = PathBuf::from(OsStr::from_bytes(&file.path));
            for path in spellings(path) {
                if path != under && path.starts_with(under) {
                    found.insert(path);
                }
            }
        }
    }
    found.into_iter().collect()
}

/// `path`, as the kernel builds it, and where it passes through a firmlink,
/// the same file below the Data volume's mount point.
fn spellings(path: PathBuf) -> Vec<PathBuf> {
    let data = firmlinks().iter().find_map(|link| {
        let rest = path.strip_prefix(&link.source).ok()?;
        Some(Path::new(DATA_MOUNT).join(&link.target).join(rest))
    });
    [Some(path), data].into_iter().flatten().collect()
}
