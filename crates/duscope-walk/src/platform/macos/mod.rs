//! The macOS provider: `getattrlistbulk(2)` with the basic attribute list,
//! a stat queue for directories that wait on I/O, and the fallback provider
//! for filesystems without bulk reads.

mod bulk;
mod ffi;
pub(in crate::platform) mod fsevents;
mod stat;
#[cfg(test)]
mod tests;
pub(in crate::platform) mod trash;
mod volume;

pub(super) use stat::from_stat;
pub(super) use volume::{
    full_disk_access_missing, host_app, mount_info, mount_point_under, of as volume, reaches,
};

use std::cell::RefCell;
use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use bulk::{Clock, Decoded, Outcome};
use ffi::AttrBuf;

use crate::platform::fallback::FallbackProvider;
use crate::platform::posix;
use crate::{
    Boundary, DirHandle, Entry, Flush, NameRef, Provider, STREAM_CHUNK, ScanError, Skip,
    VolumeInfo, WalkOptions,
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
    /// The walk root as given and its real path, resolved once.
    root: Mutex<Option<(PathBuf, Option<PathBuf>)>>,
    /// Test knobs: read every directory through the stat queue as if the
    /// bulk read had answered this.
    #[cfg(test)]
    pretend: Option<Outcome>,
    /// Time the probe on a clock where every refill waits on I/O.
    #[cfg(test)]
    waiting: bool,
    /// How many directories the probe handed to the stat queue.
    #[cfg(test)]
    switched: std::sync::atomic::AtomicUsize,
}

impl MacProvider {
    /// The provider. Making one sets up the process (see
    /// [`platform::setup`](super::setup)).
    pub(crate) fn new() -> MacProvider {
        super::setup();
        MacProvider::default()
    }

    /// The real path of the walk root `given`, which may hold `..` or be
    /// relative, resolved once per root: the firmlink decisions compare
    /// absolute, normalised paths.
    fn real_root(&self, given: &Path) -> Option<PathBuf> {
        let mut root = self.root.lock().ok()?;
        match &*root {
            Some((cached, real)) if cached == given => real.clone(),
            _ => {
                let real = fs::canonicalize(given).ok();
                *root = Some((given.to_owned(), real.clone()));
                real
            }
        }
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

/// A firmlink of the System volume: its path, and the directory of the Data
/// volume it leads to, relative to that volume's root.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Firmlink {
    source: PathBuf,
    target: PathBuf,
}

impl Firmlink {
    /// The firmlink a line of the system's table describes: a source path,
    /// a tab, and its place on the Data volume. A line without that place is
    /// read as naming the same path there, as every line of the table does.
    fn parse(line: &[u8]) -> Option<Firmlink> {
        let mut fields = line.split(|&b| b == b'\t');
        let source = fields.next()?;
        let same = source.strip_prefix(b"/").filter(|rel| !rel.is_empty())?;
        let target = fields.next().filter(|t| !t.is_empty()).unwrap_or(same);
        Some(Firmlink {
            source: PathBuf::from(OsStr::from_bytes(source)),
            target: PathBuf::from(OsStr::from_bytes(target)),
        })
    }
}

/// The firmlinks of the System volume (`/usr/share/firmlinks`: a source
/// path and its place on the Data volume per line). This system table and
/// Linux's `/proc/self/mountinfo` are the only files the scanner reads
/// beside the `CACHEDIR.TAG` check: they describe the layout, not the data.
fn firmlinks() -> &'static [Firmlink] {
    static FIRMLINKS: OnceLock<Vec<Firmlink>> = OnceLock::new();
    FIRMLINKS.get_or_init(|| {
        let Ok(text) = fs::read("/usr/share/firmlinks") else {
            return Vec::new();
        };
        text.split(|&b| b == b'\n')
            .filter_map(Firmlink::parse)
            .collect()
    })
}

/// Where the Data volume is mounted in the boot volume group.
const DATA_MOUNT: &str = "/System/Volumes/Data";

/// The firmlinks of the System volume that exist here, as their paths.
fn firmlink_sources() -> Vec<&'static Path> {
    firmlinks()
        .iter()
        .map(|link| link.source.as_path())
        .filter(|source| source.exists())
        .collect()
}

/// Whether a walk rooted at `root`, absolute and resolved, leaves out the
/// Data volume's way in at `way`, by the firmlink `sources`: see
/// [`MacProvider::skip`]. A walk rooted at the way in enters it, and `-x`
/// keeps the mount point out before this is asked.
fn left_out(way: Boundary, sources: &[&Path], root: &Path, one_file_system: bool) -> bool {
    let every = !sources.is_empty() && sources.iter().all(|source| source.starts_with(root));
    match way {
        Boundary::Firmlink => !every && !one_file_system && Path::new(DATA_MOUNT).starts_with(root),
        Boundary::Mount => every,
        _ => false,
    }
}

