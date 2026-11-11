//! `clean`'s platform facts on macOS: the Trash, through `NSFileManager`'s
//! `trashItemAtURL:resultingItemURL:error:` (bound by `objc2-foundation`),
//! which moves an item into its volume's Trash (the user's for the startup
//! volume, `.Trashes` on another) and never removes anything; and whether an
//! entry is a firmlink.

use std::path::Path;

use objc2::rc::autoreleasepool;
use objc2_foundation::{NSFeatureUnsupportedError, NSFileManager, NSInteger, NSString, NSURL};

use super::volume;
use crate::TrashError;
use crate::platform::posix;

/// The startup volume's two mount points: the sealed system volume and the
/// Data volume, which holds the account's home and its Trash.
const STARTUP: [&str; 2] = ["/", "/System/Volumes/Data"];

/// Moves `path` to its volume's Trash when its device and inode are
/// `expect`. In a test run (`guarded`) an item on the startup volume is
/// refused: `NSFileManager` puts it in the account's own Trash whatever
/// `HOME` says, so a test moves only items on a disk image it attached.
pub(in crate::platform) fn trash(
    path: &Path,
    expect: (u64, u64),
    guarded: bool,
) -> Result<(), TrashError> {
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
    // An `NSString` holds text: a path that is not UTF-8 would reach
    // `NSFileManager` changed, naming another entry or none, so it is refused.
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
    // Still the entry the plan saw. `trashItemAtURL` takes a URL, not a
    // descriptor, so it is read by path, just before the call.
    {
        use std::os::unix::fs::MetadataExt as _;
        match std::fs::symlink_metadata(path) {
            Ok(meta) if (meta.dev(), meta.ino()) == expect => {}
            _ => {
                return Err(TrashError::Changed {
                    path: path.to_owned(),
                });
            }
        }
    }
    move_item(path)
}

/// The value of Foundation's `NSCocoaErrorDomain`, the domain of
/// `NSFileManager`'s own errors.
const COCOA_ERROR_DOMAIN: &str = "NSCocoaErrorDomain";

/// Moves `path` with `trashItemAtURL:resultingItemURL:error:`. A refusal is
/// told apart by its error's domain and code ([`classify`]), never by its
/// message, which Foundation words, and translates, as it sees fit.
fn move_item(path: &Path) -> Result<(), TrashError> {
    let Some(text) = url_text(path) else {
        return Err(TrashError::Failed(format!(
            "cannot move {}: it names no entry",
            path.display()
        )));
    };
    // What Foundation autoreleases during the call is released when it
    // returns, on whichever thread it runs.
    autoreleasepool(|_| {
        let url = NSURL::fileURLWithPath(&NSString::from_str(text));
        NSFileManager::defaultManager()
            .trashItemAtURL_resultingItemURL_error(&url, None)
            .map_err(|error| {
                let domain = error.domain().to_string();
                let code = error.code();
                let description = error.localizedDescription().to_string();
                let message = format!(
                    "cannot move {}: {} ({domain} {code})",
                    path.display(),
                    description.strip_suffix('.').unwrap_or(&description)
                );
                classify(&domain, code, message)
            })
    })
}

/// `path` as the text an `NSURL` is made from, when it is UTF-8 and ends in
/// a name. "", "/" and a path ending in ".." end in none: they are refused
/// rather than handed to `NSFileManager` as whatever folder they lead to.
fn url_text(path: &Path) -> Option<&str> {
    path.file_name()?;
    path.to_str()
}

/// What a refused move is, by its `NSError`'s domain and code.
/// `NSFeatureUnsupportedError` (3328) of the Cocoa domain says the volume has
/// no Trash (Apple's `FoundationErrors.h`: "some volumes may not support a
/// Trash folder"; a read-only volume answers it): [`TrashError::Unavailable`],
/// whose hint names `--permanent`. Anything else is [`TrashError::Failed`]:
/// no permission (513, a locked file's too; 257), no such file (4), a Trash
/// that cannot be made (512), and any code not known here, so that no error
/// unforeseen suggests deleting for good.
fn classify(domain: &str, code: NSInteger, message: String) -> TrashError {
    if domain == COCOA_ERROR_DOMAIN && code == NSFeatureUnsupportedError {
        TrashError::Unavailable(message)
    } else {
        TrashError::Failed(message)
    }
}

/// Whether `path` is a firmlink of the System volume, by the system's table
/// of them: a lookup by path resolves a firmlink to the Data volume's
/// directory, whose flags do not carry `SF_FIRMLINK`.
pub(in crate::platform) fn firmlink(path: &Path) -> bool {
    super::firmlinks().iter().any(|link| link.source == path)
}

#[cfg(test)]
mod tests;
