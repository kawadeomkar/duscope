//! The volume holding a path, from `statfs(2)` and `pathconf(2)`, and the
//! platform matrix's class of its filesystem type; and what the privacy
//! protections let this process read of the home on it: the Full Disk
//! Access probe, and the app a grant belongs to.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

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
    let mut volume = describe(&fs, dev);
    volume.case_sensitive = posix::c_path(&volume.mount_point)
        .ok()
        .and_then(|c| ffi::pathconf(&c, libc::_PC_CASE_SENSITIVE))
        .map(|value| value != 0);
    Ok(volume)
}

/// The volume mounted at `path` from the kernel's record of its mounts,
/// without asking the volume anything, so its case sensitivity is left
/// unknown; a path that is not a mount point (a firmlink) is read with
/// `statfs`. The walk fills in the device.
pub(in crate::platform) fn mount_info(path: &Path, dev: u64) -> io::Result<VolumeInfo> {
    let mounted = ffi::mounted_volumes()?;
    let target = path.as_os_str().as_bytes();
    // By identity first: the walk may reach a mount point by another path
    // (a root holding `..`, or /System/Volumes/Data/Volumes/X for
    // /Volumes/X), and a statfs of it there could block on a dead server.
    let by_dev = mounted.iter().find(|fs| ffi::fsid_dev(fs) == dev);
    match by_dev.or_else(|| mounted.iter().find(|fs| c_chars(&fs.f_mntonname) == target)) {
        Some(fs) => Ok(describe(fs, 0)),
        None => of(path),
    }
}

/// A volume record from its `statfs`, case sensitivity not read.
fn describe(fs: &libc::statfs, dev: u64) -> VolumeInfo {
    let block = u64::from(fs.f_bsize);
    let fs_type = String::from_utf8_lossy(&c_chars(&fs.f_fstypename)).to_lowercase();
    let mount_point = PathBuf::from(OsStr::from_bytes(&c_chars(&fs.f_mntonname)));
    VolumeInfo {
        dev,
        mnt_id: dev,
        mount_point,
        class: class(&fs_type),
        read_only: fs.f_flags & flag(libc::MNT_RDONLY) != 0,
        case_sensitive: None,
        normalization: if fs_type == "apfs" || fs_type == "hfs" {
            Normalization::Insensitive
        } else {
            Normalization::None
        },
        options: options(fs.f_flags),
        fs_type,
        total_bytes: fs.f_blocks.saturating_mul(block),
        free_bytes: fs.f_bavail.saturating_mul(block),
        reason: None,
    }
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

/// The folders of a home that the privacy protections (TCC) refuse to a
/// process without Full Disk Access, which the probe opens: a heuristic,
/// since Full Disk Access has no API.
const TCC_PROBES: [&str; 2] = ["Library/Mail", "Library/Safari"];

/// Whether this process lacks Full Disk Access: opening each of
/// [`TCC_PROBES`] under `home` failed with `EPERM`, the privacy protections'
/// refusal (a folder that is not there fails with `ENOENT`, one the mode
/// bits close with `EACCES`). Under `denied` (`DUSCOPE_TEST_TCC`) every
/// open is refused as TCC would refuse it, and nothing is opened.
pub(in crate::platform) fn full_disk_access_missing(home: &Path, denied: bool) -> bool {
    refused(TCC_PROBES.map(|rel| {
        if denied {
            Some(libc::EPERM)
        } else {
            fs::read_dir(home.join(rel))
                .err()
                .and_then(|e| e.raw_os_error())
        }
    }))
}

/// Whether the probe's opens, by their error numbers (`None` for one that
/// succeeded), were all refused by the privacy protections.
fn refused(errnos: [Option<i32>; 2]) -> bool {
    errnos.iter().all(|&errno| errno == Some(libc::EPERM))
}

/// How many ancestors [`host_app`] looks at before it gives up: a process
/// tree is never this deep, but a cycle of recycled ids must end.
const MAX_ANCESTORS: usize = 64;

/// The app a Full Disk Access grant for this process belongs to: the
/// outermost app bundle the nearest ancestor that runs from one runs from
/// (a helper inside `Visual Studio Code.app` names `Visual Studio Code`).
/// Parents are read with `PROC_PIDT_SHORTBSDINFO`, which answers for
/// another user's process too (`login`, between a terminal and its shell, is
/// root's), where `PROC_PIDTBSDINFO` refuses.
pub(in crate::platform) fn host_app() -> Option<String> {
    let mut pid = libc::pid_t::try_from(std::os::unix::process::parent_id()).ok()?;
    for _ in 0..MAX_ANCESTORS {
        if pid <= 1 {
            return None;
        }
        if let Some(app) = pid_path(pid).as_deref().and_then(app_name) {
            return Some(app);
        }
        pid = parent_of(pid)?;
    }
    None
}

/// The executable process `pid` runs (`proc_pidpath`).
fn pid_path(pid: libc::pid_t) -> Option<PathBuf> {
    let mut buf = vec![0_u8; libc::PROC_PIDPATHINFO_MAXSIZE.unsigned_abs() as usize];
    let len = u32::try_from(buf.len()).ok()?;
    // SAFETY: the buffer is writable for `len` bytes, which proc_pidpath
    // never writes past; it returns how many it wrote, or 0 or less.
    let n = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), len) };
    let n = usize::try_from(n)
        .ok()
        .filter(|&n| n > 0 && n <= buf.len())?;
    buf.truncate(n);
    Some(PathBuf::from(OsStr::from_bytes(&buf)))
}

