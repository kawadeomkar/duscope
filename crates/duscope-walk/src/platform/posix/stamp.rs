//! The directory sweep of `scans --check`: each directory's modification
//! and status-change times, read again by path with one call that asks for
//! them and the type alone (`statx` on Linux, `getattrlist` on macOS), with
//! the walk's flags, so a directory nothing touched reads exactly as the
//! walk recorded it. A final symlink is never followed, unless the scan
//! followed links (`--follow-symlinks`): its walk records a link to a
//! directory as that directory, under the link's name.
//!
//! A path at or beyond `PATH_MAX`, which the walk reaches through its
//! parents' descriptors, is read the same way: its leading directories are
//! opened a piece at a time and the rest is read relative to the last one.

use std::ffi::CStr;
use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use crate::DirStamp;

/// The longest path, its terminating NUL included, one call takes.
const PATH_MAX: usize = libc::PATH_MAX as usize;

/// The stamp of each directory at `dirs`, in order, following symbolic
/// links when `follow_symlinks`.
pub(crate) fn stamp_dirs<'a>(
    dirs: impl Iterator<Item = &'a Path>,
    follow_symlinks: bool,
) -> Vec<io::Result<DirStamp>> {
    // One buffer for every path's C string: a sweep reads a path for each
    // directory of a scan, and an allocation each would cost more than the
    // copy.
    let mut buf = Vec::new();
    dirs.map(|dir| {
        stamp_path(
            dir.as_os_str().as_bytes(),
            PATH_MAX,
            follow_symlinks,
            &mut buf,
        )
    })
    .collect()
}

/// The stamp of the directory at `path`, read in one call when it is
/// shorter than `limit` bytes with its NUL, else relative to its leading
/// directories, opened in pieces that are; a final symlink, of the path or
/// of a piece, followed when `follow`.
fn stamp_path(path: &[u8], limit: usize, follow: bool, buf: &mut Vec<u8>) -> io::Result<DirStamp> {
    if path.len() < limit {
        return stamp_at(None, c_str(path, buf)?, follow);
    }
    let open = if follow {
        super::OPEN_DIR & !libc::O_NOFOLLOW
    } else {
        super::OPEN_DIR
    };
    let mut dir: Option<OwnedFd> = None;
    let mut rest = path;
    while rest.len() >= limit {
        // The longest leading piece, up to a slash, that fits; the first
        // piece of an absolute path keeps its leading slash.
        let cut = rest[..limit]
            .iter()
            .rposition(|&b| b == b'/')
            .filter(|&cut| cut > 0)
            .ok_or_else(|| io::Error::from_raw_os_error(libc::ENAMETOOLONG))?;
        let opened = super::open_with(
            dir.as_ref().map(AsFd::as_fd),
            c_str(&rest[..cut], buf)?,
            open,
        )?;
        dir = Some(opened);
        rest = &rest[cut + 1..];
    }
    stamp_at(dir.as_ref().map(AsFd::as_fd), c_str(rest, buf)?, follow)
}

/// `bytes` as a C string in `buf`.
fn c_str<'b>(bytes: &[u8], buf: &'b mut Vec<u8>) -> io::Result<&'b CStr> {
    buf.clear();
    buf.extend_from_slice(bytes);
    buf.push(0);
    CStr::from_bytes_with_nul(buf).map_err(|_| io::ErrorKind::InvalidInput.into())
}

/// A time as seconds and nanoseconds, as Unix nanoseconds, saturating at
/// the ends of `i64`.
fn time_ns(sec: i64, nsec: i64) -> i64 {
    let ns = i128::from(sec) * 1_000_000_000 + i128::from(nsec);
    i64::try_from(ns).unwrap_or(if ns < 0 { i64::MIN } else { i64::MAX })
}

/// What a path that is no longer a directory reads as.
fn not_a_directory() -> io::Error {
    io::Error::from_raw_os_error(libc::ENOTDIR)
}

