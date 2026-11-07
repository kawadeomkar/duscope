//! `clean`'s platform facts on macOS: the Trash, through `NSFileManager`'s
//! `trashItemAtURL` (the `trash` crate), which moves an item into its
//! volume's Trash (the user's for the startup volume, `.Trashes` on another)
//! and never removes anything; and whether an entry is a firmlink.

use std::path::Path;

use trash::macos::{DeleteMethod, TrashContextExtMacos};

use super::volume;
use crate::TrashError;
use crate::platform::posix;

/// The startup volume's two mount points: the sealed system volume and the
/// Data volume, which holds the account's home and its Trash.
const STARTUP: [&str; 2] = ["/", "/System/Volumes/Data"];

/// Moves `path` to its volume's Trash. In a test run (`guarded`) an item on
/// the startup volume is refused: `NSFileManager` puts it in the account's own
/// Trash whatever `HOME` says, so a test moves only items on a disk image it
/// attached.
pub(in crate::platform) fn trash(path: &Path, guarded: bool) -> Result<(), TrashError> {
    if guarded {
        let mount = volume::of(path)
            .map_err(|e| TrashError::Failed(format!("cannot read {}: {e}", path.display())))?
            .mount_point;
        if STARTUP.iter().any(|startup| mount == Path::new(startup)) {
            return Err(TrashError::Unavailable(format!(
                "the test environment moves nothing on the startup volume to the Trash, and {} \
                 is on {}",
                path.display(),
                mount.display()
            )));
        }
    }
    // The crate percent-encodes a path that is not UTF-8 into another path,
    // which could name another entry: such a path is never handed to it.
    if path.to_str().is_none() {
        return Err(TrashError::Failed(format!(
            "cannot move {}: the Trash takes only UTF-8 paths",
            path.display()
        )));
    }
    // Moving an entry needs its directory writable: refused for that, the
    // volume's Trash is not the matter.
    if path.parent().is_some_and(|parent| !posix::writable(parent)) {
        return Err(TrashError::Failed(format!(
            "cannot move {}: permission denied on its directory",
            path.display()
        )));
    }
    let mut context = trash::TrashContext::default();
    // Not the crate's default, Finder through osascript, which needs a
    // session with Finder running and plays its sound.
    context.set_delete_method(DeleteMethod::NsFileManager);
    context.delete(path).map_err(|e| {
        // The crate passes on NSFileManager's localized description only, not
        // its code: one about permission (a protected folder, in English) is a
        // failure to move, any other that the volume's Trash took nothing.
        let message = e.to_string();
        if message.to_lowercase().contains("permission") {
            TrashError::Failed(message)
        } else {
            TrashError::Unavailable(message)
        }
    })
}

/// Whether `path` is a firmlink of the System volume, by the system's table
/// of them: a lookup by path resolves a firmlink to the Data volume's
/// directory, whose flags do not carry `SF_FIRMLINK`.
pub(in crate::platform) fn firmlink(path: &Path) -> bool {
    super::firmlinks().iter().any(|link| link.source == path)
}
