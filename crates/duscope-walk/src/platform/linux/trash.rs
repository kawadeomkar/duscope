//! The Trash on Linux, by the XDG Trash specification 1.0: an item on the
//! filesystem of the home Trash (`$XDG_DATA_HOME/Trash`, else
//! `~/.local/share/Trash`) goes there; another goes to its volume's own,
//! `$topdir/.Trash/$uid` when an administrator made `$topdir/.Trash` (a
//! directory with the sticky bit, not a link), else `$topdir/.Trash-$uid`.
//! Beside the item, in `info/`, a `.trashinfo` file names where it was and
//! when it was moved.
//!
//! The item is only ever renamed into the Trash, never copied: a volume
//! whose Trash cannot be used or made is [`TrashError::Unavailable`], and
//! nothing is removed in its place. (The `trash` crate's freedesktop backend
//! falls back to copying an item across filesystems into the home Trash and
//! then removing it, so the Trash is in-tree here; macOS calls
//! `NSFileManager` itself.)

use std::ffi::OsStr;
use std::fmt::Write as _;
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, ErrorKind, Write as _};
use std::os::fd::{AsFd as _, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{
    DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _,
};
use std::path::{Path, PathBuf};

use crate::platform::posix;
use crate::platform::posix::unlink::{self, CheckedDir, RenameError};
use crate::{RemoveError, TrashError};

/// The sticky bit of a directory's mode.
const STICKY: u32 = 0o1000;

/// Moves `path`, absolute, to the Trash when its device and inode are
/// `expect`: its directory opened from `/` one component at a time without
/// following a link, the entry read and renamed through that directory's
/// descriptor. In a test run (`guarded`) only an item on the filesystem of
/// the test's `$XDG_DATA_HOME` is moved, into the test's own home Trash.
pub(in crate::platform) fn trash(
    path: &Path,
    expect: (u64, u64),
    guarded: bool,
) -> Result<(), TrashError> {
    trash_to(path, expect, guarded, home_trash(guarded))
}

/// [`trash`], with `home` the home Trash (`None` where there is none): a
/// test gives one of its own, never reading the environment.
fn trash_to(
    path: &Path,
    expect: (u64, u64),
    guarded: bool,
    home: Option<PathBuf>,
) -> Result<(), TrashError> {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(TrashError::Failed(format!(
            "{} has no name",
            path.display()
        )));
    };
    let parent = CheckedDir::open(dir).map_err(|e| not_moved(path, e))?;
    unlink::check_entry(&parent, name, expect).map_err(|e| not_moved(path, e))?;
    // The Trash is chosen by the device of the item's directory, read
    // through its descriptor: a rename never leaves that directory's mount,
    // and on overlayfs only a directory reports the overlay's device, a
    // file the device of the layer under it (glib reads the parent's for
    // the same reason).
    let dev = parent.dev();
    let failed = |e: io::Error| TrashError::Failed(format!("cannot read {}: {e}", path.display()));
    let home = home.ok_or_else(|| {
        TrashError::Unavailable(if guarded {
            "the test environment sets no XDG_DATA_HOME: there is no test Trash".to_owned()
        } else {
            "neither XDG_DATA_HOME nor HOME is set: there is no home Trash".to_owned()
        })
    })?;
    let (trash, top) = if nearest_dev(&home).map_err(failed)? == dev {
        (home, None)
    } else if guarded {
        return Err(TrashError::Unavailable(format!(
            "the test environment moves only items on the filesystem of $XDG_DATA_HOME to \
             the Trash, and {} is on another",
            path.display()
        )));
    } else {
        let top = topdir(path, dev);
        let trash = volume_trash(&top).ok_or_else(|| {
            TrashError::Unavailable(format!(
                "the volume mounted at {} has no Trash this user can use, and cannot be given \
                 one",
                top.display()
            ))
        })?;
        (trash, Some(top))
    };
    move_into(path, &parent, name, expect, &trash, top.as_deref())
}