/// The parent of process `pid`.
fn parent_of(pid: libc::pid_t) -> Option<libc::pid_t> {
    // SAFETY: proc_bsdshortinfo is plain integers and bytes, for which all
    // zeroes is a valid value.
    let mut info: libc::proc_bsdshortinfo = unsafe { std::mem::zeroed() };
    let size = libc::c_int::try_from(std::mem::size_of::<libc::proc_bsdshortinfo>()).ok()?;
    // SAFETY: `info` is writable for `size` bytes, the size of the record
    // the flavor fills; proc_pidinfo returns how many bytes it wrote.
    let n = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDT_SHORTBSDINFO,
            0,
            (&raw mut info).cast(),
            size,
        )
    };
    (n == size)
        .then(|| libc::pid_t::try_from(info.pbsi_ppid).ok())
        .flatten()
}

/// The outermost app bundle in `path`, without its `.app`.
fn app_name(path: &Path) -> Option<String> {
    path.components().find_map(|c| match c {
        Component::Normal(name) => name
            .to_str()?
            .strip_suffix(".app")
            .filter(|app| !app.is_empty())
            .map(str::to_owned),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{app_name, full_disk_access_missing, host_app, refused};

    #[test]
    fn p5_walk_03_the_probe_needs_both_folders_refused() {
        let (eperm, eacces, enoent) = (Some(libc::EPERM), Some(libc::EACCES), Some(libc::ENOENT));
        assert!(refused([eperm, eperm]));
        // One refused, the other missing (a home that never ran Mail),
        // unreadable by its mode, or opened: no conclusion.
        assert!(!refused([eperm, enoent]));
        assert!(!refused([eacces, eperm]));
        assert!(!refused([None, eperm]));
        assert!(!refused([enoent, enoent]));
    }

    #[test]
    fn p5_walk_03_a_home_without_the_folders_is_not_refused() {
        // A temporary home (never the real one): both folders are missing.
        let env = duscope_testkit::TestEnv::new();
        assert!(!full_disk_access_missing(&env.home(), false));
        assert!(full_disk_access_missing(&env.home(), true));
    }

    #[test]
    fn p5_walk_03_the_host_app_is_the_outermost_bundle() {
        let name = |p: &str| app_name(Path::new(p));
        assert_eq!(
            name("/System/Applications/Utilities/Terminal.app/Contents/MacOS/Terminal").as_deref(),
            Some("Terminal")
        );
        assert_eq!(
            name(
                "/Applications/Visual Studio Code.app/Contents/Frameworks/\
                 Code Helper (Plugin).app/Contents/MacOS/Code Helper (Plugin)"
            )
            .as_deref(),
            Some("Visual Studio Code")
        );
        assert_eq!(name("/opt/homebrew/bin/claude"), None);
        assert_eq!(name("/Applications/.app/x"), None);
        // Whatever runs the tests, the lookup ends, and names a bundle when
        // it names anything.
        if let Some(app) = host_app() {
            assert!(!app.is_empty() && !app.contains('/'), "{app}");
        }
    }
}