/// The stamp of `name` relative to the directory open on `dir` (`None`:
/// the working directory, so an absolute path reads alone): `statx(2)` with
/// the walk's flags, asking for the type and the two times, following a
/// final symlink only when `follow`.
#[cfg(target_os = "linux")]
fn stamp_at(dir: Option<BorrowedFd<'_>>, name: &CStr, follow: bool) -> io::Result<DirStamp> {
    use std::mem::MaybeUninit;
    use std::os::fd::AsRawFd;

    const MASK: u32 = libc::STATX_TYPE | libc::STATX_MTIME | libc::STATX_CTIME;
    // As the walk reads an entry: never follow a final symlink (but under
    // --follow-symlinks, as the walk reads a link's target), never trigger
    // an automount, take a network client's cached attributes.
    let flags = libc::AT_NO_AUTOMOUNT
        | libc::AT_STATX_DONT_SYNC
        | if follow { 0 } else { libc::AT_SYMLINK_NOFOLLOW };
    let at = dir.map_or(libc::AT_FDCWD, |fd| fd.as_raw_fd());
    let mut buf = MaybeUninit::<libc::statx>::uninit();
    let stx = loop {
        // The system call itself rather than libc's wrapper, which the musl
        // Rust links statically (the release builds) does not have.
        // SAFETY: `at` is AT_FDCWD or a descriptor open for the borrow,
        // `name` is NUL-terminated, and `buf` is writable for one statx;
        // the arguments are the system call's, in its order.
        let rc = unsafe {
            libc::syscall(
                libc::SYS_statx,
                at,
                name.as_ptr(),
                flags,
                MASK,
                buf.as_mut_ptr(),
            )
        };
        if rc == 0 {
            // SAFETY: statx succeeded, so it filled the struct.
            break unsafe { buf.assume_init() };
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    };
    if u32::from(stx.stx_mode) & libc::S_IFMT != libc::S_IFDIR {
        return Err(not_a_directory());
    }
    let ns = |t: libc::statx_timestamp| time_ns(t.tv_sec, i64::from(t.tv_nsec));
    Ok(DirStamp {
        mtime_ns: ns(stx.stx_mtime),
        ctime_ns: ns(stx.stx_ctime),
    })
}

/// The record `getattrlist` packs for the three attributes asked for: its
/// length, the object type, then the modification and the status-change
/// time as `struct timespec`s, in the order of their bits.
#[cfg(target_os = "macos")]
const RECORD_BYTES: usize = 4 + 4 + 16 + 16;

/// A buffer for that record, on an 8-byte boundary.
#[cfg(target_os = "macos")]
#[repr(C, align(8))]
struct Record([u8; 64]);

/// The stamp of `name` relative to the directory open on `dir` (`None`:
/// the working directory, so an absolute path reads alone):
/// `getattrlistat(2)` asking for the object type and the two times, not
/// following a final symlink unless `follow`, as the walk reads an entry. A
/// filesystem that cannot answer it, or answers without one of them, is
/// asked with `fstatat`.
#[cfg(target_os = "macos")]
fn stamp_at(dir: Option<BorrowedFd<'_>>, name: &CStr, follow: bool) -> io::Result<DirStamp> {
    use std::os::fd::AsRawFd;

    /// `VDIR` of `enum vtype` (`<sys/vnode.h>`).
    const VDIR: u32 = 2;
    let at = dir.map_or(libc::AT_FDCWD, |fd| fd.as_raw_fd());
    let mut attrs = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: libc::ATTR_CMN_OBJTYPE | libc::ATTR_CMN_MODTIME | libc::ATTR_CMN_CHGTIME,
        volattr: 0,
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    };
    let mut record = Record([0; 64]);
    let read = loop {
        // SAFETY: `at` is AT_FDCWD or a descriptor open for the borrow,
        // `name` is NUL-terminated, `attrs` is an initialised attrlist the
        // kernel only reads, and `record` is writable for its length and
        // 8-byte aligned.
        let rc = unsafe {
            libc::getattrlistat(
                at,
                name.as_ptr(),
                (&raw mut attrs).cast(),
                record.0.as_mut_ptr().cast(),
                record.0.len(),
                if follow {
                    0
                } else {
                    libc::c_ulong::from(libc::FSOPT_NOFOLLOW)
                },
            )
        };
        if rc == 0 {
            break Ok(());
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            break Err(error);
        }
    };
    let bytes = &record.0;
    let word =
        |at: usize| u32::from_ne_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    let long = |at: usize| {
        let mut eight = [0u8; 8];
        eight.copy_from_slice(&bytes[at..at + 8]);
        i64::from_ne_bytes(eight)
    };
    match read {
        Ok(()) if usize::try_from(word(0)).is_ok_and(|len| len == RECORD_BYTES) => {
            if word(4) != VDIR {
                return Err(not_a_directory());
            }
            Ok(DirStamp {
                mtime_ns: time_ns(long(8), long(16)),
                ctime_ns: time_ns(long(24), long(32)),
            })
        }
        Err(e) if !unsupported(&e) => Err(e),
        // Refused, or answered without every attribute.
        _ => {
            let stat = match (dir, follow) {
                (Some(dir), false) => super::lstat_at(dir, name)?,
                (None, false) => super::lstat(name)?,
                (_, true) => stat_following(at, name)?,
            };
            if stat.st_mode & libc::S_IFMT != libc::S_IFDIR {
                return Err(not_a_directory());
            }
            Ok(DirStamp {
                mtime_ns: time_ns(stat.st_mtime, stat.st_mtime_nsec),
                ctime_ns: time_ns(stat.st_ctime, stat.st_ctime_nsec),
            })
        }
    }
}

