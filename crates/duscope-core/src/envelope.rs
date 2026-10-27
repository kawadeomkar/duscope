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

/// The version of the envelope's shape. It changes additively only once
/// P2-CLI-01 freezes it at 0.2.
pub const SCHEMA_VERSION: &str = "0.1";

/// The program that wrote an envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct Tool {
    /// Always `duscope`.
    pub name: &'static str,
    /// The release, as `duscope --version` prints it.
    pub version: &'static str,
}

impl Tool {
    /// This build.
    pub const CURRENT: Tool = Tool {
        name: "duscope",
        version: env!("CARGO_PKG_VERSION"),
    };
}

/// What every list verb prints under `--json`: one object, its rows in
/// `entries`.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Envelope<T> {
    /// Whether the command succeeded.
    pub ok: bool,
    /// [`SCHEMA_VERSION`].
    pub schema_version: &'static str,
    /// The program.
    pub tool: Tool,
    /// The scan the rows come from.
    pub scan: ScanBlock,
    /// What was asked.
    pub query: QueryBlock,
    /// The rows.
    pub entries: Vec<T>,
    /// Rows in `entries`.
    pub returned_count: u64,
    /// Rows the query matched before `--limit`.
    pub total_count: u64,
    /// Whether `--limit` left rows out.
    pub truncated: bool,
    /// Where the next page starts; null on the last page (pages arrive
    /// with the index).
    pub next_cursor: Option<String>,
    /// Warnings, with their codes.
    pub warnings: Vec<Warning>,
    /// Up to three runnable commands for the same scan, most useful first.
    pub next: Vec<String>,
}

/// The scan an envelope describes.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ScanBlock {
    /// The saved scan's id; null for a scan that was not saved.
    pub id: Option<String>,
    /// The root, absolute, symbolic links resolved.
    pub root: String,
    /// The root as the command line gave it.
    pub root_as_given: String,
    /// When the scan started.
    pub started_at: String,
    /// When it finished.
    pub finished_at: String,
    /// The size the rows are ranked and shown by: `allocated` or
    /// `apparent`.
    pub size_kind: &'static str,
    /// The attributes collected: `basic` or `full`.
    pub attrs: &'static str,
    /// Worker threads.
    pub thread_count: u64,
    /// The root volume's filesystem type.
    pub fs_type: String,
    /// Whether the scan stayed on the root's filesystem (`-x`).
    pub device_confined: bool,
    /// Bytes on disk, hard links and clones counted once.
    pub total_allocated_bytes: u64,
    /// Apparent bytes, hard links counted once.
    pub total_apparent_bytes: u64,
    /// Non-directories.
    pub file_count: u64,
    /// Directories, the root included.
    pub dir_count: u64,
    /// Directories that could not be read, and entries listed but not
    /// read.
    pub error_count: u64,
    /// Entries that disappeared during the scan.
    pub vanished_count: u64,
    /// Whether the scan reused an earlier one (never before phase 5).
    pub incremental: bool,
    /// Every volume the scan met, entered or not.
    pub volumes: Vec<VolumeEntry>,
    /// Files left out and directories not entered by the excludes
    /// (`--exclude`, `--exclude-from`, `--ignore-from`, `--exclude-caches`).
    pub excluded_count: u64,
    /// Each directory the excludes kept the scan out of, path bytewise.
    pub excluded: Vec<ExcludedEntry>,
}

/// A directory the scan listed but did not enter because of an exclude.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ExcludedEntry {
    /// Its absolute path, as lossy UTF-8.
    pub path: String,
    /// `excluded` (a glob or ignore pattern) or `excluded_cache`.
    pub reason: &'static str,
}

/// A volume the scan met.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct VolumeEntry {
    /// Where it is mounted.
    pub mount_point: String,
    /// The filesystem type.
    pub fs_type: String,
    /// Whether the scan entered it.
    pub entered: bool,
    /// Why not (`one_file_system`, `network`, …); null when it did.
    pub reason: Option<&'static str>,
    /// Mounted read-only.
    pub read_only: bool,
    /// The volume's size.
    pub total_bytes: u64,
    /// Bytes an unprivileged user can still write.
    pub free_bytes: u64,
}

/// What a list verb was asked.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct QueryBlock {
    /// The verb.
    pub command: &'static str,
    /// The path the rows are under, absolute.
    pub path: String,
    /// `--depth`; null when not limited.
    pub depth: Option<u32>,
    /// `--limit`; null when every row is returned (`--format ndjson`).
    pub limit: Option<u64>,
    /// The sort key and direction (`allocated_desc`).
    pub sort: String,
    /// The filters applied, as given (`--min-size=1M`).
    pub filters: Vec<String>,
}

