//! The Linux provider: `getdents64(2)` listings and one dirfd-relative
//! `statx(2)` per entry, with a wide directory's entries spread over every
//! worker in batches of 1,024.

mod dents;
mod mounts;
mod statx;

pub(super) use mounts::Mounts;
pub(super) use statx::from_stat;
#[cfg(test)]
mod tests;

use std::cell::RefCell;
use std::io;
use std::os::fd::AsFd;
use std::path::Path;

use dents::DentBuf;

use crate::platform::posix;
use crate::{
    DirHandle, Entry, Flags, Kind, NameRef, Provider, STAT_BATCH, ScanError, VolumeInfo,
    WalkOptions,
};

thread_local! {
    /// Each worker's listing buffer, reused for every directory it reads.
    static DENTS: RefCell<DentBuf> = RefCell::new(DentBuf::new());
}

/// The Linux provider.
#[derive(Debug, Default)]
pub(crate) struct LinuxProvider {
    /// The mount table, read for this walk.
    mounts: Mounts,
    /// `statx` calls made, for the test that counts them.
    #[cfg(test)]
    statx_calls: std::sync::atomic::AtomicUsize,
    /// Of those, reads of a listed directory's own subvolume.
    #[cfg(test)]
    subvol_reads: std::sync::atomic::AtomicUsize,
}

impl LinuxProvider {
    /// The provider. Making one sets up the process (see
    /// [`platform::setup`](super::setup)).
    pub(crate) fn new() -> LinuxProvider {
        super::setup();
        LinuxProvider::default()
    }

    /// Reads `names` in the directory open on `fd`: an entry or an error
    /// each, one `statx` per name, each name moving into its entry or error.
    /// A directory that is a different subvolume from `fd`'s is flagged;
    /// `fd`'s own subvolume is read once, and only when a child reports one.
    fn read_names(
        &self,
        fd: std::os::fd::BorrowedFd<'_>,
        dir: &DirHandle,
        names: impl IntoIterator<Item = NameRef>,
        opts: &WalkOptions,
        out: &mut Vec<Entry>,
        errs: &mut Vec<ScanError>,
    ) {
        let mask = statx::mask(opts);
        let mut dir_subvol = None;
        for name in names {
            #[cfg(test)]
            self.statx_calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            match statx::statx_at(fd, &name, mask) {
                Ok(stx) => {
                    let mut entry = statx::entry(name, &stx, opts.attrs);
                    self.mounts.fill_mnt_id(&mut entry, dir);
                    if entry.kind == Kind::Dir && stx.stx_mask & libc::STATX_SUBVOL != 0 {
                        let parent = *dir_subvol.get_or_insert_with(|| {
                            #[cfg(test)]
                            for counter in [&self.statx_calls, &self.subvol_reads] {
                                counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            }
                            statx::subvol_of(fd)
                        });
                        entry.flags.set(
                            Flags::SUBVOLUME,
                            parent.is_some_and(|p| p != stx.stx_subvol),
                        );
                    }
                    out.push(entry);
                }
                Err(e) => errs.push(ScanError {
                    dir: dir.id(),
                    name,
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
        self.read_names(fd.as_fd(), dir, names, opts, out, errs);
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
        self.read_names(fd.as_fd(), dir, names.iter().cloned(), opts, out, errs);
        Ok(())
    }

    /// Clone ids are read only on APFS; on Linux, `explain --clones` finds
    /// shared extents on demand.
    fn clones_unavailable(&self, _opts: &WalkOptions) -> bool {
        true
    }

    fn volume(&self, path: &Path) -> io::Result<VolumeInfo> {
        self.mounts.volume(path)
    }

    fn mount_info(&self, path: &Path, _dev: u64) -> io::Result<VolumeInfo> {
        self.mounts.mount_info(path)
    }

    fn root(&self, path: &Path, opts: &WalkOptions) -> io::Result<Entry> {
        let name = NameRef::from(path.file_name().unwrap_or(path.as_os_str()));
        #[cfg(test)]
        self.statx_calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // The root is read through an automount, as opening it will be.
        let stx = statx::statx_through(&posix::c_path(path)?, statx::mask(opts))?;
        let mut entry = statx::entry(name, &stx, opts.attrs);
        if entry.mnt_id == 0 {
            entry.mnt_id = self.mounts.mnt_id_of(path);
        }
        Ok(entry)
    }
}