/// `fstatat(2)` of `name` relative to `at`, following a final symlink.
#[cfg(target_os = "macos")]
fn stat_following(at: libc::c_int, name: &CStr) -> io::Result<libc::stat> {
    use std::mem::MaybeUninit;

    let mut stat = MaybeUninit::<libc::stat>::uninit();
    loop {
        // SAFETY: `at` is AT_FDCWD or a descriptor open for the caller's
        // borrow, `name` is NUL-terminated, and `stat` is writable for one
        // `struct stat`.
        if unsafe { libc::fstatat(at, name.as_ptr(), stat.as_mut_ptr(), 0) } == 0 {
            // SAFETY: fstatat succeeded, so it filled the struct.
            return Ok(unsafe { stat.assume_init() });
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

/// Whether `error` says the filesystem cannot answer an attribute read, as
/// the walk tells it.
#[cfg(target_os = "macos")]
fn unsupported(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(libc::ENOTSUP | libc::EOPNOTSUPP | libc::EINVAL | libc::ENOSYS)
    ) || error.kind() == io::ErrorKind::Unsupported
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs::{self, File, FileTimes, Permissions};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::{Path, PathBuf};
    use std::time::{Duration, SystemTime};

    use duscope_testkit::TestEnv;

    use super::{PATH_MAX, stamp_dirs, stamp_path};
    use crate::{DirId, DirStamp, Entry, Kind, ScanEvent, WalkOptions, walk};

    /// Every directory a walk of `root` records, by path.
    fn walked_dirs(root: &Path) -> BTreeMap<PathBuf, Entry> {
        walked_dirs_with(root, &WalkOptions::default())
    }

    /// Every directory a walk of `root` with `opts` records, by path.
    fn walked_dirs_with(root: &Path, opts: &WalkOptions) -> BTreeMap<PathBuf, Entry> {
        let mut paths: BTreeMap<DirId, PathBuf> = BTreeMap::new();
        let mut dirs = BTreeMap::new();
        walk(root, opts, |event| {
            if let ScanEvent::Entry(entry) = event {
                let path = if entry.dir == Some(DirId::ROOT) {
                    root.to_owned()
                } else {
                    paths[&entry.parent].join(entry.name.as_os_str())
                };
                if let Some(id) = entry.dir {
                    paths.insert(id, path.clone());
                }
                if entry.kind == Kind::Dir {
                    dirs.insert(path, entry);
                }
            }
        })
        .unwrap();
        dirs
    }

    fn stamp(path: &Path) -> std::io::Result<DirStamp> {
        stamp_dirs(std::iter::once(path), false).pop().unwrap()
    }

    fn stamp_following(path: &Path) -> std::io::Result<DirStamp> {
        stamp_dirs(std::iter::once(path), true).pop().unwrap()
    }

    /// A tree of a few directories, a file and two links.
    fn small_tree(env: &TestEnv) -> PathBuf {
        let root = env.work_dir().join("tree");
        for dir in ["a/b/c", "a/d", "e"] {
            fs::create_dir_all(root.join(dir)).unwrap();
        }
        fs::write(root.join("a/file"), b"x").unwrap();
        symlink("a", root.join("link-to-dir")).unwrap();
        symlink("nowhere", root.join("dangling")).unwrap();
        root
    }

    /// 2001-09-09T01:46:40Z, long before any test runs.
    const LONG_AGO_NS: i64 = 1_000_000_000 * 1_000_000_000;

    /// Sets the modification time of the directory `dir` to [`LONG_AGO_NS`],
    /// so the next change to it reads as one however coarse the clock.
    fn backdate(dir: &Path) {
        let then = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
        File::open(dir)
            .unwrap()
            .set_times(FileTimes::new().set_modified(then))
            .unwrap();
        assert_eq!(stamp(dir).unwrap().mtime_ns, LONG_AGO_NS);
    }

    #[test]
    fn p5_core_02_stamp_dirs_reads_what_the_walk_recorded() {
        let env = TestEnv::new();
        let root = small_tree(&env);
        let dirs = walked_dirs(&root);
        assert_eq!(dirs.len(), 6, "{:?}", dirs.keys());
        let paths: Vec<&Path> = dirs.keys().map(PathBuf::as_path).collect();
        let stamps = stamp_dirs(paths.iter().copied(), false);
        assert_eq!(stamps.len(), paths.len());
        for ((path, entry), stamp) in dirs.iter().zip(stamps) {
            let stamp = stamp.unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            assert_eq!(
                (stamp.mtime_ns, stamp.ctime_ns),
                (entry.mtime_ns, entry.ctime_ns),
                "{}",
                path.display()
            );
            assert_eq!(stamp.latest_ns(), entry.mtime_ns.max(entry.ctime_ns));
        }
    }

    #[test]
    fn p5_core_02_stamp_dirs_sees_an_entry_added_renamed_or_removed() {
        let env = TestEnv::new();
        let root = small_tree(&env);
        let dir = root.join("a/b");
        let changed = |change: &dyn Fn()| {
            backdate(&dir);
            change();
            stamp(&dir).unwrap().mtime_ns != LONG_AGO_NS
        };
        assert!(changed(&|| fs::write(dir.join("new"), b"").unwrap()));
        assert!(changed(&|| fs::rename(
            dir.join("new"),
            dir.join("renamed")
        )
        .unwrap()));
        assert!(changed(&|| fs::remove_file(dir.join("renamed")).unwrap()));
        // Its mode changes its status-change time alone.
        let e = root.join("e");
        let before = stamp(&e).unwrap();
        fs::set_permissions(&e, Permissions::from_mode(0o700)).unwrap();
        let after = stamp(&e).unwrap();
        assert_eq!(after.mtime_ns, before.mtime_ns);
        assert!(after.ctime_ns >= before.ctime_ns);
    }

    #[test]
    fn p5_core_02_stamp_dirs_refuses_what_is_not_a_directory() {
        let env = TestEnv::new();
        let root = small_tree(&env);
        let errno = |rel: &str| stamp(&root.join(rel)).unwrap_err().raw_os_error();
        assert_eq!(errno("a/file"), Some(libc::ENOTDIR));
        // A link is read itself, never followed, whether or not it leads
        // to a directory.
        assert_eq!(errno("link-to-dir"), Some(libc::ENOTDIR));
        assert_eq!(errno("dangling"), Some(libc::ENOTDIR));
        assert_eq!(errno("gone"), Some(libc::ENOENT));
        // One failure leaves the others' stamps in their places.
        let paths = [root.join("a"), root.join("gone"), root.join("e")];
        let stamps = stamp_dirs(paths.iter().map(PathBuf::as_path), false);
        assert!(stamps[0].is_ok() && stamps[1].is_err() && stamps[2].is_ok());
    }

    #[test]
    fn p5_core_02_stamp_dirs_follows_links_as_a_walk_that_followed_them() {
        let env = TestEnv::new();
        let root = small_tree(&env);
        symlink("a/file", root.join("link-to-file")).unwrap();
        // A walk that follows links records the link to a directory as
        // that directory, under the link's name, and the directories
        // under it under the link too.
        let opts = WalkOptions {
            follow_symlinks: true,
            ..WalkOptions::default()
        };
        let dirs = walked_dirs_with(&root, &opts);
        let link = root.join("link-to-dir");
        assert!(dirs.contains_key(&link.join("b/c")), "{:?}", dirs.keys());
        let paths: Vec<&Path> = dirs.keys().map(PathBuf::as_path).collect();
        for ((path, entry), stamp) in dirs.iter().zip(stamp_dirs(paths.iter().copied(), true)) {
            let stamp = stamp.unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            assert_eq!(
                (stamp.mtime_ns, stamp.ctime_ns),
                (entry.mtime_ns, entry.ctime_ns),
                "{}",
                path.display()
            );
        }
        assert_eq!(
            stamp_following(&link).unwrap(),
            stamp(&root.join("a")).unwrap()
        );
        // Without following, the link is itself, no directory.
        assert_eq!(
            stamp(&link).unwrap_err().raw_os_error(),
            Some(libc::ENOTDIR)
        );
        // Followed, a link to a file is still no directory, and one to
        // nothing is gone.
        let errno = |rel: &str| stamp_following(&root.join(rel)).unwrap_err().raw_os_error();
        assert_eq!(errno("link-to-file"), Some(libc::ENOTDIR));
        assert_eq!(errno("dangling"), Some(libc::ENOENT));
        // A long path whose piece ends at the link opens the piece through
        // it when following, and refuses it otherwise (`ELOOP` on Linux,
        // `ENOTDIR` on macOS, which `O_DIRECTORY` decides first).
        let deep = link.join("b");
        let bytes = deep.as_os_str().as_bytes();
        let limit = link.as_os_str().len() + 1;
        let mut buf = Vec::new();
        assert_eq!(
            stamp_path(bytes, limit, true, &mut buf).unwrap(),
            stamp(&root.join("a/b")).unwrap()
        );
        let refused = stamp_path(bytes, limit, false, &mut buf).unwrap_err();
        assert!(
            matches!(refused.raw_os_error(), Some(libc::ELOOP | libc::ENOTDIR)),
            "{refused}"
        );
    }

    #[test]
    fn p5_core_02_a_long_path_reads_in_pieces_as_a_short_one_does() {
        let env = TestEnv::new();
        let root = small_tree(&env);
        // The longest component of the root's path: a piece holds at least
        // one, with the slashes on either side.
        let longest = root
            .components()
            .map(|c| c.as_os_str().len())
            .max()
            .unwrap();
        let mut buf = Vec::new();
        for rel in ["a", "a/b", "a/b/c", "e"] {
            let path = root.join(rel);
            let bytes = path.as_os_str().as_bytes();
            let whole = stamp_path(bytes, PATH_MAX, false, &mut buf).unwrap();
            // The root in one piece, then the rest; and one piece per
            // component, or nearly.
            for limit in [root.as_os_str().len() + 1, longest + 2] {
                let pieces = stamp_path(bytes, limit, false, &mut buf)
                    .unwrap_or_else(|e| panic!("{} in pieces of {limit}: {e}", path.display()));
                assert_eq!(pieces, whole, "{} in pieces of {limit}", path.display());
            }
        }
        // No piece can be cut short enough.
        let path = root.join("a/b/c");
        let error = stamp_path(path.as_os_str().as_bytes(), 2, false, &mut buf).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::ENAMETOOLONG));
    }

    #[test]
    fn p5_core_02_a_directory_deeper_than_path_max_reads_as_the_walk_recorded() {
        let env = TestEnv::new();
        let root = env.work_dir().join("deep");
        fs::create_dir(&root).unwrap();
        // Names of 200 bytes, enough of them to pass PATH_MAX, made in two
        // relative steps, since no single call takes the whole path: half
        // from the root, the rest from the end of that half.
        let name = "d".repeat(200);
        let levels = PATH_MAX / 201 + 2;
        let half = vec![name.as_str(); levels / 2].join("/");
        let rest = vec![name.as_str(); levels - levels / 2].join("/");
        for (from, rel) in [(root.clone(), &half), (root.join(&half), &rest)] {
            let made = env
                .command("mkdir")
                .arg("-p")
                .arg(rel)
                .current_dir(&from)
                .status()
                .unwrap();
            assert!(made.success());
        }
        let dirs = walked_dirs(&root);
        assert_eq!(dirs.len(), levels + 1);
        let deepest = dirs.keys().map(|p| p.as_os_str().len()).max().unwrap();
        assert!(deepest > PATH_MAX, "{deepest} bytes");
        let paths: Vec<&Path> = dirs.keys().map(PathBuf::as_path).collect();
        for ((path, entry), stamp) in dirs.iter().zip(stamp_dirs(paths.iter().copied(), false)) {
            let stamp = stamp.unwrap_or_else(|e| panic!("{} bytes: {e}", path.as_os_str().len()));
            assert_eq!(
                (stamp.mtime_ns, stamp.ctime_ns),
                (entry.mtime_ns, entry.ctime_ns)
            );
        }
        // Removed by relative steps too.
        let removed = env
            .command("rm")
            .arg("-rf")
            .arg(&name)
            .current_dir(&root)
            .status()
            .unwrap();
        assert!(removed.success());
    }
}