/// Whether a walk rooted at `folder`, below `root` (both absolute and
/// resolved), goes into the Data volume differently under it than a walk of
/// `root` goes there.
pub(super) fn walk_differs_below(root: &Path, folder: &Path, opts: &WalkOptions) -> bool {
    differs_by(
        &firmlink_sources(),
        root,
        folder,
        opts.one_file_system,
        opts.follow_symlinks,
    )
}

/// [`walk_differs_below`], by the firmlink `sources`. Each way in that a
/// walk of the folder can meet is asked of the two roots, its mount point
/// and its firmlinks alike: at or below the folder, or anywhere when a
/// followed link can lead there. The folder itself is a walk's root, which
/// it always enters.
fn differs_by(
    sources: &[&Path],
    root: &Path,
    folder: &Path,
    one_file_system: bool,
    follow_symlinks: bool,
) -> bool {
    let leaves_out = |way: Boundary, from: &Path| {
        (way == Boundary::Mount && one_file_system) || left_out(way, sources, from, one_file_system)
    };
    let differs_at = |way: Boundary, at: &Path| {
        (follow_symlinks || at.starts_with(folder))
            && leaves_out(way, root) != (at != folder && leaves_out(way, folder))
    };
    !sources.is_empty()
        && (differs_at(Boundary::Mount, Path::new(DATA_MOUNT))
            || sources
                .iter()
                .any(|&source| differs_at(Boundary::Firmlink, source)))
}

/// The device of the Data volume: the one the firmlinks lead to, read
/// the way entries read theirs (`FSOPT_RETURN_REALDEV`), so the two compare
/// on one definition of a device. `lstat` would give the volume group's
/// shared `st_dev`, which matches the Data volume's only by coincidence.
fn data_volume() -> Option<u64> {
    static DATA: OnceLock<Option<u64>> = OnceLock::new();
    *DATA.get_or_init(|| {
        let opts = WalkOptions::default();
        firmlinks().iter().find_map(|Firmlink { source, .. }| {
            let name = NameRef::from(source.file_name()?);
            Some(stat::entry_of_path(source, name, &opts).ok()?.dev)
        })
    })
}

