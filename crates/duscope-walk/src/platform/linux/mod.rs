//! The Linux provider: `getdents64(2)` listings and one dirfd-relative
//! `statx(2)` per entry, with a wide directory's entries spread over every
//! worker in batches of 1,024. Under the walk's counters it times the two
//! apart: opening and listing a directory, and reading its entries.

mod dents;
pub(in crate::platform) mod fiemap;
mod mounts;
mod statx;
pub(in crate::platform) mod trash;

pub(super) use mounts::{Mounts, mount_point_under};
pub(super) use statx::from_stat;
#[cfg(test)]
mod tests;

use std::cell::RefCell;
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::path::Path;

use dents::DentBuf;

use crate::counters::{self, Timer};
use crate::platform::posix;
use crate::{
    Boundary, Counters, DirHandle, Entry, Flags, Flush, Kind, NameRef, Provider, STAT_BATCH,
    STREAM_CHUNK, ScanError, VolumeInfo, WalkOptions,
};

/// Filesystems by the magic number `statfs` gives as their type
/// (`linux/magic.h`).
mod magic {
    pub(super) const BTRFS: u32 = 0x9123_683E;
    pub(super) const XFS: u32 = 0x5846_5342;
    pub(super) const BCACHEFS: u32 = 0xCA45_1A4E;
    /// ext2 and ext3 too.
    pub(super) const EXT4: u32 = 0xEF53;
    pub(super) const F2FS: u32 = 0xF2F5_2010;
    pub(super) const NTFS3: u32 = 0x7366_746E;
}

/// The filesystems whose `STATX_SUBVOL` numbers their own subvolumes.
const SUBVOLUMES: [u32; 2] = [magic::BTRFS, magic::BCACHEFS];

/// The magic number of the filesystem holding the file open on `fd`
/// (`fstatfs(2)`, which a descriptor opened as a path only answers too).
fn fs_magic(fd: BorrowedFd<'_>) -> io::Result<u32> {
    let mut buf = MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: `fd` is open for the borrow and `buf` is writable for one
    // `struct statfs`.
    if unsafe { libc::fstatfs(fd.as_raw_fd(), buf.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fstatfs succeeded, so it filled the struct.
    let stat = unsafe { buf.assume_init() };
    // `f_type` is a signed long to glibc and an unsigned one to musl; the
    // magic numbers are 32 bits either way.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Ok(stat.f_type as u32)
}

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
    /// A directory that is another subvolume than `fd`'s is a
    /// [`Boundary::Subvolume`] ([`subvolume`](Self::subvolume)); `fd`'s own
    /// subvolume is read once, and only when a child reports one.
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
        let dir_subvol = std::cell::OnceCell::new();
        for name in names {
            #[cfg(test)]
            self.statx_calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            match statx::statx_at(fd, &name, mask) {
                Ok(stx) => {
                    let mut entry = statx::entry(name, &stx, opts.attrs);
                    self.mounts.fill_mnt_id(&mut entry, dir);
                    if entry.kind == Kind::Dir && self.subvolume(fd, dir, &entry, &stx, &dir_subvol)
                    {
                        entry.boundary = Boundary::Subvolume;
                        entry.flags.insert(Flags::SUBVOLUME);
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

    /// Whether the directory `entry`, read by `stx` in `dir` (open on
    /// `fd`), is another subvolume than `dir`'s: a btrfs or ZFS mount root
    /// the mount table names as another subvolume or dataset of the
    /// filesystem around it; else another subvolume id (`STATX_SUBVOL`,
    /// Linux 6.10), where `dir` is on a filesystem that numbers its
    /// subvolumes (`dir`'s own read once into `dir_subvol`, and only when
    /// a child reports one); else another device inside the same mount,
    /// which is how btrfs tells its subvolumes apart before 6.10.
    fn subvolume(
        &self,
        fd: std::os::fd::BorrowedFd<'_>,
        dir: &DirHandle,
        entry: &Entry,
        stx: &libc::statx,
        dir_subvol: &std::cell::OnceCell<Option<u64>>,
    ) -> bool {
        if entry.boundary == Boundary::Mount {
            return entry.flags.contains(Flags::MOUNT_ROOT)
                && entry.mnt_id != dir.mnt_id()
                && self.mounts.subvolume_mount(dir, entry.name.as_bytes());
        }
        if stx.stx_mask & libc::STATX_SUBVOL != 0
            && let Some(parent) = *dir_subvol.get_or_init(|| {
                // An overlay passes its layers' subvolume ids through, which
                // say nothing of the overlay's own directories.
                if !fs_magic(fd).is_ok_and(|m| SUBVOLUMES.contains(&m)) {
                    return None;
                }
                #[cfg(test)]
                for counter in [&self.statx_calls, &self.subvol_reads] {
                    counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                statx::subvol_of(fd)
            })
        {
            return parent != stx.stx_subvol;
        }
        entry.mnt_id != 0 && entry.mnt_id == dir.mnt_id() && entry.dev != dir.dev()
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

    fn read_dir_split(
        &self,
        dir: &DirHandle,
        opts: &WalkOptions,
        out: &mut Vec<Entry>,
        errs: &mut Vec<ScanError>,
        later: &mut Vec<NameRef>,
    ) -> io::Result<()> {
        self.read_dir_streaming(dir, opts, out, errs, later, &mut |_, _, _| {})
    }

    /// Lists the directory, reads the first 1,024 entries here and leaves
    /// the rest for stat batches on any worker, flushing them to the walk
    /// every [`STREAM_CHUNK`] as they are listed.
    fn read_dir_streaming(
        &self,
        dir: &DirHandle,
        opts: &WalkOptions,
        out: &mut Vec<Entry>,
        errs: &mut Vec<ScanError>,
        later: &mut Vec<NameRef>,
        flush: &mut Flush<'_>,
    ) -> io::Result<()> {
        let timed = opts.counters != Counters::Off;
        let listing = timed.then(Timer::start);
        let fd = posix::open_handle(dir)?;
        let mut names = Vec::new();
        let listed = DENTS.with_borrow_mut(|buf| {
            dents::list(fd.as_fd(), buf, &mut names, &mut |names| {
                if names.len() > STAT_BATCH {
                    later.extend(names.drain(STAT_BATCH..));
                    if later.len() >= STREAM_CHUNK {
                        flush(out, errs, later);
                    }
                }
            })
        });
        if let Some(listing) = listing {
            let took = listing.elapsed();
            counters::add(|tally| tally.listing += took);
        }
        listed?;
        let stat = timed.then(Timer::start);
        self.read_names(fd.as_fd(), dir, names, opts, out, errs);
        if let Some(stat) = stat {
            let took = stat.elapsed();
            counters::add(|tally| tally.stat += took);
        }
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
        let stat = (opts.counters != Counters::Off).then(Timer::start);
        let fd = posix::reopen(dir)?;
        self.read_names(fd.as_fd(), dir, names.iter().cloned(), opts, out, errs);
        if let Some(stat) = stat {
            let took = stat.elapsed();
            counters::add(|tally| tally.stat += took);
        }
        Ok(())
    }

    fn times_listing(&self) -> bool {
        true
    }

    /// Clone ids are read only on APFS; on Linux, `explain --clones` finds
    /// shared extents on demand.
    fn clones_unavailable(&self, _opts: &WalkOptions) -> bool {
        true
    }

    fn untriggered_automount(&self, entry: &Entry) -> bool {
        statx::untriggered_automount(entry)
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
