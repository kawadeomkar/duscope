//! mktree builds duscope's reference fixture tree deterministically from
//! `manifest.toml`, the manifest committed beside it.
//!
//! The manifest declares named cases (hard links, clones, sparse and
//! compressed files, unreadable directories, non-UTF-8 names, a 200-level
//! path, a 100,000-entry directory, symlink cycles, `CACHEDIR.TAG` files,
//! project markers for every cache rule, …) and seeded random subtrees. It
//! is expanded into a [`Plan`], the exact list of entries with every size,
//! mode and timestamp resolved; the SHA-256 of the plan's text is the
//! manifest hash, which depends only on the manifest and this generator.
//! Every mtime and atime is relative to the manifest's fixed instant, which
//! tests export as `DUSCOPE_TEST_NOW`.
//!
//! [`OutDir::ensure`] builds the tree (`target/fixtures.noindex/tree`) and its
//! [`Record`]; cases a platform or filesystem cannot build are recorded as
//! skipped there, with the reason and what stands in for them.
//!
//! Test tooling only: `publish = false`, used by `duscope-testkit` and by
//! `just fixtures`.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod build;
pub mod inspect;
pub mod manifest;
pub mod out;
pub mod plan;
pub mod record;
pub mod rng;
pub mod time;

pub use inspect::{Report, listing, verify};
pub use manifest::Manifest;
pub use out::{OutDir, remove_tree};
pub use plan::{Entry, Kind, Plan};
pub use record::{CaseRecord, Record, Status};

/// An error from reading the manifest or building the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(String);

impl Error {
    /// An error with this message.
    pub fn new(message: impl Into<String>) -> Error {
        Error(message.into())
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

/// The result type of this crate.
pub type Result<T> = std::result::Result<T, Error>;
