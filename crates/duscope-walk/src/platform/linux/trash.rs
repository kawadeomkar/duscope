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
//! nothing is removed in its place. (The `trash` crate, which macOS uses,
//! falls back to copying an item across filesystems into the home Trash and
//! then removing it, so Linux does not use it.)

use std::fmt::Write as _;
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{ErrorKind, Write as _};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

use crate::TrashError;
use crate::platform::posix;

/// The sticky bit of a directory's mode.
const STICKY: u32 = 0o1000;

/// Moves `path`, absolute, to the Trash. In a test run (`guarded`) only an
/// item on the filesystem of the test's `$XDG_DATA_HOME` is moved, into the
/// test's own home Trash.
pub(in crate::platform) fn trash(path: &Path, guarded: bool) -> Result<(), TrashError> {
    let failed =
        |e: std::io::Error| TrashError::Failed(format!("cannot read {}: {e}", path.display()));
    let dev = fs::symlink_metadata(path).map_err(failed)?.dev();
    let home = home_trash(guarded).ok_or_else(|| {
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
    move_into(path, &trash, top.as_deref())
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

/// Renames `path` into `trash` under a name of its own, writing its
/// `.trashinfo` first: the name is taken by creating that file, which only
/// one mover can. `top` is the volume's top directory for a volume's own
/// Trash, whose `Path` is relative to it.
fn move_into(path: &Path, trash: &Path, top: Option<&Path>) -> Result<(), TrashError> {
    let failed =
        |e: std::io::Error| TrashError::Failed(format!("cannot move {}: {e}", path.display()));
    let mut builder = DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder.create(trash.join("files")).map_err(failed)?;
    builder.create(trash.join("info")).map_err(failed)?;
    let name = path
        .file_name()
        .ok_or_else(|| TrashError::Failed(format!("{} has no name", path.display())))?;
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
        let info = trash.join("info").join(&info_name);
        let file = OpenOptions::new().write(true).create_new(true).open(&info);
        let mut file = match file {
            Ok(file) => file,
            Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(failed(e)),
        };
        let target = trash.join("files").join(&taken);
        // rename(2) replaces an existing file: a name whose info file went
        // missing is passed over too.
        if fs::symlink_metadata(&target).is_ok() {
            let _ = fs::remove_file(&info);
            continue;
        }
        if let Err(e) = file
            .write_all(record.as_bytes())
            .and_then(|()| file.sync_all())
        {
            let _ = fs::remove_file(&info);
            return Err(failed(e));
        }
        return match fs::rename(path, &target) {
            Ok(()) => Ok(()),
            Err(e) => {
                let _ = fs::remove_file(&info);
                if e.kind() == ErrorKind::CrossesDevices {
                    Err(TrashError::Unavailable(format!(
                        "{} is on another filesystem than its Trash, {}",
                        path.display(),
                        trash.display()
                    )))
                } else {
                    Err(failed(e))
                }
            }
        };
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
    use super::escape;

    #[test]
    fn p4_core_01_a_trashinfo_path_is_escaped_as_a_uri() {
        assert_eq!(
            escape(b"/home/a b/caf\xc3\xa9%"),
            "/home/a%20b/caf%C3%A9%25"
        );
    }
}
