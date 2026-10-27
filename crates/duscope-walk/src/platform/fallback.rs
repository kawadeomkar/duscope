//! The fallback provider: `readdir` and one `fstatat(AT_SYMLINK_NOFOLLOW)`
//! per entry, on both platforms. It fills the same `Entry` fields as the
//! platform providers, less what only their calls report: the mount id and
//! the attribute flags on Linux, the clone ids on macOS. macOS reads a
//! filesystem that refuses bulk reads through it.
//!
//! On macOS it reads no clone ids or extended flags, which only
//! `getattrlist` returns: the filesystems that refuse bulk reads (SMB,
//! FUSE) have none.

use std::ffi::CString;
use std::fs;
use std::io;
use std::os::fd::{AsFd, BorrowedFd};
use std::path::Path;

use crate::platform::posix;
use crate::{DirHandle, Entry, NameRef, Provider, STAT_BATCH, ScanError, VolumeInfo, WalkOptions};

#[cfg(target_os = "linux")]
use super::linux::{from_stat, volume};
#[cfg(target_os = "macos")]
use super::macos::{from_stat, volume};

/// The fallback provider.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct FallbackProvider;

impl Provider for FallbackProvider {
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
    /// the rest for stat batches on any worker, as the Linux provider does.
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
        for entry in fs::read_dir(dir.path())? {
            names.push(NameRef::from(entry?.file_name().as_os_str()));
        }
        if names.len() > STAT_BATCH {
            later.extend(names.drain(STAT_BATCH..));
        }
        read_names(fd.as_fd(), dir, &names, opts, out, errs);
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
        read_names(fd.as_fd(), dir, names, opts, out, errs);
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
        Ok(from_stat(
            name,
            &posix::lstat(&posix::c_path(path)?)?,
            opts.attrs,
        ))
    }

    /// The platform provider's default, so the two compare like for like.
    fn default_threads(&self) -> usize {
        if cfg!(target_os = "macos") {
            8
        } else {
            let cpus = std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get);
            cpus.saturating_mul(2).clamp(4, 32)
        }
    }
}