/// Turns off dataless-file materialisation for the process, so reading an
/// iCloud placeholder's attributes never downloads it. A failure leaves the
/// default policy, under which it can; it is logged.
pub(super) fn keep_dataless_files() {
    if let Err(e) = ffi::disable_dataless_materialization() {
        log::warn!("cannot turn off dataless-file materialisation: {e}");
    }
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
        self.read_dir_streaming(dir, opts, out, errs, later, &mut |_, _, _| {})
    }

    /// Reads the directory in bulk, turning each record into an entry as its
    /// refill is decoded and flushing them every [`STREAM_CHUNK`].
    fn read_dir_streaming(
        &self,
        dir: &DirHandle,
        opts: &WalkOptions,
        out: &mut Vec<Entry>,
        errs: &mut Vec<ScanError>,
        later: &mut Vec<NameRef>,
        flush: &mut Flush<'_>,
    ) -> io::Result<()> {
        let fd = posix::open_handle(dir)?;
        let (out_start, errs_start) = (out.len(), errs.len());
        // The device of the first entry that is no boundary, for the clone
        // check: the entries may have been flushed by the time it runs.
        let mut clone_dev = None;
        #[cfg(test)]
        let (pretend, clock): (_, &dyn Clock) = (
            self.pretend,
            if self.waiting {
                &tests::Waiting
            } else {
                &bulk::Clocks
            },
        );
        #[cfg(not(test))]
        let (pretend, clock): (_, &dyn Clock) = (None, &bulk::Clocks);
        let outcome = if let Some(outcome) = pretend {
            outcome
        } else {
            // The probe only pays off when other workers can share the
            // stat queue.
            let probe = (opts.threads != 1).then_some(clock);
            SINGLE.with_borrow_mut(|single| {
                BULK.with_borrow_mut(|buf| {
                    bulk::read(fd.as_fd(), opts, buf, probe, &mut |item, settled| {
                        take(dir, fd.as_fd(), opts, single, item, out, errs);
                        if clone_dev.is_none()
                            && let Some(entry) = out.last().filter(|e| e.boundary == Boundary::None)
                        {
                            clone_dev = Some(entry.dev);
                        }
                        // Not before the probe can no longer switch: a switch
                        // keeps what was decoded, which must still be here to
                        // be told from the names left to read.
                        if settled && out.len() + errs.len() >= STREAM_CHUNK {
                            flush(out, errs, later);
                        }
                    })
                })
            })?
        };
        match outcome {
            Outcome::Done => {}
            // The fallback reads a filesystem without bulk reads, through
            // the descriptor already open, and reads no clone ids.
            Outcome::Unsupported => {
                if opts.clones {
                    self.clones_unavailable.store(true, Ordering::Relaxed);
                }
                FallbackProvider::default().read_open(fd.as_fd(), dir, opts, out, errs, later)?;
            }
            // The probed refills' entries are kept; the rest of the
            // directory is listed by name, handed to the walk as it is
            // listed, and every worker reads it from the stat queue.
            Outcome::IoBound => {
                #[cfg(test)]
                self.switched
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                // A record without a name is listed again below.
                let tail = errs.split_off(errs_start);
                errs.extend(tail.into_iter().filter(|e| !e.name.as_bytes().is_empty()));
                let seen: HashSet<NameRef> = out[out_start..]
                    .iter()
                    .map(|e| e.name.clone())
                    .chain(errs[errs_start..].iter().map(|e| e.name.clone()))
                    .collect();
                posix::each_name(fd.as_fd(), &mut |name| {
                    if !seen.contains(&name) {
                        later.push(name);
                        if later.len() >= STREAM_CHUNK {
                            flush(out, errs, later);
                        }
                    }
                })?;
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
            }
        }
        if opts.clones
            && outcome != Outcome::Unsupported
            && let Some(dev) = clone_dev
        {
            self.check_clones(fd.as_fd(), dev);
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
        let fd = posix::reopen(dir)?;
        // One answer per batch: a filesystem that refuses attribute reads
        // refuses them for every name.
        let mut attrs = true;
        SINGLE.with_borrow_mut(|buf| {
            for name in names {
                match stat::entry_at(fd.as_fd(), name, opts, buf, &mut attrs) {
                    Ok(entry) => out.push(entry),
                    Err(e) => errs.push(scan_error(dir, name.clone(), &e)),
                }
            }
        });
        Ok(())
    }

    /// The Data volume is reached two ways: through its own mount point,
    /// `/System/Volumes/Data`, and through the firmlinks (`/Users`,
    /// `/Applications`, `/System/Library/Caches` ...). Whichever way the
    /// scan does not need is listed but not entered, so nothing is counted
    /// twice. A root that holds every firmlink (a scan of `/`) keeps the
    /// firmlinks and skips the mount point; a root that holds the mount
    /// point but not every firmlink (a scan of `/System`) enters the mount
    /// point and skips the firmlinks below the root. A scan rooted inside
    /// the Data volume is ordinary. Under `-x` the mount point stays out
    /// (another volume), so the firmlinks are the way in and are kept.
    fn skip(&self, dir: &DirHandle, entry: &Entry, opts: &WalkOptions) -> Option<Skip> {
        if Some(entry.dev) != data_volume()
            || !matches!(entry.boundary, Boundary::Mount | Boundary::Firmlink)
        {
            return None;
        }
        // The walk root's own path, resolved: below a followed link the
        // directory's path is the link's target, not under the root.
        let root = self.real_root(&dir.ancestors().last()?.path())?;
        left_out(
            entry.boundary,
            &firmlink_sources(),
            &root,
            opts.one_file_system,
        )
        .then_some(Skip::Firmlinked)
    }

    fn clones_unavailable(&self, _opts: &WalkOptions) -> bool {
        self.clones_unavailable.load(Ordering::Relaxed)
    }

    fn volume(&self, path: &Path) -> io::Result<VolumeInfo> {
        volume::of(path)
    }

    fn mount_info(&self, path: &Path, dev: u64) -> io::Result<VolumeInfo> {
        volume::mount_info(path, dev)
    }

    fn root(&self, path: &Path, opts: &WalkOptions) -> io::Result<Entry> {
        let name = NameRef::from(path.file_name().unwrap_or(path.as_os_str()));
        stat::entry_of_path(path, name, opts)
    }

    /// The thread sweep's count (P1-BENCH-01), from bench-macos's
    /// `bench/results/thread-sweep-macos-arm64.json` by the rule in
    /// `bench/sweep.py`: the smallest count whose warm T1 p50 is within 5% of
    /// the best (20, against 24's 1.29 s), rounded to the nearest of 4, 8, 16
    /// and 32. It replaces D40's provisional 8.
    fn default_threads(&self) -> usize {
        16
    }
}

/// Files one decoded bulk record under `dir`: an entry in `out`, anything
/// that could not be read in `errs`. A record that must be read again is
/// read by name, through the directory's descriptor.
fn take(
    dir: &DirHandle,
    fd: BorrowedFd<'_>,
    opts: &WalkOptions,
    single: &mut AttrBuf,
    item: Decoded,
    out: &mut Vec<Entry>,
    errs: &mut Vec<ScanError>,
) {
    match item {
        Decoded::Entry(entry) => out.push(entry),
        Decoded::Error(name, errno) => errs.push(ScanError {
            dir: dir.id(),
            name,
            errno,
        }),
        Decoded::Restat(name, boundary) => match stat::entry_at(fd, &name, opts, single, &mut true)
        {
            Ok(mut entry) => {
                if let Some(boundary) = boundary {
                    entry.boundary = boundary;
                }
                out.push(entry);
            }
            Err(e) => errs.push(scan_error(dir, name, &e)),
        },
        // Nothing names the entry, so it is counted as an error with an
        // empty name rather than dropped from the totals unseen.
        Decoded::Nameless => errs.push(ScanError {
            dir: dir.id(),
            name: NameRef::new(b""),
            errno: crate::EIO,
        }),
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
