//! The crate's error enum.

/// Why an operation of the engine failed.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    /// A rule table (`classes.toml`, `caches.toml`, or a user's table)
    /// could not be read.
    #[error("{file}:{line}: {message}")]
    Rules {
        /// The table's file name.
        file: String,
        /// The 1-based line the problem is on.
        line: usize,
        /// What is wrong.
        message: String,
    },
    /// The configuration file is not valid.
    #[error("{file}:{line}: {message}")]
    Config {
        /// The file's path.
        file: String,
        /// The 1-based line the problem is on.
        line: usize,
        /// What is wrong.
        message: String,
    },
    /// The configuration file could not be read.
    #[error("{file}: {source}")]
    ConfigRead {
        /// The file's path.
        file: String,
        /// Why.
        source: std::io::Error,
    },
    /// An index could not be written.
    #[error("cannot write the index {file}: {message}")]
    IndexWrite {
        /// The index file's path.
        file: String,
        /// What failed.
        message: String,
    },
    /// A saved scan could not be read: not an index, or damaged.
    #[error("cannot read the index {file}: {message}")]
    IndexCorrupt {
        /// The index file's path.
        file: String,
        /// What failed.
        message: String,
    },
    /// A saved scan was written in an index version this build cannot
    /// read.
    #[error("the index {file} is version {found}, which this duscope cannot read")]
    IndexVersion {
        /// The index file's path.
        file: String,
        /// The version it records.
        found: String,
    },
    /// No saved scan answers `--scan`.
    #[error("{0}")]
    ScanNotFound(String),
    /// `--scan` was given something that is not a scan selector, or an id
    /// prefix more than one scan has.
    #[error("{0}")]
    Selector(String),
    /// The scan does not fit the arena's record fields.
    #[error("the scan is too large for the arena: {0}")]
    ArenaFull(&'static str),
    /// A query needs attributes the scan did not collect.
    #[error("{0} needs --attrs full, which this scan did not collect")]
    AttrsNotCollected(&'static str),
    /// `--cursor` is not a cursor, or not one of this scan and query.
    #[error("{0}")]
    InvalidCursor(String),
    /// `query --sql` refused the statement: not one read of the index's
    /// tables through the functions it allows, or past one of its limits.
    #[error("{0}")]
    SqlDenied(String),
    /// `query --sql` ran past its `--timeout`, in seconds.
    #[error("the statement ran past the {0} s --timeout")]
    SqlTimeout(u64),
    /// `query --sql` was given a statement `SQLite` cannot prepare.
    #[error("{0}")]
    SqlInvalid(String),
}