/// The Trash's error for a failure on the way to `path`, or at it.
fn not_moved(path: &Path, error: RemoveError) -> TrashError {
    match error {
        RemoveError::Changed { path } => TrashError::Changed { path },
        RemoveError::Io { path: at, source } => TrashError::Failed(format!(
            "cannot move {}: cannot read {}: {source}",
            path.display(),
            at.display()
        )),
        other => TrashError::Failed(format!("cannot move {}: {other}", path.display())),
    }
}

/// The home Trash; in a test run (`guarded`) only the one under the test's
/// `$XDG_DATA_HOME`.
fn home_trash(guarded: bool) -> Option<PathBuf> {
    let set = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
    let data = set("XDG_DATA_HOME").map(|data| PathBuf::from(data).join("Trash"));
    if guarded {
        return data;
    }
    data.or_else(|| set("HOME").map(|home| PathBuf::from(home).join(".local/share/Trash")))
}

/// The device of `path`, or of its nearest ancestor that exists: the home
/// Trash is made on first use.
fn nearest_dev(path: &Path) -> std::io::Result<u64> {
    let mut at = Some(path);
    while let Some(dir) = at {
        if let Ok(meta) = fs::metadata(dir) {
            return Ok(meta.dev());
        }
        at = dir.parent();
    }
    fs::metadata("/").map(|m| m.dev())
}

/// The top directory of the filesystem `path` is on (device `dev`): its
/// highest ancestor on that device.
fn topdir(path: &Path, dev: u64) -> PathBuf {
    let mut top = path.parent().unwrap_or(path);
    while let Some(parent) = top.parent() {
        if fs::metadata(parent).map(|m| m.dev()).ok() != Some(dev) {
            break;
        }
        top = parent;
    }
    top.to_path_buf()
}

/// The Trash of the volume whose top directory is `top`, made if need be:
/// `.Trash/$uid` under an administrator's `.Trash`, else `.Trash-$uid`;
/// `None` when neither can be used or made.
fn volume_trash(top: &Path) -> Option<PathBuf> {
    let uid = posix::system::effective_uid();
    let shared = top.join(".Trash");
    let admin = fs::symlink_metadata(&shared)
        .is_ok_and(|m| m.is_dir() && m.permissions().mode() & STICKY != 0);
    if admin {
        let own = shared.join(uid.to_string());
        if usable(&own) {
            return Some(own);
        }
    }
    let own = top.join(format!(".Trash-{uid}"));
    usable(&own).then_some(own)
}

/// Whether `dir` is a directory of this user's that is not a link, made
/// with mode 0700 when missing, with its `files` and `info` directories.
fn usable(dir: &Path) -> bool {
    let mut builder = DirBuilder::new();
    builder.mode(0o700);
    match builder.create(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
        Err(_) => return false,
    }
    let own = fs::symlink_metadata(dir)
        .is_ok_and(|m| m.is_dir() && m.uid() == posix::system::effective_uid());
    own && ["files", "info"].iter().all(|sub| {
        let sub = dir.join(sub);
        (builder.create(&sub).is_ok() || sub.is_dir()) && posix::writable(&sub)
    })
}

/// A Trash's `files` and `info` directories, open.
struct Dirs {
    files: OwnedFd,
    info: OwnedFd,
}

impl Dirs {
    /// Opens the Trash `trash` and its `files` and `info` directories, each
    /// made with mode 0700 if missing and opened through the Trash's
    /// descriptor, never through a symbolic link. A volume's Trash (`own`)
    /// is opened without following a link too, and must be this user's;
    /// the home Trash is made if need be, and is the directory its path
    /// leads to, since `$XDG_DATA_HOME` may be a link.
    fn open(trash: &Path, own: bool) -> io::Result<Dirs> {
        let mut flags = libc::O_DIRECTORY;
        if own {
            flags |= libc::O_NOFOLLOW;
        } else {
            DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(trash)?;
        }
        let dir = OpenOptions::new()
            .read(true)
            .custom_flags(flags)
            .open(trash)?;
        if own && dir.metadata()?.uid() != posix::system::effective_uid() {
            return Err(io::Error::new(
                ErrorKind::PermissionDenied,
                format!("{} is another user's", trash.display()),
            ));
        }
        Ok(Dirs {
            files: unlink::make_dir_at(dir.as_fd(), c"files")?,
            info: unlink::make_dir_at(dir.as_fd(), c"info")?,
        })
    }
}

