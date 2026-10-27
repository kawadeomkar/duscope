//! Mounts: `/proc/self/mountinfo`, the platform matrix's class of each
//! filesystem type, and the volume holding a path.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use super::statx;
use crate::platform::posix;
use crate::{DirHandle, Entry, Kind, NameRef, Normalization, VolumeClass, VolumeInfo};

/// One line of `/proc/self/mountinfo`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Mount {
    /// The mount id (the reusable one, `STATX_MNT_ID`).
    pub(super) id: u64,
    /// Where it is mounted.
    pub(super) path: PathBuf,
    pub(super) fs_type: String,
    /// The per-mount options, then the superblock's.
    pub(super) options: Vec<String>,
}

/// The mounts in a `/proc/self/mountinfo` text: `id parent major:minor root
/// mount-point options [optional fields...] - fs-type source super-options`,
/// with spaces, tabs, newlines and backslashes in paths escaped as `\ooo`.
pub(super) fn parse_mountinfo(text: &[u8]) -> Vec<Mount> {
    text.split(|&b| b == b'\n')
        .filter_map(|line| {
            let fields: Vec<&[u8]> = line.split(|&b| b == b' ').collect();
            let id = std::str::from_utf8(fields.first()?).ok()?.parse().ok()?;
            let path = PathBuf::from(OsStr::from_bytes(&unescape(fields.get(4)?)));
            let dash = fields.iter().position(|&f| f == b"-")?;
            let fs_type = String::from_utf8_lossy(fields.get(dash + 1)?).into_owned();
            let mut options = split_options(fields.get(5)?);
            options.extend(
                fields
                    .get(dash + 3)
                    .map(|o| split_options(o))
                    .unwrap_or_default(),
            );
            Some(Mount {
                id,
                path,
                fs_type,
                options,
            })
        })
        .collect()
}

