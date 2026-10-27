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
pub(super) use volume::of as volume;

use std::cell::RefCell;
use std::collections::HashSet;
use std::io;
use std::os::fd::{AsFd, BorrowedFd};
use std::path::Path;

use bulk::{Clock, Decoded, Outcome};
use ffi::AttrBuf;

use crate::platform::fallback::FallbackProvider;
use crate::platform::posix;
use crate::{DirHandle, Entry, NameRef, Provider, ScanError, VolumeInfo, WalkOptions};

thread_local! {
    /// Each worker's bulk buffer, reused for every directory it reads.
    static BULK: RefCell<AttrBuf> = RefCell::new(AttrBuf::new(bulk::BUFFER_BYTES));
    /// Each worker's buffer for single-entry reads.
    static SINGLE: RefCell<AttrBuf> = RefCell::new(AttrBuf::new(stat::BUFFER_BYTES));
}

/// The macOS provider.
#[derive(Debug, Default)]
pub(crate) struct MacProvider {
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
        let fd = posix::open_dir(dir.path())?;
        let (out_start, errs_start) = (out.len(), errs.len());
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
                    bulk::read(fd.as_fd(), opts, buf, probe, &mut |item| {
                        take(dir, fd.as_fd(), opts, single, item, out, errs);
                    })
                })
            })?
        };
        match outcome {
            Outcome::Done => {}
            // The fallback provider reads a filesystem without bulk reads.
            Outcome::Unsupported => {
                return FallbackProvider.read_dir_split(dir, opts, out, errs, later);
            }
            // The probed refills' entries are kept; the rest of the
            // directory is listed by name and every worker reads it from
            // the stat queue.
            Outcome::IoBound => {
                #[cfg(test)]
                self.switched
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                // A record without a name is listed again below.
                let tail = errs.split_off(errs_start);
                errs.extend(tail.into_iter().filter(|e| !e.name.as_bytes().is_empty()));
                let seen: HashSet<&[u8]> = out[out_start..]
                    .iter()
                    .map(|e| e.name.as_bytes())
                    .chain(errs[errs_start..].iter().map(|e| e.name.as_bytes()))
                    .collect();
                let names = posix::list_names(fd.as_fd())?;
                later.extend(
                    names
                        .into_iter()
                        .filter(|name| !seen.contains(name.as_bytes())),
                );
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

    fn volume(&self, path: &Path) -> io::Result<VolumeInfo> {
        volume::of(path)
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
        Decoded::Restat(name, boundary) => match stat::entry_at(fd, &name, opts, single) {
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