/// Renames `name`, the entry `expect` in `parent` (at `path`), into
/// `trash` under a name of its own, writing its `.trashinfo` first: the
/// name is taken by creating that file, which only one mover can, and the
/// rename never replaces an entry already at it in `files`. `top` is the
/// volume's top directory for a volume's own Trash, whose `Path` is
/// relative to it.
fn move_into(
    path: &Path,
    parent: &CheckedDir,
    name: &OsStr,
    expect: (u64, u64),
    trash: &Path,
    top: Option<&Path>,
) -> Result<(), TrashError> {
    let failed = |e: io::Error| TrashError::Failed(format!("cannot move {}: {e}", path.display()));
    let dirs = Dirs::open(trash, top.is_some()).map_err(failed)?;
    let recorded = match top {
        Some(top) => path.strip_prefix(top).unwrap_or(path),
        None => path,
    };
    let record = format!(
        "[Trash Info]\nPath={}\nDeletionDate={}\n",
        escape(recorded.as_os_str().as_bytes()),
        posix::system::local_time()
    );
    for n in 1..=u32::MAX {
        let mut taken = name.to_owned();
        if n > 1 {
            taken.push(format!(".{n}"));
        }
        let mut info_name = taken.clone();
        info_name.push(".trashinfo");
        let mut file = match unlink::create_at(dirs.info.as_fd(), &info_name) {
            Ok(file) => file,
            Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(failed(e)),
        };
        let forget = || {
            let _ = unlink::unlink_at(dirs.info.as_fd(), &info_name);
        };
        if let Err(e) = file
            .write_all(record.as_bytes())
            .and_then(|()| file.sync_all())
        {
            forget();
            return Err(failed(e));
        }
        drop(file);
        match unlink::rename_checked(parent, name, expect, dirs.files.as_fd(), &taken) {
            Ok(()) => return Ok(()),
            // An entry in `files` whose info file went missing: its name is
            // passed over too.
            Err(RenameError::Taken) => forget(),
            Err(RenameError::Entry(e)) => {
                forget();
                return Err(not_moved(path, e));
            }
            // What took the item's place went to the Trash and could not be
            // moved back: its info file, which says where it was, stays.
            Err(RenameError::Stranded(e)) => {
                return Err(TrashError::Failed(format!(
                    "{} changed while it was being moved to the Trash: what was there in its \
                     place was moved to {} and could not be moved back ({e}), and the item was \
                     not moved",
                    path.display(),
                    trash.join("files").join(&taken).display()
                )));
            }
            Err(RenameError::Io(e)) => {
                forget();
                return Err(if e.kind() == ErrorKind::CrossesDevices {
                    TrashError::Unavailable(format!(
                        "{} is on another filesystem than its Trash, {}",
                        path.display(),
                        trash.display()
                    ))
                } else {
                    failed(e)
                });
            }
        }
    }
    Err(TrashError::Failed(format!(
        "no free name for {} in {}",
        path.display(),
        trash.display()
    )))
}

