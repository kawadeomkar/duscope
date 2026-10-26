//! The crate's error enum and the per-entry error record.

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