fn split_options(field: &[u8]) -> Vec<String> {
    field
        .split(|&b| b == b',')
        .filter(|o| !o.is_empty())
        .map(|o| String::from_utf8_lossy(o).into_owned())
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

/// The platform matrix's class of a Linux filesystem type. The kernel
/// list is ncdu's `--exclude-kernfs` set, by type rather than path.
pub(super) fn class(fs_type: &str) -> VolumeClass {
    match fs_type {
        "binfmt_misc" | "bpf" | "cgroup" | "cgroup2" | "debugfs" | "devpts" | "proc" | "pstore"
        | "securityfs" | "selinuxfs" | "sysfs" | "tracefs" => VolumeClass::Kernel,
        // FUSE on a block device (ntfs-3g, exfat-fuse) is a local disk.
        "fuseblk" => VolumeClass::Foreign,
        "cifs" | "smb3" | "smbfs" | "9p" | "virtiofs" | "afp" | "afpfs" | "autofs" | "ceph"
        | "lustre" | "gpfs" | "afs" | "openafs" | "beegfs" | "pvfs2" => VolumeClass::Network,
        t if t.starts_with("nfs") || t.starts_with("fuse") => VolumeClass::Network,
        "tmpfs" | "ramfs" | "devtmpfs" => VolumeClass::Memory,
        "overlay" => VolumeClass::Overlay,
        "exfat" | "vfat" | "msdos" | "fat" | "ntfs" | "ntfs3" | "hfs" | "hfsplus" => {
            VolumeClass::Foreign
        }
        "squashfs" | "iso9660" | "erofs" | "cramfs" | "udf" => VolumeClass::ReadOnlyImage,
        _ => VolumeClass::Local,
    }
}

/// Whether names on a filesystem differ by case alone: not on exFAT and
/// FAT, not on NTFS mounted with `nocase`; unknown for HFS+, whose HFSX
/// variant is case-sensitive; yes elsewhere.
pub(super) fn case_sensitive(fs_type: &str, options: &[String]) -> Option<bool> {
    match fs_type {
        "exfat" | "vfat" | "msdos" | "fat" => Some(false),
        "ntfs" | "ntfs3" => Some(!options.iter().any(|o| o == "nocase")),
        "hfs" | "hfsplus" => None,
        _ => Some(true),
    }
}

/// The mount table one provider reads from `/proc/self/mountinfo`, for one
/// walk: read when first needed, and read again when a lookup misses or
/// finds a mount at another place than asked, since a freed mount id is
/// handed to the next mount at once.
#[derive(Debug, Default)]
pub(in crate::platform) struct Mounts {
    table: Mutex<Vec<Mount>>,
    /// The walk root as given and its real path, for matching paths
    /// against mount points.
    root: Mutex<Option<(PathBuf, PathBuf)>>,
}

impl Mounts {
    /// The mount holding `path`: by `mnt_id` (the reusable id, as
    /// `STATX_MNT_ID` gives it) when there is one, else by the longest
    /// mount point the real path `real` starts with. With `at_point`, the
    /// mount must be mounted at `real`.
    fn lookup(&self, real: &Path, mnt_id: Option<u64>, at_point: bool) -> io::Result<Mount> {
        let find = |table: &[Mount]| -> Option<Mount> {
            let found = match mnt_id {
                Some(id) => table.iter().find(|m| m.id == id),
                None => table
                    .iter()
                    .filter(|m| real.starts_with(&m.path))
                    .max_by_key(|m| m.path.as_os_str().len()),
            };
            found.filter(|m| !at_point || m.path == real).cloned()
        };
        let mut table = self.table.lock().unwrap_or_else(PoisonError::into_inner);
        if !table.is_empty()
            && let Some(found) = find(&table)
        {
            return Ok(found);
        }
        *table = parse_mountinfo(&fs::read("/proc/self/mountinfo")?);
        find(&table)
            .or_else(|| {
                (at_point && mnt_id.is_some())
                    .then(|| table.iter().find(|m| Some(m.id) == mnt_id).cloned())
                    .flatten()
            })
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no mount holds the path"))
    }

    /// The real path of `name` in `dir`: the walk root's real path, read
    /// once, joined with the names below it. A directory reached through a
    /// link followed under `--follow-symlinks` has its resolved path already.
    fn real(&self, dir: &DirHandle, name: &[u8]) -> io::Result<PathBuf> {
        let given = dir.ancestors().last().map_or(dir.path(), DirHandle::path);
        let Ok(below) = dir.path().strip_prefix(given) else {
            return Ok(dir.path().join(OsStr::from_bytes(name)));
        };
        let mut root = self.root.lock().unwrap_or_else(PoisonError::into_inner);
        let real_root = match &*root {
            Some((cached, real)) if cached == given => real.clone(),
            _ => {
                let real = fs::canonicalize(given)?;
                *root = Some((given.to_owned(), real.clone()));
                real
            }
        };
        Ok(real_root.join(below).join(OsStr::from_bytes(name)))
    }

    /// The mount id of the mount holding `path`, by path, for kernels whose
    /// `statx` reports none (before 5.8) and for the fallback provider; 0
    /// when it cannot be found.
    pub(in crate::platform) fn mnt_id_of(&self, path: &Path) -> u64 {
        fs::canonicalize(path)
            .and_then(|real| self.lookup(&real, None, false))
            .map_or(0, |m| m.id)
    }

    /// Gives an entry whose mount id `statx` did not report (before Linux
    /// 5.8, or read by `stat`) its mount's id. A directory is looked up by
    /// path, so a bind mount on the same device is still a mount of its
    /// own; any other entry on its directory's device takes its
    /// directory's.
    pub(in crate::platform) fn fill_mnt_id(&self, entry: &mut Entry, dir: &DirHandle) {
        if entry.mnt_id != 0 {
            return;
        }
        entry.mnt_id = if entry.kind != Kind::Dir && entry.dev == dir.dev() {
            dir.mnt_id()
        } else {
            self.real(dir, entry.name.as_bytes())
                .and_then(|real| self.lookup(&real, None, false))
                .map_or(dir.mnt_id(), |m| m.id)
        };
    }

    /// The volume mounted at `path` from its mountinfo line alone, sizes 0:
    /// the mounted filesystem is not asked anything (`statx` with
    /// `AT_NO_AUTOMOUNT` and `AT_STATX_DONT_SYNC` reads only what the
    /// kernel holds).
    pub(in crate::platform) fn mount_info(&self, path: &Path) -> io::Result<VolumeInfo> {
        let c_path = posix::c_path(path)?;
        let stx = statx::statx(None, &c_path, libc::STATX_MNT_ID | libc::STATX_INO)?;
        let mnt_id = (stx.stx_mask & libc::STATX_MNT_ID != 0).then_some(stx.stx_mnt_id);
        let mount = self.lookup(path, mnt_id, true)?;
        Ok(describe(
            libc::makedev(stx.stx_dev_major, stx.stx_dev_minor),
            mount,
            0,
            0,
        ))
    }

    /// The volume holding `path`; sizes from `statvfs`, free bytes being
    /// `f_bavail × f_frsize`. The walk enters it, so an automount there is
    /// triggered and the mounted volume described. `mnt_id` is the reusable
    /// id mountinfo lists; the walk puts the entries' id in its place.
    pub(in crate::platform) fn volume(&self, path: &Path) -> io::Result<VolumeInfo> {
        let c_path = posix::c_path(path)?;
        let stx = statx::statx_through(&c_path, statx::BASIC & !libc::STATX_MNT_ID_UNIQUE)?;
        let entry: Entry = statx::entry(NameRef::new(b""), &stx, crate::Attrs::Basic);
        let mnt_id = (stx.stx_mask & libc::STATX_MNT_ID != 0).then_some(stx.stx_mnt_id);
        let real = fs::canonicalize(path)?;
        let mount = self.lookup(&real, mnt_id, false)?;
        let vfs = posix::statvfs(&c_path)?;
        // The fields are 32-bit on 32-bit targets.
        #[allow(clippy::useless_conversion)]
        let (frsize, blocks, avail) = (
            u64::from(vfs.f_frsize),
            u64::from(vfs.f_blocks),
            u64::from(vfs.f_bavail),
        );
        Ok(describe(
            entry.dev,
            mount,
            blocks.saturating_mul(frsize),
            avail.saturating_mul(frsize),
        ))
    }
}

/// A volume record from a mount and its sizes.
fn describe(dev: u64, mount: Mount, total_bytes: u64, free_bytes: u64) -> VolumeInfo {
    let fs_type = mount.fs_type.to_lowercase();
    VolumeInfo {
        dev,
        mnt_id: mount.id,
        mount_point: mount.path,
        class: class(&fs_type),
        read_only: mount.options.iter().any(|o| o == "ro"),
        case_sensitive: case_sensitive(&fs_type, &mount.options),
        normalization: if fs_type == "hfsplus" || fs_type == "hfs" {
            Normalization::Insensitive
        } else {
            Normalization::None
        },
        options: mount.options,
        fs_type,
        total_bytes,
        free_bytes,
        reason: None,
    }
}
