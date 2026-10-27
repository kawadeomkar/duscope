//! The macOS provider: `getattrlistbulk(2)` with the basic attribute list,
//! a stat queue for directories that wait on I/O, and the fallback provider
//! for filesystems without bulk reads.

mod bulk;
mod ffi;
mod stat;
#[cfg(test)]
mod tests;
mod volume;

pub(super) use stat::from_stat;
pub(super) use volume::{mount_info, of as volume};

use std::cell::RefCell;
use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Once, OnceLock};

use bulk::{Decoded, Outcome};
use ffi::AttrBuf;

use crate::platform::fallback::FallbackProvider;
use crate::platform::posix;
use crate::{
    Boundary, DirHandle, Entry, NameRef, Provider, ScanError, Skip, VolumeInfo, WalkOptions,
};

thread_local! {
    /// The devices each worker has checked for clone ids.
    static CHECKED: RefCell<HashSet<u64>> = RefCell::new(HashSet::new());
    /// Each worker's bulk buffer, reused for every directory it reads.
    static BULK: RefCell<AttrBuf> = RefCell::new(AttrBuf::new(bulk::BUFFER_BYTES));
    /// Each worker's buffer for single-entry reads.
    static SINGLE: RefCell<AttrBuf> = RefCell::new(AttrBuf::new(stat::BUFFER_BYTES));
}

/// The macOS provider.
#[derive(Debug, Default)]
pub(crate) struct MacProvider {
    /// Set when `--clones` reads a directory on a volume without clone ids.
    clones_unavailable: AtomicBool,
    /// Test knobs: read every directory through the stat queue as if the
    /// bulk read had answered this.
    #[cfg(test)]
    pretend: Option<Outcome>,
}

impl MacProvider {
    /// The provider. The first one made sets up the process: the
    /// descriptor limit is raised and dataless files are never downloaded.
    pub(crate) fn new() -> MacProvider {
        static SETUP: Once = Once::new();
        SETUP.call_once(|| {
            posix::raise_nofile_limit();
            // A failure leaves the default policy; nothing else can be done.
            let _ = ffi::disable_dataless_materialization();
        });
        MacProvider::default()
    }

    /// Notes whether the volume of the directory open on `fd`, which holds
    /// an entry on device `dev`, has clone ids; each worker checks a device
    /// once.
    fn check_clones(&self, fd: BorrowedFd<'_>, dev: u64) {
        if !CHECKED.with_borrow_mut(|checked| checked.insert(dev)) {
            return;
        }
        let apfs = ffi::fstatfs(fd).is_ok_and(|fs| {
            let name: Vec<u8> = fs
                .f_fstypename
                .iter()
                .take_while(|&&c| c != 0)
                .map(|&c| c.cast_unsigned())
                .collect();
            name == bulk::CLONE_FS.as_bytes()
        });
        if !apfs {
            self.clones_unavailable.store(true, Ordering::Relaxed);
        }
    }
}

/// The firmlinks of the System volume (`/usr/share/firmlinks`: a source
/// path and its place on the Data volume per line).
fn firmlinks() -> &'static [PathBuf] {
    static FIRMLINKS: OnceLock<Vec<PathBuf>> = OnceLock::new();
    FIRMLINKS.get_or_init(|| {
        let Ok(text) = fs::read("/usr/share/firmlinks") else {
            return Vec::new();
        };
        text.split(|&b| b == b'\n')
            .filter_map(|line| line.split(|&b| b == b'\t').next())
            .filter(|source| source.starts_with(b"/"))
            .map(|source| PathBuf::from(OsStr::from_bytes(source)))
            .collect()
    })
}

/// The device of the Data volume: the one the firmlinks lead to.
fn data_volume() -> Option<u64> {
    static DATA: OnceLock<Option<u64>> = OnceLock::new();
    *DATA.get_or_init(|| {
        firmlinks().iter().find_map(|source| {
            let stat = posix::lstat(&posix::c_path(source).ok()?).ok()?;
            Some(bulk::dev(stat.st_dev))
        })
    })
}