/// `bytes` as a `.trashinfo` file's `Path` writes them: escaped as a URI
/// path (RFC 3986), `/` kept.
fn escape(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    for &b in bytes {
        if b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::fs;
    use std::os::unix::fs::{MetadataExt as _, symlink};
    use std::path::{Path, PathBuf};

    use duscope_testkit::TestEnv;

    use super::{escape, move_into, trash_to};
    use crate::TrashError;
    use crate::platform::posix::unlink::{CheckedDir, hook};

    #[test]
    fn p4_core_01_a_trashinfo_path_is_escaped_as_a_uri() {
        assert_eq!(
            escape(b"/home/a b/caf\xc3\xa9%"),
            "/home/a%20b/caf%C3%A9%25"
        );
    }

    /// `dir/item/a.bin` to move, and `dir/trash` for a Trash of the test's
    /// own: never a real one.
    fn setup(env: &TestEnv) -> (PathBuf, PathBuf) {
        let dir = fs::canonicalize(env.tmp_dir()).unwrap();
        fs::create_dir_all(dir.join("item")).unwrap();
        fs::write(dir.join("item/a.bin"), b"planned").unwrap();
        (dir.join("item/a.bin"), dir.join("trash"))
    }

    /// `path`'s device and inode, as a plan records them.
    fn ident(path: &Path) -> (u64, u64) {
        let meta = fs::symlink_metadata(path).unwrap();
        (meta.dev(), meta.ino())
    }

    /// Moves `path` into `trash` as `clean` does.
    fn move_to(path: &Path, expect: (u64, u64), trash: &Path) -> Result<(), TrashError> {
        let parent = CheckedDir::open(path.parent().unwrap()).unwrap();
        move_into(
            path,
            &parent,
            path.file_name().unwrap(),
            expect,
            trash,
            None,
        )
    }

    /// The names in the directory `dir`, sorted.
    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn p5_core_05_linux_a_name_taken_in_files_is_passed_over_and_its_info_file_removed() {
        let env = TestEnv::new();
        let (file, trash) = setup(&env);
        // An entry in `files` whose info file went missing: the move passes
        // its name over, and leaves no info file of its own for that name.
        fs::create_dir_all(trash.join("files")).unwrap();
        fs::write(trash.join("files/a.bin"), b"trashed before").unwrap();
        move_to(&file, ident(&file), &trash).unwrap();
        assert_eq!(names(&trash.join("files")), ["a.bin", "a.bin.2"]);
        assert_eq!(names(&trash.join("info")), ["a.bin.2.trashinfo"]);
        assert_eq!(fs::read(trash.join("files/a.bin.2")).unwrap(), b"planned");
        assert_eq!(
            fs::read(trash.join("files/a.bin")).unwrap(),
            b"trashed before"
        );
        let info = fs::read_to_string(trash.join("info/a.bin.2.trashinfo")).unwrap();
        assert!(
            info.contains(&format!(
                "\nPath={}\n",
                escape(file.as_os_str().as_encoded_bytes())
            )),
            "{info}"
        );
    }

    #[test]
    fn p5_core_05_linux_a_changed_item_leaves_the_trash_as_it_was() {
        let env = TestEnv::new();
        let (file, trash) = setup(&env);
        let expect = ident(&file);
        // Gone before the move: nothing in the Trash.
        fs::rename(&file, file.with_file_name("a.away")).unwrap();
        fs::write(&file, b"another").unwrap();
        let err = move_to(&file, expect, &trash).unwrap_err();
        assert!(
            matches!(&err, TrashError::Changed { path } if *path == file),
            "{err:?}"
        );
        // Swapped between the check and the rename: what moved comes back,
        // and its info file goes.
        fs::remove_file(&file).unwrap();
        fs::rename(file.with_file_name("a.away"), &file).unwrap();
        let (at, away) = (file.clone(), file.with_file_name("a.away"));
        let _hook = hook::on_step(move |path| {
            if path == at {
                fs::rename(&at, &away).unwrap();
                fs::write(&at, b"swapped in").unwrap();
            }
        });
        let err = move_to(&file, expect, &trash).unwrap_err();
        assert!(matches!(err, TrashError::Changed { .. }), "{err:?}");
        assert_eq!(fs::read(&file).unwrap(), b"swapped in");
        assert!(names(&trash.join("files")).is_empty());
        assert!(names(&trash.join("info")).is_empty());
    }

    /// The names in the Trash `trash`'s `files` and `info` directories,
    /// none where they were never made.
    fn trashed(trash: &Path) -> Vec<String> {
        ["files", "info"]
            .iter()
            .filter(|sub| trash.join(sub).exists())
            .flat_map(|sub| names(&trash.join(sub)))
            .collect()
    }

    #[test]
    fn p5_core_05_linux_the_trash_moves_nothing_through_a_parent_swapped_for_a_symlink() {
        let env = TestEnv::new();
        let (file, _) = setup(&env);
        let item = file.parent().unwrap().to_owned();
        let (away, outside) = (
            item.with_file_name("item.away"),
            item.with_file_name("outside"),
        );
        // Outside, a link of the very file the plan saw: through a symbolic
        // link in place of its directory the path leads to the same device
        // and inode, so only the way there tells them apart.
        fs::create_dir(&outside).unwrap();
        fs::hard_link(&file, outside.join("a.bin")).unwrap();
        let expect = ident(&file);
        // The test's own home Trash, as `clean` under DUSCOPE_TEST_TRASH
        // finds it, never read from the environment.
        let home = env.xdg_data_home().join("Trash");
        let swap = |dir: &Path, away: &Path, to: &Path| {
            fs::rename(dir, away).unwrap();
            symlink(to, dir).unwrap();
        };
        let nothing_moved = || {
            assert!(trashed(&home).is_empty(), "{:?}", trashed(&home));
            for at in [away.join("a.bin"), outside.join("a.bin")] {
                assert_eq!(ident(&at), expect, "{}", at.display());
                assert_eq!(fs::read(&at).unwrap(), b"planned");
            }
        };
        // After the plan, before the move: the item's directory goes, and a
        // link to the outside one takes its place.
        swap(&item, &away, &outside);
        let err = trash_to(&file, expect, true, Some(home.clone())).unwrap_err();
        assert!(
            matches!(&err, TrashError::Changed { path } if *path == item),
            "{err:?}"
        );
        nothing_moved();
        // Put back, then swapped as the way to the item is opened, when its
        // directory is reached.
        fs::remove_file(&item).unwrap();
        fs::rename(&away, &item).unwrap();
        let (at, from, to) = (item.clone(), away.clone(), outside.clone());
        let _hook = hook::on_step(move |path| {
            if path == at {
                swap(&at, &from, &to);
            }
        });
        let err = trash_to(&file, expect, true, Some(home.clone())).unwrap_err();
        assert!(
            matches!(&err, TrashError::Changed { path } if *path == item),
            "{err:?}"
        );
        nothing_moved();
    }

    #[test]
    fn p5_core_05_linux_a_trash_reached_through_links_moves_nothing_through_them() {
        let env = TestEnv::new();
        let (file, trash) = setup(&env);
        let elsewhere = file.parent().unwrap().parent().unwrap().join("elsewhere");
        fs::create_dir_all(&elsewhere).unwrap();
        // `files` a link to another directory: never followed.
        fs::create_dir_all(&trash).unwrap();
        symlink(&elsewhere, trash.join("files")).unwrap();
        let err = move_to(&file, ident(&file), &trash).unwrap_err();
        assert!(matches!(err, TrashError::Failed(_)), "{err:?}");
        assert!(names(&elsewhere).is_empty());
        assert_eq!(fs::read(&file).unwrap(), b"planned");
        // A volume's Trash that is itself a link: never used.
        let volume = file.parent().unwrap().parent().unwrap().join(".Trash-1000");
        symlink(&elsewhere, &volume).unwrap();
        let parent = CheckedDir::open(file.parent().unwrap()).unwrap();
        let top = volume.parent().unwrap().to_owned();
        let err = move_into(
            &file,
            &parent,
            OsStr::new("a.bin"),
            ident(&file),
            &volume,
            Some(&top),
        )
        .unwrap_err();
        assert!(matches!(err, TrashError::Failed(_)), "{err:?}");
        assert!(names(&elsewhere).is_empty());
        assert_eq!(fs::read(&file).unwrap(), b"planned");
    }
}
