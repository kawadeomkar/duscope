//! `explain --clones` on btrfs and XFS: a file's extent map through the
//! `FS_IOC_FIEMAP` ioctl, and which of its extents the filesystem shares
//! with another file or a snapshot (`FIEMAP_EXTENT_SHARED`: a reflink copy,
//! a deduplicated range, a snapshot's data).
//!
//! Read access only, on demand: the file is opened read-only and never read;
//! the ioctl reads the filesystem's own map of where the file's data lies,
//! not the data, and is asked without `FIEMAP_FLAG_SYNC`, so nothing is
//! written back first. The scan never calls it; `explain` does, for the
//! files under the path it is asked about.

use std::fs::OpenOptions;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use crate::Extents;

/// `FS_IOC_FIEMAP`: `_IOWR('f', 11, struct fiemap)`, whose header is 32
/// bytes; the same number on every Linux architecture duscope builds for.
const FS_IOC_FIEMAP: u32 = 0xC020_660B;
/// The file's last extent.
const FIEMAP_EXTENT_LAST: u32 = 0x1;
/// The extent's blocks are shared with another file or a snapshot.
const FIEMAP_EXTENT_SHARED: u32 = 0x2000;
/// The extents asked for in one call.
const BATCH: u32 = 64;

/// `struct fiemap_extent`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct Extent {
    logical: u64,
    // The kernel's layout: written by the ioctl, not read here.
    #[allow(dead_code)]
    physical: u64,
    length: u64,
    #[allow(dead_code)]
    reserved64: [u64; 2],
    flags: u32,
    #[allow(dead_code)]
    reserved: [u32; 3],
}

const EMPTY: Extent = Extent {
    logical: 0,
    physical: 0,
    length: 0,
    reserved64: [0; 2],
    flags: 0,
    reserved: [0; 3],
};

/// `struct fiemap`, with room for [`BATCH`] extents.
#[repr(C)]
#[derive(Debug)]
struct Map {
    start: u64,
    length: u64,
    flags: u32,
    mapped_extents: u32,
    extent_count: u32,
    reserved: u32,
    extents: [Extent; BATCH as usize],
}

/// The extents of the regular file at `path`: their bytes, and of those the
/// bytes the filesystem shares with another file or a snapshot. The path
/// itself is opened (a symbolic link is an error, not followed), without
/// blocking, so a FIFO swapped in cannot stall `explain`, and checked
/// through the descriptor to be a regular file. A filesystem that keeps no
/// extent map (tmpfs, exFAT) answers `EOPNOTSUPP`, an
/// [`io::ErrorKind::Unsupported`] error here.
pub(in crate::platform) fn extents(path: &Path) -> io::Result<Extents> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_NOCTTY)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let mut map = Map {
        start: 0,
        length: 0,
        flags: 0,
        mapped_extents: 0,
        extent_count: 0,
        reserved: 0,
        extents: [EMPTY; BATCH as usize],
    };
    let mut out = Extents::default();
    let mut start = 0u64;
    loop {
        map.start = start;
        map.length = u64::MAX - start;
        map.flags = 0;
        map.mapped_extents = 0;
        map.extent_count = BATCH;
        // The request is an `unsigned long` to glibc and an `int` to musl,
        // with the same bits.
        #[allow(clippy::cast_possible_wrap, clippy::cast_lossless)]
        let request = FS_IOC_FIEMAP as _;
        // SAFETY: the descriptor is open for the call, and `map` is a
        // `struct fiemap` whose `fm_extent_count` says how many extents its
        // array holds, which the kernel writes no further than; it lives on
        // this stack frame for the call.
        let rc = unsafe { libc::ioctl(file.as_raw_fd(), request, &raw mut map) };
        if rc < 0 {
            let error = io::Error::last_os_error();
            match error.raw_os_error() {
                Some(libc::EINTR) => continue,
                Some(libc::EOPNOTSUPP) => {
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        "the filesystem keeps no extent map",
                    ));
                }
                _ => return Err(error),
            }
        }
        let mapped = map.mapped_extents.min(BATCH);
        let mut last = false;
        let before = start;
        for extent in &map.extents[..mapped as usize] {
            out.mapped = out.mapped.saturating_add(extent.length);
            if extent.flags & FIEMAP_EXTENT_SHARED != 0 {
                out.shared = out.shared.saturating_add(extent.length);
            }
            last |= extent.flags & FIEMAP_EXTENT_LAST != 0;
            start = extent.logical.saturating_add(extent.length);
        }
        // Fewer extents than asked for: the range holds no more. A map
        // that does not move on would never end.
        if last || mapped < BATCH || start <= before {
            return Ok(out);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use duscope_testkit::TestEnv;

    use super::extents;

    #[test]
    fn p5_walk_02_a_plain_file_shares_nothing_and_a_link_is_not_followed() {
        let env = TestEnv::new();
        let dir = env.tmp_dir().join("fiemap");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("plain");
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(&vec![7u8; 64 * 1024]).unwrap();
        file.sync_all().unwrap();
        match extents(&path) {
            Ok(found) => {
                assert!(found.mapped >= 64 * 1024, "{found:?}");
                assert_eq!(found.shared, 0, "{found:?}");
            }
            // tmpfs keeps no extent map.
            Err(e) if e.kind() == std::io::ErrorKind::Unsupported => {
                eprintln!("skipped: no extent map on the temporary directory's filesystem");
            }
            Err(e) => panic!("{e}"),
        }
        std::os::unix::fs::symlink(&path, dir.join("link")).unwrap();
        assert_eq!(
            extents(&dir.join("link")).unwrap_err().raw_os_error(),
            Some(libc::ELOOP)
        );
        assert_eq!(
            extents(&dir).unwrap_err().kind(),
            std::io::ErrorKind::InvalidInput
        );
    }
}