/// Reads `names` in the directory open on `fd`: an entry or an error each.
fn read_names(
    fd: BorrowedFd<'_>,
    dir: &DirHandle,
    names: &[NameRef],
    opts: &WalkOptions,
    out: &mut Vec<Entry>,
    errs: &mut Vec<ScanError>,
) {
    for name in names {
        let stat = CString::new(name.as_bytes())
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
            .and_then(|c_name| posix::lstat_at(fd, &c_name));
        match stat {
            Ok(stat) => {
                #[allow(unused_mut)] // only Linux fills in the mount id
                let mut entry = from_stat(name.clone(), &stat, opts.attrs);
                #[cfg(target_os = "linux")]
                super::linux::fill_mnt_id(&mut entry, dir);
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

#[cfg(test)]
mod tests {
    //! P1-WALK-04: the platform provider and the fallback provider read the
    //! fixture tree the same.

    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    use duscope_testkit::TestEnv;

    use super::FallbackProvider;
    use crate::{
        Boundary, DirId, Entry, Flags, Kind, Provider, ScanEvent, Share, WalkOptions, walk,
        walk_with,
    };

    /// What a walk saw: entries by path, scan errors by path, and the
    /// directories that closed with an error.
    #[derive(Debug, PartialEq, Eq)]
    struct Seen {
        entries: BTreeMap<PathBuf, Entry>,
        errors: BTreeMap<PathBuf, i32>,
        unreadable: BTreeMap<PathBuf, i32>,
    }

    /// The walk of `root`, with each entry reduced to what both providers
    /// report: the directory ids (scan order), and the fields only the
    /// platform's own calls fill.
    fn seen(provider: &dyn Provider, root: &Path, opts: &WalkOptions) -> Seen {
        let mut dirs: BTreeMap<DirId, PathBuf> = BTreeMap::new();
        let mut seen = Seen {
            entries: BTreeMap::new(),
            errors: BTreeMap::new(),
            unreadable: BTreeMap::new(),
        };
        walk_with(provider, root, opts, |event| match event {
            ScanEvent::Entry(entry) => {
                let path = if entry.dir == Some(DirId::ROOT) {
                    root.to_owned()
                } else {
                    dirs[&entry.parent].join(entry.name.as_os_str())
                };
                if let Some(id) = entry.dir {
                    dirs.insert(id, path.clone());
                }
                seen.entries.insert(path, comparable(entry));
            }
            ScanEvent::Error(error) => {
                seen.errors
                    .insert(dirs[&error.dir].join(error.name.as_os_str()), error.errno);
            }
            ScanEvent::DirClosed {
                dir,
                errno: Some(errno),
            } => {
                seen.unreadable.insert(dirs[&dir].clone(), errno);
            }
            _ => {}
        })
        .unwrap();
        seen
    }

    /// An entry without what differs between the providers by design.
    fn comparable(entry: Entry) -> Entry {
        let mut entry = Entry {
            parent: DirId::ROOT,
            dir: entry.dir.map(|_| DirId::ROOT),
            ..entry
        };
        if cfg!(target_os = "linux") {
            // stat carries neither the mount id nor statx's attributes.
            entry.mnt_id = 0;
            entry.flags.remove(Flags::COMPRESSED | Flags::MOUNT_ROOT);
            if entry.boundary == Boundary::Mount {
                entry.boundary = Boundary::None;
            }
        }
        if cfg!(target_os = "macos") {
            // ATTR_DIR_LINKCOUNT counts a directory's hard links (1 on
            // APFS); st_nlink also counts its subdirectories.
            if entry.kind == Kind::Dir {
                entry.nlink = 0;
            }
            // stat carries neither clone ids nor the extended flags.
            entry
                .flags
                .remove(Flags::SPARSE | Flags::PURGEABLE | Flags::MAY_SHARE | Flags::SHARES_ALL);
            if matches!(entry.share, Share::Clone(_) | Share::MayShare) {
                entry.share = if entry.nlink > 1 {
                    Share::Hardlink
                } else {
                    Share::None
                };
            }
        }
        entry
    }

    #[test]
    fn p1_walk_04_the_providers_read_the_fixture_tree_the_same() {
        let env = TestEnv::new();
        let root = env.fixtures().root();
        let opts = WalkOptions {
            threads: 4,
            ..WalkOptions::default()
        };
        let native = crate::platform::native().unwrap();
        let native = seen(&*native, &root, &opts);
        let fallback = seen(&FallbackProvider, &root, &opts);
        assert!(native.entries.len() > 100_000);
        assert_eq!(native.entries.len(), fallback.entries.len());
        for (path, entry) in &native.entries {
            assert_eq!(Some(entry), fallback.entries.get(path), "{path:?}");
        }
        assert_eq!(native.errors, fallback.errors);
        assert_eq!(native.unreadable, fallback.unreadable);
    }

    #[test]
    fn p1_walk_04_walk_reads_through_the_fallback_when_asked() {
        let env = TestEnv::new();
        let root = env.fixtures().case_root("deep-path");
        let opts = WalkOptions {
            fallback: true,
            ..WalkOptions::default()
        };
        let mut entries = Vec::new();
        walk(&root, &opts, |event| {
            if let ScanEvent::Entry(entry) = event {
                entries.push(entry);
            }
        })
        .unwrap();
        assert!(entries.len() > 200);
        // What only the fallback reports: no mount id on Linux, st_nlink
        // for a directory with a subdirectory on macOS.
        if cfg!(target_os = "linux") {
            assert!(entries.iter().all(|e| e.mnt_id == 0));
        } else {
            assert!(entries.iter().any(|e| e.kind == Kind::Dir && e.nlink >= 3));
        }
    }
}
