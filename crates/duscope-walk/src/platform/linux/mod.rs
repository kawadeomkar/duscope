//! The Linux provider: `getdents64(2)` listings and one dirfd-relative
//! `statx(2)` per entry, with a wide directory's entries spread over every
//! worker in batches of 1,024.

mod dents;
mod statx;

pub(super) use statx::from_stat;
#[cfg(test)]
mod tests;

use std::cell::RefCell;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::os::fd::AsFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
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
                Ok(entry) => out.push(entry),
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

    fn volume(&self, path: &Path) -> io::Result<VolumeInfo> {
        volume(path)
    }

    fn root(&self, path: &Path, opts: &WalkOptions) -> io::Result<Entry> {
        let name = NameRef::from(path.file_name().unwrap_or(path.as_os_str()));
        #[cfg(test)]
        self.statx_calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let stx = statx::statx(None, &posix::c_path(path)?, statx::mask(opts))?;
        Ok(statx::entry(name, &stx))
    }
}

/// The volume holding `path`: its mount from `/proc/self/mountinfo` by the
/// mount id `statx` reports (5.8), else by the longest mount point the
/// path's real path starts with; sizes from `statvfs`, free bytes being
/// `f_bavail × f_frsize`.
pub(super) fn volume(path: &Path) -> io::Result<VolumeInfo> {
    let c_path = posix::c_path(path)?;
    let stx = statx::statx(None, &c_path, statx::BASIC)?;
    let entry = statx::entry(NameRef::new(b""), &stx);
    let mountinfo = fs::read("/proc/self/mountinfo")?;
    let mounts = parse_mountinfo(&mountinfo);
    let mount = if entry.mnt_id == 0 {
        let real = fs::canonicalize(path)?;
        mounts
            .iter()
            .filter(|m| real.starts_with(&m.path))
            .max_by_key(|m| m.path.as_os_str().len())
    } else {
        mounts.iter().find(|m| m.id == entry.mnt_id)
    };
    let mount =
        mount.ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no mount holds the path"))?;
    let vfs = posix::statvfs(&c_path)?;
    // The fields are 32-bit on 32-bit targets.
    #[allow(clippy::useless_conversion)]
    let (frsize, blocks, avail) = (
        u64::from(vfs.f_frsize),
        u64::from(vfs.f_blocks),
        u64::from(vfs.f_bavail),
    );
    Ok(VolumeInfo {
        dev: entry.dev,
        mnt_id: mount.id,
        mount_point: mount.path.clone(),
        fs_type: mount.fs_type.to_lowercase(),
        total_bytes: blocks.saturating_mul(frsize),
        free_bytes: avail.saturating_mul(frsize),
    })
}

/// One line of `/proc/self/mountinfo`.
#[derive(Debug, PartialEq, Eq)]
struct Mount {
    id: u64,
    /// Where it is mounted.
    path: PathBuf,
    fs_type: String,
}

/// The mounts in `/proc/self/mountinfo`: `id parent major:minor root
/// mount-point options [optional fields...] - fs-type source super-options`,
/// with spaces, tabs, newlines and backslashes in paths escaped as `\ooo`.
fn parse_mountinfo(text: &[u8]) -> Vec<Mount> {
    text.split(|&b| b == b'\n')
        .filter_map(|line| {
            let fields: Vec<&[u8]> = line.split(|&b| b == b' ').collect();
            let id = std::str::from_utf8(fields.first()?).ok()?.parse().ok()?;
            let path = PathBuf::from(OsStr::from_bytes(&unescape(fields.get(4)?)));
            let dash = fields.iter().position(|&f| f == b"-")?;
            let fs_type = String::from_utf8_lossy(fields.get(dash + 1)?).into_owned();
            Some(Mount { id, path, fs_type })
        })
        .collect()
}

/// A mountinfo path with its `\ooo` escapes decoded.
fn unescape(field: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(field.len());
    let mut i = 0;
    while i < field.len() {
        let octal = field.get(i + 1..i + 4).and_then(|digits| {
            let digits = std::str::from_utf8(digits).ok()?;
            u8::from_str_radix(digits, 8).ok()
        });
        match (field[i], octal) {
            (b'\\', Some(byte)) => {
                out.push(byte);
                i += 4;
            }
            (byte, _) => {
                out.push(byte);
                i += 1;
            }
        }
    }
    out
}
