//! The volume holding a path, from `statfs(2)` and `pathconf(2)`, and the
//! platform matrix's class of its filesystem type.

use std::ffi::OsStr;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use super::bulk;
use super::ffi;
use crate::platform::posix;
use crate::{Normalization, VolumeClass, VolumeInfo};

/// The volume holding `path`. Free bytes are `f_bavail × f_bsize`, what an
/// unprivileged user can still write.
pub(in crate::platform) fn of(path: &Path) -> io::Result<VolumeInfo> {
    let c_path = posix::c_path(path)?;
    let fs = ffi::statfs(&c_path)?;
    let dev = bulk::dev(posix::lstat(&c_path)?.st_dev);
    let block = u64::from(fs.f_bsize);
    let fs_type = String::from_utf8_lossy(&c_chars(&fs.f_fstypename)).to_lowercase();
    let mount_point = PathBuf::from(OsStr::from_bytes(&c_chars(&fs.f_mntonname)));
    let case_sensitive = posix::c_path(&mount_point)
        .ok()
        .and_then(|c| ffi::pathconf(&c, libc::_PC_CASE_SENSITIVE))
        .map(|value| value != 0);
    Ok(VolumeInfo {
        dev,
        mnt_id: dev,
        mount_point,
        class: class(&fs_type),
        read_only: fs.f_flags & flag(libc::MNT_RDONLY) != 0,
        case_sensitive,
        normalization: if fs_type == "apfs" || fs_type == "hfs" {
            Normalization::Insensitive
        } else {
            Normalization::None
        },
        options: options(fs.f_flags),
        fs_type,
        total_bytes: fs.f_blocks.saturating_mul(block),
        free_bytes: fs.f_bavail.saturating_mul(block),
        entered: true,
        skip: None,
    })
}

/// The platform matrix's class of a macOS filesystem type.
pub(super) fn class(fs_type: &str) -> VolumeClass {
    match fs_type {
        "devfs" => VolumeClass::Kernel,
        "autofs" | "nfs" | "smbfs" | "afpfs" | "webdav" | "ftp" | "cifs" => VolumeClass::Network,
        t if t.starts_with("fuse") || t.starts_with("macfuse") || t.starts_with("osxfuse") => {
            VolumeClass::Network
        }
        "msdos" | "exfat" | "hfs" | "ntfs" | "lifs" => VolumeClass::Foreign,
        "cd9660" | "udf" => VolumeClass::ReadOnlyImage,
        _ => VolumeClass::Local,
    }
}

/// A `MNT_*` constant as `f_flags`' type.
fn flag(value: libc::c_int) -> u32 {
    value.cast_unsigned()
}

/// The mount options `f_flags` holds, in `mount(8)`'s words.
fn options(flags: u32) -> Vec<String> {
    let mut options = vec![if flags & flag(libc::MNT_RDONLY) != 0 {
        "ro"
    } else {
        "rw"
    }];
    for (bit, name) in [
        (libc::MNT_NOATIME, "noatime"),
        (libc::MNT_NOSUID, "nosuid"),
        (libc::MNT_NODEV, "nodev"),
        (libc::MNT_NOEXEC, "noexec"),
        (libc::MNT_LOCAL, "local"),
    ] {
        if flags & flag(bit) != 0 {
            options.push(name);
        }
    }
    options.into_iter().map(str::to_owned).collect()
}

/// The bytes of a NUL-terminated `char` array, without the NUL.
fn c_chars(chars: &[libc::c_char]) -> Vec<u8> {
    chars
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c.cast_unsigned())
        .collect()
}
