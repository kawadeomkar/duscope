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
    /// The scan does not fit the arena's record fields.
    #[error("the scan is too large for the arena: {0}")]
    ArenaFull(&'static str),
}
