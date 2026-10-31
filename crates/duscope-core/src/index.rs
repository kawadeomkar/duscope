//! The on-disk index: one database file per scan, written once and never
//! edited (`docs/index-format.md`).
//!
//! The DDL is `schemas/index/v2.sql`, copied byte for byte into
//! `index/schema.sql` so the published crate carries it.

pub mod reader;
pub mod scanid;
pub mod writer;

use std::ffi::OsString;
use std::path::PathBuf;

pub use reader::{Index, IndexVolume};
pub use scanid::{ScanId, random_bytes};
pub use writer::{
    EXCLUDED, EXCLUDED_CACHE, FIRMLINK, IndexWriter, LATER_LINK, ScanMeta, UPPER_BOUND, index_path,
};

/// The index version this build writes and reads: major.minor, apart from the
/// envelope's `schema_version`. A reader opens every minor of its major.
pub const INDEX_VERSION: &str = "2.0";

/// The keys of `scan_meta`, every one present in an index; the DDL writes
/// `index_version` and the writer the others.
pub const SCAN_META_KEYS: [&str; 24] = [
    "index_version",
    "scan_id",
    "root",
    "root_as_given",
    "started_at",
    "finished_at",
    "attrs",
    "options",
    "platform",
    "scanning_uid",
    "thread_count",
    "size_kind",
    "volume_total",
    "volume_free",
    "unaccounted_bytes",
    "error_count",
    "vanished_count",
    "excluded_count",
    "n_entries",
    "incremental",
    "parent_scan_id",
    "source",
    "tool_version",
    "sibling_order",
];

/// The DDL of the index: the tables, the `index_version` row, then the four
/// indexes.
pub const SCHEMA: &str = include_str!("index/schema.sql");

/// The comment line of [`SCHEMA`] that the indexes follow.
const INDEXES_FOLLOW: &str = "-- Indexes, created after the bulk insert.\n";

/// The index directory when `--index-dir`, `DUSCOPE_INDEX_DIR` and the
/// configuration name none: `$XDG_CACHE_HOME/duscope` when that is set and
/// absolute, else `duscope` in the platform's cache directory under `HOME`
/// (`~/Library/Caches` on macOS, `~/.cache` elsewhere). `env` reads a
/// variable; `None` when neither is set.
pub fn default_dir(env: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let set = |name: &str| env(name).filter(|v| !v.is_empty()).map(PathBuf::from);
    let cache = set("XDG_CACHE_HOME")
        .filter(|p| p.is_absolute())
        .or_else(|| set("HOME").map(|home| duscope_walk::cache_dir(&home)))?;
    Some(cache.join("duscope"))
}

/// [`SCHEMA`] in two parts: the tables with the `index_version` row, and the
/// indexes, which a writer creates after its inserts, since building an index
/// once costs less than keeping it up to date row by row.
pub fn schema_parts() -> (&'static str, &'static str) {
    SCHEMA.split_once(INDEXES_FOLLOW).unwrap_or((SCHEMA, ""))
}

#[cfg(test)]
mod tests;
