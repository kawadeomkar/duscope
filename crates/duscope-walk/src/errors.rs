//! The crate's error enums and the per-entry error record.

use std::io;
use std::path::PathBuf;

use crate::{DirId, NameRef};

/// Why a walk did not complete.
#[derive(Debug, thiserror::Error)]
pub enum WalkError {
    /// No provider is built for this platform yet.
    #[error("no scanner is built for this platform yet")]
    NoProvider,
    /// The root could not be read.
    #[error("cannot read {}: {source}", path.display())]
    Root {
        /// The root as given.
        path: PathBuf,
        /// The error reading it.
        source: io::Error,
    },
    /// A worker thread could not be started.
    #[error("cannot start a scanner thread: {0}")]
    Spawn(#[source] io::Error),
    /// The cancellation flag was set.
    #[error("the scan was cancelled")]
    Cancelled,
    /// The tree has more directories than a [`DirId`] can number.
    #[error("the tree has more than {} directories", u32::MAX)]
    TooManyDirectories,
}

/// Why an item was not moved to the Trash. Nothing was moved or removed.
#[derive(Debug, thiserror::Error)]
pub enum TrashError {
    /// The item's volume has no Trash this platform can move it into (a
    /// network mount, a read-only volume, some removable media), or the
    /// system refused the move.
    #[error("{0}")]
    Unavailable(String),
    /// The item could not be read or moved.
    #[error("{0}")]
    Failed(String),
}

/// Why [`fsevents::replay`](crate::fsevents::replay) read no history.
#[derive(Debug, thiserror::Error)]
pub enum FsEventsError {
    /// The platform keeps no `FSEvents` history.
    #[error("FSEvents history exists only on macOS")]
    Unsupported,
    /// The directory to replay cannot be opened or placed on its volume.
    #[error("cannot read {}: {source}", path.display())]
    Path {
        /// The directory.
        path: PathBuf,
        /// The error opening or placing it.
        source: io::Error,
    },
    /// The directory is on another device than the one the mark is of.
    #[error("{} is on device {found}, not device {dev}", path.display())]
    OtherDevice {
        /// The directory.
        path: PathBuf,
        /// The device of the mark.
        dev: u64,
        /// The device the directory is on.
        found: u64,
    },
    /// `FSEvents` logging is off on the volume: `.fseventsd/no_log` exists
    /// at its root.
    #[error("FSEvents logging is off on the volume at {} (.fseventsd/no_log)", mount_point.display())]
    Disabled {
        /// Where the volume is mounted.
        mount_point: PathBuf,
    },
    /// The device's history is no longer kept (no `FSEvents` UUID).
    #[error("device {dev} has no FSEvents history")]
    NoHistory {
        /// The device.
        dev: u64,
    },
    /// The device's history is another one than the mark's.
    #[error("device {dev} has the FSEvents history {found}, not {recorded}")]
    OtherHistory {
        /// The device.
        dev: u64,
        /// The mark's UUID.
        recorded: String,
        /// The UUID now.
        found: String,
    },
    /// The system was upgraded or replaced since the mark.
    #[error("the system is build {found:?}, not {recorded}, which took the mark")]
    OtherSystem {
        /// The mark's build.
        recorded: String,
        /// The build now; empty when it cannot be read.
        found: String,
    },
    /// The system's event ids are behind the mark's.
    #[error("the latest FSEvents event id {current} is behind the mark's {since}")]
    IdsBehind {
        /// The mark's event id.
        since: u64,
        /// The system's latest.
        current: u64,
    },
    /// The stream could not be created or started.
    #[error("cannot open an FSEvents stream on device {dev}")]
    Stream {
        /// The device.
        dev: u64,
    },
}

impl FsEventsError {
    /// The reason a rescan gives for its full scan; `None` where the
    /// platform keeps no history, which is no failed check.
    #[must_use]
    pub fn fallback(&self) -> Option<crate::fsevents::Fallback> {
        use crate::fsevents::Fallback;
        match self {
            FsEventsError::Unsupported => None,
            FsEventsError::Path { .. }
            | FsEventsError::OtherDevice { .. }
            | FsEventsError::NoHistory { .. }
            | FsEventsError::OtherHistory { .. } => Some(Fallback::VolumeChanged),
            FsEventsError::Disabled { .. } => Some(Fallback::FseventsDisabled),
            FsEventsError::OtherSystem { .. }
            | FsEventsError::IdsBehind { .. }
            | FsEventsError::Stream { .. } => Some(Fallback::HistoryGone),
        }
    }
}

/// An entry that was listed but could not be read: one `scan_errors` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanError {
    /// The directory that listed it.
    pub dir: DirId,
    /// The entry's name.
    pub name: NameRef,
    /// The error number.
    pub errno: i32,
}
