//! The Linux provider: `getdents64(2)` listings and one dirfd-relative
//! `statx(2)` per entry, with a wide directory's entries spread over every
//! worker in batches of 1,024.

mod dents;
mod mounts;
mod statx;

pub(super) use mounts::volume;
pub(super) use statx::from_stat;
#[cfg(test)]
mod tests;

use std::cell::RefCell;
use std::io;
use std::os::fd::AsFd;
use std::path::Path;
use std::sync::Once;

use dents::DentBuf;

use crate::platform::posix;
use crate::{DirHandle, Entry, NameRef, Provider, STAT_BATCH, ScanError, VolumeInfo, WalkOptions};

thread_local! {
    /// Each worker's listing buffer, reused for every directory it reads.
    static DENTS: RefCell<DentBuf> = RefCell::new(DentBuf::new());
}

/// The Linux provider.
#[derive(Debug, Default)]
pub(crate) struct LinuxProvider {
    /// `statx` calls made, for the test that counts them.
    #[cfg(test)]
    statx_calls: std::sync::atomic::AtomicUsize,
}

impl LinuxProvider {
    /// The provider. The first one made raises the descriptor limit.
    pub(crate) fn new() -> LinuxProvider {
        static SETUP: Once = Once::new();
        SETUP.call_once(posix::raise_nofile_limit);
        LinuxProvider::default()
    }

    /// Reads `names` in the directory open on `fd`: an entry or an error
    /// each, one `statx` per name.
    #[cfg_attr(not(test), allow(clippy::unused_self))]
    fn read_names(
        &self,
        fd: std::os::fd::BorrowedFd<'_>,
        dir: &DirHandle,
        names: &[NameRef],
        opts: &WalkOptions,
        out: &mut Vec<Entry>,
        errs: &mut Vec<ScanError>,
    ) {
        #[cfg(test)]
        self.statx_calls
            .fetch_add(names.len(), std::sync::atomic::Ordering::Relaxed);
        for name in names {
            match statx::entry_at(fd, name, opts) {
                Ok(mut entry) => {
                    fill_mnt_id(&mut entry, dir);
                    out.push(entry);
                }
                Err(e) => errs.push(ScanError {
                    dir: dir.id(),
                    name: name.clone(),
                    errno: e.raw_os_error().unwrap_or(crate::EIO),
                }),
            }
        }
    }
}

impl Provider for LinuxProvider {
    fn read_dir(
        &self,
        dir: &DirHandle,
        opts: &WalkOptions,
        out: &mut Vec<Entry>,
        errs: &mut Vec<ScanError>,
    ) -> io::Result<()> {
        let mut later = Vec::new();
        self.read_dir_split(dir, opts, out, errs, &mut later)?;
        if later.is_empty() {
            Ok(())
        } else {
            self.stat_names(dir, &later, opts, out, errs)
        }
    }

    /// Lists the directory, reads the first 1,024 entries here and leaves
    /// the rest for stat batches on any worker.
    fn read_dir_split(
        &self,
        dir: &DirHandle,
        opts: &WalkOptions,
        out: &mut Vec<Entry>,
        errs: &mut Vec<ScanError>,
        later: &mut Vec<NameRef>,
    ) -> io::Result<()> {
        let fd = posix::open_dir(dir.path())?;
        let mut names = Vec::new();
        DENTS.with_borrow_mut(|buf| dents::list(fd.as_fd(), buf, &mut names))?;
        if names.len() > STAT_BATCH {
            later.extend(names.drain(STAT_BATCH..));
        }
        self.read_names(fd.as_fd(), dir, &names, opts, out, errs);
        Ok(())
    }

    fn stat_names(
        &self,
        dir: &DirHandle,
        names: &[NameRef],
        opts: &WalkOptions,
        out: &mut Vec<Entry>,
        errs: &mut Vec<ScanError>,
    ) -> io::Result<()> {
        let fd = posix::open_dir(dir.path())?;
        self.read_names(fd.as_fd(), dir, names, opts, out, errs);
        Ok(())
    }

    /// Clone ids are read only on APFS; on Linux, `explain --clones` finds
    /// shared extents on demand.
    fn clones_unavailable(&self, _opts: &WalkOptions) -> bool {
        true
    }

    fn volume(&self, path: &Path) -> io::Result<VolumeInfo> {
        volume(path)
    }

    fn root(&self, path: &Path, opts: &WalkOptions) -> io::Result<Entry> {
        let name = NameRef::from(path.file_name().unwrap_or(path.as_os_str()));
        #[cfg(test)]
        self.statx_calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let stx = statx::statx(None, &posix::c_path(path)?, statx::mask(opts))?;
        let mut entry = statx::entry(name, &stx, opts.attrs);
        if entry.mnt_id == 0 {
            entry.mnt_id = mounts::mnt_id_of(path);
        }
        Ok(entry)
    }
}

/// Gives an entry whose mount id `statx` did not report (before Linux 5.8,
/// or read by `stat`) the id of its directory's mount, or of the mount
/// holding it when it sits on another device.
pub(super) fn fill_mnt_id(entry: &mut Entry, dir: &DirHandle) {
    if entry.mnt_id == 0 {
        entry.mnt_id = if entry.dev == dir.dev() {
            dir.mnt_id()
        } else {
            mounts::mnt_id_of(&dir.path().join(entry.name.as_os_str()))
        };
    }
}