impl Provider for MacProvider {
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
        let fd = posix::open_dir(dir.path())?;
        let mut decoded = Vec::new();
        #[cfg(test)]
        let pretend = self.pretend;
        #[cfg(not(test))]
        let pretend = None;
        let outcome = match pretend {
            Some(outcome) => outcome,
            // The probe only pays off when other workers can share the
            // stat queue.
            None => BULK.with_borrow_mut(|buf| {
                bulk::read(fd.as_fd(), opts, buf, opts.threads != 1, &mut decoded)
            })?,
        };
        match outcome {
            Outcome::Done => {}
            // The fallback provider reads a filesystem without bulk reads,
            // and reads no clone ids.
            Outcome::Unsupported => {
                if opts.clones {
                    self.clones_unavailable.store(true, Ordering::Relaxed);
                }
                return FallbackProvider::default().read_dir_split(dir, opts, out, errs, later);
            }
            // Listed by name alone; the stat queue reads every entry, on
            // every worker.
            Outcome::IoBound => {
                later.extend(posix::list_names(fd.as_fd())?);
                return Ok(());
            }
            // Each child is listed as an entry that cannot be read, as `du`
            // and the stat queue report them.
            Outcome::Unsearchable => {
                for name in posix::list_names(fd.as_fd())? {
                    errs.push(ScanError {
                        dir: dir.id(),
                        name,
                        errno: libc::EACCES,
                    });
                }
                return Ok(());
            }
        }
        SINGLE.with_borrow_mut(|buf| {
            for item in decoded {
                match item {
                    Decoded::Entry(entry) => out.push(entry),
                    Decoded::Error(name, errno) => errs.push(ScanError {
                        dir: dir.id(),
                        name,
                        errno,
                    }),
                    Decoded::Restat(name, boundary) => {
                        match stat::entry_at(fd.as_fd(), &name, opts, buf) {
                            Ok(mut entry) => {
                                if let Some(boundary) = boundary {
                                    entry.boundary = boundary;
                                }
                                out.push(entry);
                            }
                            Err(e) => errs.push(scan_error(dir, name, &e)),
                        }
                    }
                    Decoded::Nameless => {}
                }
            }
        });
        if opts.clones
            && let Some(entry) = out.iter().find(|e| e.boundary == Boundary::None)
        {
            self.check_clones(fd.as_fd(), entry.dev);
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
        let fd = posix::open_dir(dir.path())?;
        SINGLE.with_borrow_mut(|buf| {
            for name in names {
                match stat::entry_at(fd.as_fd(), name, opts, buf) {
                    Ok(entry) => out.push(entry),
                    Err(e) => errs.push(scan_error(dir, name.clone(), &e)),
                }
            }
        });
        Ok(())
    }

    /// A scan whose root holds every firmlink (a scan of `/`) reaches the
    /// Data volume through them, so the volume's own mount point,
    /// `/System/Volumes/Data`, is listed but not entered: nothing is counted
    /// twice. A scan rooted inside the Data volume is ordinary.
    fn skip(&self, dir: &DirHandle, entry: &Entry, _opts: &WalkOptions) -> Option<Skip> {
        if entry.boundary != Boundary::Mount || Some(entry.dev) != data_volume() {
            return None;
        }
        // The walk root's own path: below a followed link the directory's
        // path is the link's target, not under the root.
        let root = dir.ancestors().last()?.path();
        let sources: Vec<&PathBuf> = firmlinks().iter().filter(|s| s.exists()).collect();
        (!sources.is_empty() && sources.iter().all(|source| source.starts_with(root)))
            .then_some(Skip::Firmlinked)
    }

    fn clones_unavailable(&self, _opts: &WalkOptions) -> bool {
        self.clones_unavailable.load(Ordering::Relaxed)
    }

    fn volume(&self, path: &Path) -> io::Result<VolumeInfo> {
        volume::of(path)
    }

    fn mount_info(&self, path: &Path) -> io::Result<VolumeInfo> {
        volume::mount_info(path)
    }

    fn root(&self, path: &Path, opts: &WalkOptions) -> io::Result<Entry> {
        let name = NameRef::from(path.file_name().unwrap_or(path.as_os_str()));
        stat::entry_of_path(path, name, opts)
    }

    /// dua-cli's macOS default since 2.40.0, until the phase-1 thread sweep
    /// (P1-BENCH-01) sets it.
    fn default_threads(&self) -> usize {
        8
    }
}

/// The scan error for an entry that could not be read.
fn scan_error(dir: &DirHandle, name: NameRef, error: &io::Error) -> ScanError {
    ScanError {
        dir: dir.id(),
        name,
        errno: error.raw_os_error().unwrap_or(crate::EIO),
    }
}
