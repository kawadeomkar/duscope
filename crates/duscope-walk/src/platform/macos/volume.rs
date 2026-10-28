//! The volume holding a path, from `statfs(2)`.

use std::ffi::OsStr;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use super::bulk;
use super::ffi;
use crate::VolumeInfo;
use crate::platform::posix;

/// The volume holding `path`. Free bytes are `f_bavail × f_bsize`, what an
/// unprivileged user can still write.
pub(in crate::platform) fn of(path: &Path) -> io::Result<VolumeInfo> {
    let c_path = posix::c_path(path)?;
    let fs = ffi::statfs(&c_path)?;
    let dev = bulk::dev(posix::lstat(&c_path)?.st_dev);
    let block = u64::from(fs.f_bsize);
    Ok(VolumeInfo {
        dev,
        mnt_id: dev,
        mount_point: PathBuf::from(OsStr::from_bytes(&c_chars(&fs.f_mntonname))),
        fs_type: String::from_utf8_lossy(&c_chars(&fs.f_fstypename)).to_lowercase(),
        total_bytes: fs.f_blocks.saturating_mul(block),
        free_bytes: fs.f_bavail.saturating_mul(block),
    })
}

/// The bytes of a NUL-terminated `char` array, without the NUL.
fn c_chars(chars: &[libc::c_char]) -> Vec<u8> {
    chars
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c.cast_unsigned())
        .collect()
}
