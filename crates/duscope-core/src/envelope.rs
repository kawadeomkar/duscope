//! The types of the JSON envelope that every verb prints under `--json`.
//!
//! The envelope types arrive with P1-CLI-02. Until P2-CLI-01 points the schema
//! drift test at them, the test runs on [`PlaceholderEnvelope`].
//!
//! Every envelope type derives `schemars::JsonSchema` in test builds only, with
//! `#[cfg_attr(test, derive(schemars::JsonSchema))]`. `schemars` is a
//! dev-dependency, so the derive never reaches the binary. The drift test
//! (`src/schema_drift.rs`) compares each derived schema with its checked-in file
//! under `schemas/`.

/// A stand-in for the envelope types, for the schema drift test until phase 2.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct PlaceholderEnvelope {
    /// Whether the command succeeded.
    pub ok: bool,
    /// The version of the envelope schema.
    pub schema_version: String,
    /// The rows.
    pub entries: Vec<PlaceholderEntry>,
    /// The number of rows in `entries`.
    pub returned_count: u64,
    /// Where the next page starts, or null on the last page.
    pub next_cursor: Option<String>,
}

/// One row of the placeholder envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct PlaceholderEntry {
    /// The absolute path of the entry.
    pub path: String,
    /// The bytes the entry occupies on disk.
    pub allocated_bytes: u64,
}
