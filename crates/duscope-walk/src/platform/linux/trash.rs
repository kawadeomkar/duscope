//! The Trash on Linux: the XDG Trash specification, through the `trash`
//! crate. An item on the home Trash's filesystem goes to the home Trash
//! (`$XDG_DATA_HOME/Trash`, else `~/.local/share/Trash`); another goes to
//! its volume's own (`$topdir/.Trash/$uid` or `$topdir/.Trash-$uid`).
//!
//! Where it cannot use a volume's own Trash, the crate falls back to the
//! home Trash across filesystems: it copies the item and then removes it.
//! That fallback is never taken here: an item whose volume has no Trash
//! that can be used, or made, is [`TrashError::Unavailable`] before the
//! crate is asked.

use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

use crate::TrashError;
use crate::platform::posix;

/// Moves `path` to the Trash. In a test run (`guarded`) only an item on the
/// filesystem of the test's `$XDG_DATA_HOME` is moved, into the test's own
/// home Trash.
pub(in crate::platform) fn trash(path: &Path, guarded: bool) -> Result<(), TrashError> {
    let failed =
        |e: std::io::Error| TrashError::Failed(format!("cannot read {}: {e}", path.display()));
    let dev = std::fs::symlink_metadata(path).map_err(failed)?.dev();
    let home = home_trash().ok_or_else(|| {
        TrashError::Unavailable(
            "neither XDG_DATA_HOME nor HOME is set: there is no home Trash".to_owned(),
        )
    })?;
    let home_dev = nearest_dev(&home).map_err(failed)?;
    if dev != home_dev {
        if guarded {
            return Err(TrashError::Unavailable(format!(
                "the test environment moves only items on the filesystem of $XDG_DATA_HOME to \
                 the Trash, and {} is on another",
                path.display()
            )));
        }
        let top = topdir(path, dev);
        if !volume_trash_usable(&top) {
            return Err(TrashError::Unavailable(format!(
                "the volume mounted at {} has no Trash: it has no .Trash-{uid} and its top \
                 directory cannot hold one",
                top.display(),
                uid = posix::system::effective_uid()
            )));
        }
    }
    trash::delete(path).map_err(|e| TrashError::Failed(e.to_string()))
}

/// The home Trash, as the crate finds it.
fn home_trash() -> Option<PathBuf> {
    let set = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
    set("XDG_DATA_HOME")
        .map(|data| PathBuf::from(data).join("Trash"))
        .or_else(|| set("HOME").map(|home| PathBuf::from(home).join(".local/share/Trash")))
}

/// The device of `path`, or of its nearest ancestor that exists: the home
/// Trash is made on first use.
fn nearest_dev(path: &Path) -> std::io::Result<u64> {
    let mut at = Some(path);
    while let Some(dir) = at {
        if let Ok(meta) = std::fs::metadata(dir) {
            return Ok(meta.dev());
        }
        at = dir.parent();
    }
    std::fs::metadata("/").map(|m| m.dev())
}

/// The top directory of the filesystem `path` is on (device `dev`): its
/// highest ancestor on that device.
fn topdir(path: &Path, dev: u64) -> PathBuf {
    let mut top = path.parent().unwrap_or(path);
    while let Some(parent) = top.parent() {
        if std::fs::metadata(parent).map(|m| m.dev()).ok() != Some(dev) {
            break;
        }
        top = parent;
    }
    top.to_path_buf()
}

/// Whether the crate can put an item into the Trash of the volume whose top
/// directory is `top`: one it made before, or an administrator's `.Trash`
/// with this user's directory in it, or a top directory it can make one in.
fn volume_trash_usable(top: &Path) -> bool {
    let uid = posix::system::effective_uid();
    top.join(".Trash").join(uid.to_string()).is_dir()
        || top.join(format!(".Trash-{uid}")).is_dir()
        || posix::writable(top)
}