/// A warning.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Warning {
    /// One of the conventions' warning codes.
    pub code: &'static str,
    /// One sentence.
    pub message: String,
    /// How many things it is about, where that applies.
    pub count: Option<u64>,
    /// One imperative sentence naming a command.
    pub hint: Option<String>,
}

/// A failure, as `--json` prints it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ErrorEnvelope {
    /// Always false.
    pub ok: bool,
    /// [`SCHEMA_VERSION`].
    pub schema_version: &'static str,
    /// The program.
    pub tool: Tool,
    /// What went wrong.
    pub error: ErrorObject,
}

/// The conventions' error object.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ErrorObject {
    /// The error code (`scan_not_found`, `permission_denied`, …).
    pub code: &'static str,
    /// One sentence.
    pub message: String,
    /// One imperative sentence naming a command; null when none helps.
    pub hint: Option<String>,
    /// Whether the same command may succeed if run again.
    pub retryable: bool,
    /// Who can fix it: `user`, `agent` or `none`.
    pub fixable_by: &'static str,
}

/// A row of `top`.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct TopEntry {
    /// The entry's absolute path, as lossy UTF-8.
    pub path: String,
    /// The path's bytes, which `--plain` writes raw; not in the JSON.
    #[serde(skip)]
    pub path_bytes: Vec<u8>,
    /// Its name, as lossy UTF-8.
    pub name: String,
    /// The name's bytes in base64, only when they are not valid UTF-8.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name_raw: Option<String>,
    /// `file`, `dir`, `symlink` or `other`.
    pub kind: &'static str,
    /// Bytes on disk, the subtree's for a directory.
    pub allocated_bytes: u64,
    /// Apparent bytes, the subtree's for a directory.
    pub apparent_bytes: u64,
    /// The share of the root's total in the scan's size kind, in percent
    /// with one decimal.
    pub pct_of_root: f64,
    /// Non-directories in the subtree; 1 for a file.
    pub file_count: u64,
    /// Directories in the subtree, the directory included; 0 for a file.
    pub dir_count: u64,
    /// The newest modification time in the subtree.
    pub max_mtime: String,
    /// What else is true of it (`upper_bound`, `unreadable`, `sparse`, …).
    pub flags: Vec<&'static str>,
}

/// A line of `--format ndjson`: the scan first, then each row, then the
/// counts. It streams every row, so it has no cursor.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum NdjsonLine<'a, T> {
    /// The first line.
    Scan {
        /// [`SCHEMA_VERSION`].
        schema_version: &'static str,
        /// The program.
        tool: Tool,
        /// The scan.
        scan: &'a ScanBlock,
        /// What was asked.
        query: &'a QueryBlock,
    },
    /// A row.
    Entry(&'a T),
    /// The last line.
    End {
        /// Rows written.
        returned_count: u64,
        /// Rows the query matched.
        total_count: u64,
        /// Warnings, with their codes.
        warnings: &'a [Warning],
    },
}

/// A name as the envelope carries it: `name` is the bytes as UTF-8, with
/// invalid sequences replaced; `name_raw` is the bytes in base64, present
/// only when they are not valid UTF-8.
pub fn name_fields(bytes: &[u8]) -> (String, Option<String>) {
    match std::str::from_utf8(bytes) {
        Ok(name) => (name.to_owned(), None),
        Err(_) => (
            String::from_utf8_lossy(bytes).into_owned(),
            Some(base64(bytes)),
        ),
    }
}

/// Standard base64 (RFC 4648, with padding), encoded in-tree: the one use
/// does not need a crate.
pub fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[(n >> (18 - 6 * i)) as usize & 63]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{base64, name_fields};

    #[test]
    fn p1_cli_02_base64_matches_rfc_4648() {
        for (input, expected) in [
            (&b""[..], ""),
            (b"f", "Zg=="),
            (b"fo", "Zm8="),
            (b"foo", "Zm9v"),
            (b"foob", "Zm9vYg=="),
            (b"fooba", "Zm9vYmE="),
            (b"foobar", "Zm9vYmFy"),
            (b"\xff\xfe\x00", "//4A"),
        ] {
            assert_eq!(base64(input), expected, "{input:?}");
        }
    }

    #[test]
    fn p1_cli_02_names_carry_raw_bytes_only_when_needed() {
        assert_eq!(name_fields(b"notes.md"), ("notes.md".to_owned(), None));
        let (name, raw) = name_fields(b"caf\xe9.txt");
        assert_eq!(name, "caf\u{fffd}.txt");
        assert_eq!(raw.as_deref(), Some("Y2Fm6S50eHQ="));
    }
}
