//! Mounts: `/proc/self/mountinfo`, the platform matrix's class of each
//! filesystem type, and the volume holding a path.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use super::statx;
use crate::platform::posix;
use crate::{Entry, NameRef, Normalization, VolumeClass, VolumeInfo};

/// One line of `/proc/self/mountinfo`.
#[derive(Debug, PartialEq, Eq)]
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
        "cifs" | "smb3" | "smbfs" | "9p" | "afp" | "afpfs" | "autofs" => VolumeClass::Network,
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

/// The mount holding `path`: by the mount id `statx` reports (5.8), else by
/// the longest mount point its real path starts with.
fn mount_of(path: &Path, mnt_id: Option<u64>) -> io::Result<Mount> {
    let mounts = parse_mountinfo(&fs::read("/proc/self/mountinfo")?);
    let found = if let Some(id) = mnt_id {
        mounts.into_iter().find(|m| m.id == id)
    } else {
        let real = fs::canonicalize(path)?;
        mounts
            .into_iter()
            .filter(|m| real.starts_with(&m.path))
            .max_by_key(|m| m.path.as_os_str().len())
    };
    found.ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no mount holds the path"))
}

/// The mount id of the mount holding `path`, for kernels whose `statx`
/// reports none (before 5.8).
pub(super) fn mnt_id_of(path: &Path) -> u64 {
    mount_of(path, None).map_or(0, |m| m.id)
}

/// The volume holding `path`; sizes from `statvfs`, free bytes being
/// `f_bavail × f_frsize`. `mnt_id` is the reusable id that mountinfo
/// lists; the walk puts the entries' id in its place.
pub(in crate::platform) fn volume(path: &Path) -> io::Result<VolumeInfo> {
    let c_path = posix::c_path(path)?;
    let stx = statx::statx(None, &c_path, statx::BASIC & !libc::STATX_MNT_ID_UNIQUE)?;
    let entry: Entry = statx::entry(NameRef::new(b""), &stx, crate::Attrs::Basic);
    let mnt_id = (stx.stx_mask & libc::STATX_MNT_ID != 0).then_some(stx.stx_mnt_id);
    let mount = mount_of(path, mnt_id)?;
    let vfs = posix::statvfs(&c_path)?;
    // The fields are 32-bit on 32-bit targets.
    #[allow(clippy::useless_conversion)]
    let (frsize, blocks, avail) = (
        u64::from(vfs.f_frsize),
        u64::from(vfs.f_blocks),
        u64::from(vfs.f_bavail),
    );
    let fs_type = mount.fs_type.to_lowercase();
    Ok(VolumeInfo {
        dev: entry.dev,
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
        total_bytes: blocks.saturating_mul(frsize),
        free_bytes: avail.saturating_mul(frsize),
        entered: true,
        skip: None,
    })
}
