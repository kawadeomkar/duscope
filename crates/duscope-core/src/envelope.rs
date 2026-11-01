//! The types of the JSON envelope that every verb prints under `--json`.
//!
//! The envelope types arrived with P1-CLI-02; P2-CLI-01 froze them at
//! `schema_version` 0.2, each verb's described by a file under
//! `schemas/envelope/`.
//!
//! Every envelope type derives `schemars::JsonSchema` in test builds only, with
//! `#[cfg_attr(test, derive(schemars::JsonSchema))]`. `schemars` is a
//! dev-dependency, so the derive never reaches the binary. The drift test
//! (`src/schema_drift.rs`) compares each derived schema with its checked-in file
//! under `schemas/`.

/// The version of the envelope's shape, frozen at 0.2 (P2-CLI-01): it
/// changes additively only, a minor at a time, and `schemas/envelope/`
/// describes it with every published minor under `history/`.
pub const SCHEMA_VERSION: &str = "0.2";

/// The warning codes of the conventions appendix, the only values of
/// [`Warning::code`].
pub const WARNING_CODES: [&str; 15] = [
    "partial_read",
    "mounts_excluded",
    "stale_index",
    "scan_age",
    "atime_unreliable",
    "attrs_not_collected",
    "dataless_present",
    "full_disk_access_missing",
    "clones_unavailable",
    "upper_bound",
    "vanished",
    "index_untouched",
    "spilled",
    "deprecated_flag",
    "tz_unavailable",
];

/// The error codes of the conventions appendix, the only values of
/// [`ErrorObject::code`], each with the exit code it carries.
pub const ERROR_CODES: [(&str, u8); 26] = [
    ("usage", 2),
    ("scan_not_found", 3),
    ("invalid_cursor", 2),
    ("unknown_field", 2),
    ("attrs_not_collected", 1),
    ("incompatible_scans", 2),
    ("confirmation_required", 1),
    ("expectation_mismatch", 1),
    ("refused_path", 1),
    ("handle_invalid", 1),
    ("handle_expired", 1),
    ("trash_unavailable", 1),
    ("path_changed", 1),
    ("untrusted_index", 1),
    ("not_compiled", 2),
    ("not_available", 2),
    ("deadline_exceeded", 1),
    ("index_version_mismatch", 5),
    ("index_corrupt", 5),
    ("index_write_failed", 1),
    ("index_stale", 1),
    ("sql_denied", 1),
    ("sql_timeout", 1),
    ("io_error", 1),
    ("permission_denied", 4),
    ("interrupted", 130),
];

/// The values of the envelope's enumerated fields, as its schemas list
/// them.
pub mod values {
    /// An entry's `kind`.
    pub const KINDS: [&str; 4] = ["file", "dir", "symlink", "other"];
    /// `size_kind`.
    pub const SIZE_KINDS: [&str; 2] = ["allocated", "apparent"];
    /// `attrs`.
    pub const ATTRS: [&str; 2] = ["basic", "full"];
    /// Why the scan did not enter a volume or directory.
    pub const SKIP_REASONS: [&str; 7] = [
        "firmlinked",
        "kernel",
        "network",
        "one_file_system",
        "bind_cycle",
        "excluded",
        "excluded_cache",
    ];
    /// Why a directory was excluded.
    pub const EXCLUDE_REASONS: [&str; 2] = ["excluded", "excluded_cache"];
    /// A row's `flags`.
    pub const FLAGS: [&str; 11] = [
        "sparse",
        "compressed",
        "dataless",
        "purgeable",
        "may_share",
        "shares_all",
        "mount_root",
        "subvolume",
        "upper_bound",
        "unreadable",
        "uncharged",
    ];
    /// `diff`'s `change`.
    pub const CHANGES: [&str; 5] = ["added", "removed", "grown", "shrunk", "kind_changed"];
    /// Who can fix an error.
    pub const FIXABLE_BY: [&str; 3] = ["user", "agent", "none"];
    /// `explain`'s `confidence`.
    pub const CONFIDENCES: [&str; 3] = ["high", "medium", "low"];
    /// `explain`'s `freeable_kind`.
    pub const FREEABLE_KINDS: [&str; 3] = ["exact", "lower_bound", "upper_bound"];
    /// The commands that regenerate a cache.
    pub const REGENERABLE_BY: [&str; 9] = [
        "npm install",
        "cargo build",
        "pip install",
        "gradle build",
        "cmake --build",
        "pod install",
        "composer install",
        "xcodebuild",
        "unknown",
    ];
    /// What `scans --prune` removed.
    pub const PRUNED_KINDS: [&str; 3] = ["index", "temporary", "spill"];
    /// The platforms a column of the index is filled on, macOS first.
    pub const PLATFORMS: [&str; 2] = ["macos", "linux"];
}

/// `values` with null, for a nullable field's values.
#[cfg(test)]
fn nullable(values: &[&'static str]) -> Vec<Option<&'static str>> {
    values.iter().copied().map(Some).chain([None]).collect()
}

/// A field whose values grow additively (a warning code, a flag): the
/// values known now, or any string, so a reader of this minor accepts the
/// next one's.
#[cfg(test)]
fn known<T: serde::Serialize>(values: T) -> serde_json::Value {
    serde_json::json!([{"enum": values}, {"type": "string"}])
}

/// The error codes alone, for [`ErrorObject::code`]'s values.
#[cfg(test)]
fn error_codes() -> Vec<&'static str> {
    ERROR_CODES.iter().map(|&(code, _)| code).collect()
}

/// The program that wrote an envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Tool {
    /// Always `duscope`.
    #[cfg_attr(test, schemars(extend("const" = "duscope")))]
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
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Envelope<T> {
    /// Whether the command succeeded.
    pub ok: bool,
    /// The version of the envelope's shape.
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
    /// Where the next page starts (`--cursor`); null on the last page.
    pub next_cursor: Option<String>,
    /// Warnings, with their codes.
    pub warnings: Vec<Warning>,
    /// Up to three runnable commands for the same scan, most useful first.
    pub next: Vec<String>,
}

/// The scan an envelope describes.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct ScanBlock {
    /// The saved scan's id; null for a scan that was not saved.
    pub id: Option<String>,
    /// The root, absolute, symbolic links resolved.
    pub root: String,
    /// The root as the command line gave it.
    pub root_as_given: String,
    /// When the scan started.
    #[cfg_attr(test, schemars(extend("format" = "date-time")))]
    pub started_at: String,
    /// When it finished.
    #[cfg_attr(test, schemars(extend("format" = "date-time")))]
    pub finished_at: String,
    /// The size the rows are ranked and shown by: `allocated` or
    /// `apparent`.
    #[cfg_attr(test, schemars(extend("enum" = values::SIZE_KINDS)))]
    pub size_kind: &'static str,
    /// The attributes collected: `basic` or `full`.
    #[cfg_attr(test, schemars(extend("enum" = values::ATTRS)))]
    pub attrs: &'static str,
    /// Worker threads.
    pub thread_count: u64,
    /// The root volume's filesystem type; null when the scan recorded no
    /// volume.
    pub fs_type: Option<String>,
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
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct ExcludedEntry {
    /// Its absolute path, as lossy UTF-8.
    pub path: String,
    /// `excluded` (a glob or ignore pattern) or `excluded_cache`.
    #[cfg_attr(test, schemars(extend("anyOf" = known(values::EXCLUDE_REASONS))))]
    pub reason: &'static str,
}

/// A volume the scan met.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct VolumeEntry {
    /// Where it is mounted.
    pub mount_point: String,
    /// The filesystem type.
    pub fs_type: String,
    /// Whether the scan entered it.
    pub entered: bool,
    /// Why not (`one_file_system`, `network`, …); null when it did.
    #[cfg_attr(test, schemars(extend("anyOf" = known(nullable(&values::SKIP_REASONS)))))]
    pub reason: Option<&'static str>,
    /// Mounted read-only.
    pub read_only: bool,
    /// The volume's size.
    pub total_bytes: u64,
    /// Bytes an unprivileged user can still write.
    pub free_bytes: u64,
    /// Its device, which places the scan's directories on it.
    #[serde(skip)]
    pub dev: u64,
    /// Its mount options.
    #[serde(skip)]
    pub options: Vec<String>,
}

/// What a list verb was asked.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
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
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Warning {
    /// One of the conventions' warning codes.
    #[cfg_attr(test, schemars(extend("anyOf" = known(WARNING_CODES))))]
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
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct ErrorEnvelope {
    /// Always false.
    pub ok: bool,
    /// The version of the envelope's shape.
    pub schema_version: &'static str,
    /// The program.
    pub tool: Tool,
    /// What went wrong.
    pub error: ErrorObject,
}

/// The conventions' error object.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct ErrorObject {
    /// The error code (`scan_not_found`, `permission_denied`, …).
    #[cfg_attr(test, schemars(extend("anyOf" = known(error_codes()))))]
    pub code: &'static str,
    /// One sentence.
    pub message: String,
    /// One imperative sentence naming a command; null when none helps.
    pub hint: Option<String>,
    /// Whether the same command may succeed if run again.
    pub retryable: bool,
    /// Who can fix it: `user`, `agent` or `none`.
    #[cfg_attr(test, schemars(extend("enum" = values::FIXABLE_BY)))]
    pub fixable_by: &'static str,
}

/// A row of `top`.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
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
    #[cfg_attr(test, schemars(extend("enum" = values::KINDS)))]
    pub kind: &'static str,
    /// How far below the root it is.
    pub depth: u32,
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
    #[cfg_attr(test, schemars(extend("format" = "date-time")))]
    pub max_mtime: String,
    /// The directories the row was cut through to reach it, outermost
    /// first: each held a child of at least 80% of its bytes.
    pub via: Vec<String>,
    /// What else is true of it (`upper_bound`, `unreadable`, `sparse`,
    /// `uncharged`, …).
    #[cfg_attr(test, schemars(extend("items" = {"type": "string", "anyOf": known(values::FLAGS)})))]
    pub flags: Vec<&'static str>,
}

/// A row of `tree`: the entries down to `--depth`, largest first under
/// each directory.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct TreeEntry {
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
    #[cfg_attr(test, schemars(extend("enum" = values::KINDS)))]
    pub kind: &'static str,
    /// How far below the root it is; the root is 0.
    pub depth: u32,
    /// Bytes on disk, the subtree's for a directory.
    pub allocated_bytes: u64,
    /// Apparent bytes, the subtree's for a directory.
    pub apparent_bytes: u64,
    /// The share of the root's total, in percent with one decimal.
    pub pct_of_root: f64,
    /// The share of its parent's total.
    pub pct_of_parent: f64,
    /// Non-directories in the subtree; 1 for a file.
    pub file_count: u64,
    /// Directories in the subtree, the directory included; 0 for a file.
    pub dir_count: u64,
    /// The newest modification time in the subtree.
    #[cfg_attr(test, schemars(extend("format" = "date-time")))]
    pub max_mtime: String,
    /// What else is true of it.
    #[cfg_attr(test, schemars(extend("items" = {"type": "string", "anyOf": known(values::FLAGS)})))]
    pub flags: Vec<&'static str>,
}

/// A row of `by-type`: one file class.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct ByTypeEntry {
    /// The class: `code`, `media`, `archives`, `data`, `binaries`, `docs`
    /// or `other`.
    pub class: &'static str,
    /// Bytes on disk of its files, hard links and clones counted once.
    pub allocated_bytes: u64,
    /// Apparent bytes of its files, hard links counted once.
    pub apparent_bytes: u64,
    /// The share of the root's total, in percent with one decimal.
    pub pct_of_root: f64,
    /// Its files.
    pub file_count: u64,
    /// Its largest files, largest first.
    pub largest: Vec<Example>,
}

/// One of a class's largest files.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Example {
    /// The file's absolute path, as lossy UTF-8.
    pub path: String,
    /// Its name, as lossy UTF-8.
    pub name: String,
    /// The name's bytes in base64, only when they are not valid UTF-8.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name_raw: Option<String>,
    /// Its bytes on disk.
    pub allocated_bytes: u64,
    /// Its apparent bytes.
    pub apparent_bytes: u64,
}

/// A row of `stale`: a large file nobody has changed (or read) since the
/// cutoff.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct StaleEntry {
    /// The file's absolute path, as lossy UTF-8.
    pub path: String,
    /// The path's bytes, which `--plain` writes raw; not in the JSON.
    #[serde(skip)]
    pub path_bytes: Vec<u8>,
    /// Its name, as lossy UTF-8.
    pub name: String,
    /// The name's bytes in base64, only when they are not valid UTF-8.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name_raw: Option<String>,
    /// `file`, `symlink` or `other`.
    #[cfg_attr(test, schemars(extend("enum" = values::KINDS)))]
    pub kind: &'static str,
    /// Bytes on disk.
    pub allocated_bytes: u64,
    /// Apparent bytes.
    pub apparent_bytes: u64,
    /// Last modification.
    #[cfg_attr(test, schemars(extend("format" = "date-time")))]
    pub mtime: String,
    /// Last inode change.
    #[cfg_attr(test, schemars(extend("format" = "date-time")))]
    pub ctime: String,
    /// Last access; null when the scan did not collect it.
    #[cfg_attr(test, schemars(extend("format" = "date-time")))]
    pub atime: Option<String>,
    /// Its hard links.
    pub nlink: u32,
    /// What else is true of it.
    #[cfg_attr(test, schemars(extend("items" = {"type": "string", "anyOf": known(values::FLAGS)})))]
    pub flags: Vec<&'static str>,
}

/// A line of `--format ndjson`: the scan first, then each row, then the
/// counts. It streams every row, so it has no cursor.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum NdjsonLine<'a, T> {
    /// The first line.
    Scan {
        /// The version of the envelope's shape.
        schema_version: &'static str,
        /// The program.
        tool: Tool,
        /// The scan.
        scan: &'a ScanBlock,
        /// What was asked.
        query: &'a QueryBlock,
    },
    /// The first line of a listing of saved scans, which has no scan.
    Listing {
        /// The version of the envelope's shape.
        schema_version: &'static str,
        /// The program.
        tool: Tool,
        /// What was asked.
        query: &'a ListingQuery,
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

/// Standard base64 (RFC 4648, with padding), encoded in-tree: a name's
/// bytes and a cursor need no crate.
pub fn base64(bytes: &[u8]) -> String {
    encode64(
        bytes,
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/",
        true,
    )
}

/// `bytes` in base64 with `alphabet`, padded to a multiple of four with
/// `=` when `pad`.
pub(crate) fn encode64(bytes: &[u8], alphabet: &[u8; 64], pad: bool) -> String {
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
                out.push(char::from(alphabet[(n >> (18 - 6 * i)) as usize & 63]));
            } else if pad {
                out.push('=');
            }
        }
    }
    out
}

/// A cache directory, as `caches` lists it.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct CacheEntry {
    /// Its path, absolute, as lossy UTF-8.
    pub path: String,
    /// Its path's bytes, for `--plain`.
    #[serde(skip)]
    pub path_bytes: Vec<u8>,
    /// Its name.
    pub name: String,
    /// The name's bytes in base64 when they are not UTF-8.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name_raw: Option<String>,
    /// The rule that names it: a name or glob, or `CACHEDIR.TAG`.
    pub rule: String,
    /// The command that regenerates it.
    #[cfg_attr(test, schemars(extend("anyOf" = known(values::REGENERABLE_BY))))]
    pub regenerable_by: &'static str,
    /// The directory holding the marker that confirms it (the project);
    /// null when a `CACHEDIR.TAG`, an ancestor path or nothing (`--loose`)
    /// confirms it.
    pub project_root: Option<String>,
    /// The project root's bytes, for `--plain`; empty when it has none.
    #[serde(skip)]
    pub project_bytes: Vec<u8>,
    /// Whether its marker, ancestor path or `CACHEDIR.TAG` confirms it;
    /// false only under `--loose`.
    pub confirmed: bool,
    /// Whether it holds a valid `CACHEDIR.TAG`.
    pub cachedir_tag: bool,
    /// Bytes on disk under it, hard links and clones counted once.
    pub allocated_bytes: u64,
    /// Apparent bytes under it.
    pub apparent_bytes: u64,
    /// Its share of the root's total, in percent.
    pub pct_of_root: f64,
    /// Non-directories under it.
    pub file_count: u64,
    /// The newest modification time under it.
    #[cfg_attr(test, schemars(extend("format" = "date-time")))]
    pub max_mtime: String,
}

/// A project's caches, as `caches --by-project` lists them.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct ProjectEntry {
    /// The directory holding the marker; null for the caches no marker
    /// confirms.
    pub project_root: Option<String>,
    /// The project root's bytes, for `--plain`; empty when it has none.
    #[serde(skip)]
    pub path_bytes: Vec<u8>,
    /// Bytes on disk in its caches.
    pub allocated_bytes: u64,
    /// Apparent bytes in its caches.
    pub apparent_bytes: u64,
    /// Its caches.
    pub cache_count: u64,
    /// The newest modification time in the project, caches included.
    #[cfg_attr(test, schemars(extend("format" = "date-time")))]
    pub max_mtime: String,
    /// Its largest caches' paths, largest first, at most ten;
    /// `cache_count` counts them all.
    pub caches: Vec<String>,
}

/// What a verb that answers with one object (`explain`, `summary`,
/// `query`; `schema` to come) prints under `--json`: the shared fields, the
/// scan it read (null when none was), and the object as `result`. `query`
/// says what it was asked in a block of its own.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct ObjectEnvelope<T, Q = ObjectQuery> {
    /// Whether the command succeeded.
    pub ok: bool,
    /// The version of the envelope's shape.
    pub schema_version: &'static str,
    /// The program.
    pub tool: Tool,
    /// The saved scan the answer comes from; null when none holds the path.
    pub scan: Option<ScanBlock>,
    /// What was asked.
    pub query: Q,
    /// The answer.
    pub result: T,
    /// Warnings, with their codes.
    pub warnings: Vec<Warning>,
    /// Up to three runnable commands, most useful first.
    pub next: Vec<String>,
}

/// What a one-object verb was asked.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct ObjectQuery {
    /// The verb.
    pub command: &'static str,
    /// The path it was asked about, absolute.
    pub path: String,
}

/// What `explain` says of one path: facts with their evidence, never a
/// verdict.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Explain {
    /// The path, absolute, as lossy UTF-8.
    pub path: String,
    /// The path's bytes, for `--plain`.
    #[serde(skip)]
    pub path_bytes: Vec<u8>,
    /// Its name.
    pub name: String,
    /// The name's bytes in base64 when they are not UTF-8.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name_raw: Option<String>,
    /// `file`, `dir`, `symlink` or `other`.
    #[cfg_attr(test, schemars(extend("enum" = values::KINDS)))]
    pub kind: &'static str,
    /// Apparent bytes: a directory's subtree, hard links counted once.
    pub apparent_bytes: u64,
    /// Bytes on disk: a directory's subtree, hard links and clones once.
    pub allocated_bytes: u64,
    /// A directory's non-directories; null for a file.
    pub file_count: Option<u64>,
    /// A directory's directories, itself included; null for a file.
    pub dir_count: Option<u64>,
    /// Its modification time.
    #[cfg_attr(test, schemars(extend("format" = "date-time")))]
    pub mtime: String,
    /// Its inode change time.
    #[cfg_attr(test, schemars(extend("format" = "date-time")))]
    pub ctime: String,
    /// Its access time, when collected.
    #[cfg_attr(test, schemars(extend("format" = "date-time")))]
    pub atime: Option<String>,
    /// Its creation time, when collected and the filesystem has one.
    #[cfg_attr(test, schemars(extend("format" = "date-time")))]
    pub btime: Option<String>,
    /// Its mode bits, when collected.
    pub mode: Option<u32>,
    /// Its owner, when collected.
    pub uid: Option<u32>,
    /// Its group, when collected.
    pub gid: Option<u32>,
    /// A file's link count; null for a directory.
    pub nlink: Option<u64>,
    /// Links of the inodes under the path that lie outside it.
    pub links_outside_subtree: Option<u64>,
    /// Whether a file is a member of a clone group; null for a directory,
    /// and where the scan recorded no clone ids (it did not count clones,
    /// or the filesystem has none: every one but APFS).
    pub is_clone: Option<bool>,
    /// The members of a file's clone group; null when it is none's.
    pub clone_group_size: Option<u64>,
    /// Members of the clone groups under the path that lie outside it;
    /// null where the scan recorded no clone ids.
    pub clone_members_outside: Option<u64>,
    /// The bytes deleting the path would free now.
    pub freeable_now_bytes: Option<u64>,
    /// `exact`, `lower_bound` or `upper_bound`.
    #[cfg_attr(test, schemars(extend("enum" = nullable(&values::FREEABLE_KINDS))))]
    pub freeable_kind: Option<&'static str>,
    /// The command that regenerates a cache directory, from the rules.
    #[cfg_attr(test, schemars(extend("anyOf" = known(nullable(&values::REGENERABLE_BY)))))]
    pub regenerable_by: Option<&'static str>,
    /// Whether the directory holds a valid `CACHEDIR.TAG`.
    pub cachedir_tag: bool,
    /// What git says of the path; null outside a repository.
    pub git: Option<ExplainGit>,
    /// Its storage flags, and `upper_bound`, `unreadable`, `uncharged`.
    #[cfg_attr(test, schemars(extend("items" = {"type": "string", "anyOf": known(values::FLAGS)})))]
    pub flags: Vec<&'static str>,
    /// `high`, `medium` or `low`.
    #[cfg_attr(test, schemars(extend("enum" = values::CONFIDENCES)))]
    pub confidence: &'static str,
    /// Where each fact came from: tables, rules, markers, files.
    pub evidence: Vec<String>,
    /// A handle for `clean --from`: base64url of what `explain` found
    /// (the path, its device and inode, its bytes on disk, the scan, an
    /// expiry fifteen minutes on) and their HMAC; null where none could be
    /// issued (the path is gone, or the key cannot be read).
    pub handle: Option<String>,
}

/// What git says of a path.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct ExplainGit {
    /// The repository's working tree root.
    pub repo_root: String,
    /// Whether git ignores the path, or a directory above it.
    pub ignored: bool,
    /// Whether the pattern that ignores it marks it precious (`$name`).
    pub precious: bool,
    /// Whether a tracked file lies at or under the path.
    pub tracked_inside: bool,
}

/// A directory or entry a scan could not read, as `explain --errors` lists
/// it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct ScanErrorEntry {
    /// Its path, absolute, as lossy UTF-8.
    pub path: String,
    /// Its path's bytes, for `--plain`.
    #[serde(skip)]
    pub path_bytes: Vec<u8>,
    /// `directory` (listed but not read) or `entry` (listed but not
    /// examined).
    #[cfg_attr(test, schemars(extend("enum" = ["directory", "entry"])))]
    pub kind: &'static str,
    /// The error number.
    pub errno: i32,
    /// What the system calls it.
    pub message: String,
}

/// What `scans` prints under `--json`: the fields of a list, without a scan
/// block (it lists the saved scans, not the rows of one), and the index
/// directory it read.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Listing<T> {
    /// Whether the command succeeded.
    pub ok: bool,
    /// The version of the envelope's shape.
    pub schema_version: &'static str,
    /// The program.
    pub tool: Tool,
    /// What was asked.
    pub query: ListingQuery,
    /// The rows.
    pub entries: Vec<T>,
    /// Rows in `entries`.
    pub returned_count: u64,
    /// Rows before any limit.
    pub total_count: u64,
    /// Whether a limit left rows out.
    pub truncated: bool,
    /// Where the next page starts; null on the last page.
    pub next_cursor: Option<String>,
    /// Warnings, with their codes.
    pub warnings: Vec<Warning>,
    /// Up to three runnable commands, most useful first.
    pub next: Vec<String>,
}

/// What `scans` was asked.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct ListingQuery {
    /// `scans`, `scans --prune` or `scans --check`.
    pub command: &'static str,
    /// The index directory, absolute.
    pub index_dir: String,
}

/// A saved scan, as `scans` lists it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct SavedScanEntry {
    /// The scan's id.
    pub id: String,
    /// Its root.
    pub root: String,
    /// When it started.
    #[cfg_attr(test, schemars(extend("format" = "date-time")))]
    pub started_at: String,
    /// When it finished.
    #[cfg_attr(test, schemars(extend("format" = "date-time")))]
    pub finished_at: String,
    /// The attributes it collected: `basic` or `full`.
    #[cfg_attr(test, schemars(extend("enum" = values::ATTRS)))]
    pub attrs: &'static str,
    /// Entries in it, the root included.
    pub entry_count: u64,
    /// Bytes on disk under the root.
    pub allocated_bytes: u64,
    /// Apparent bytes under the root.
    pub apparent_bytes: u64,
    /// Non-directories.
    pub file_count: u64,
    /// Directories, the root included.
    pub dir_count: u64,
    /// The scan a rescan started from; null for a fresh scan.
    pub parent_scan_id: Option<String>,
    /// `scan`, or `import:<format>`.
    pub source: String,
    /// The index version of its file.
    pub index_version: String,
    /// The size of its file.
    pub index_bytes: u64,
    /// Its file.
    pub index_path: String,
    /// The shortest prefix of the id, at least four digits, that no other
    /// listed scan shares: what people see.
    #[serde(skip)]
    pub short_id: String,
}

/// A file `scans --prune` removed.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct PrunedEntry {
    /// The file.
    pub path: String,
    /// `index`, `temporary` (a write that never finished) or `spill`.
    #[cfg_attr(test, schemars(extend("anyOf" = known(values::PRUNED_KINDS))))]
    pub kind: &'static str,
    /// The scan whose index it was; null for the others.
    pub id: Option<String>,
    /// Its size.
    pub size_bytes: u64,
}

/// A saved scan `scans --check` found sound.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct CheckedEntry {
    /// The scan's id.
    pub id: String,
    /// Its file.
    pub index_path: String,
    /// What `PRAGMA quick_check` said: `ok`.
    #[cfg_attr(test, schemars(extend("const" = "ok")))]
    pub quick_check: &'static str,
}

/// `summary`'s answer: a scan's totals under a path, each volume's share,
/// and how the scan ran.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Summary {
    /// The path, absolute.
    pub path: String,
    /// The path's bytes, for `--plain`.
    #[serde(skip)]
    pub path_bytes: Vec<u8>,
    /// The volumes the scan met under the path.
    pub volumes: Vec<SummaryVolume>,
    /// Bytes on disk, hard links and clones counted once.
    pub allocated_bytes: u64,
    /// Apparent bytes.
    pub apparent_bytes: u64,
    /// Bytes on disk as `du` counts them: hard links once, clones each.
    pub du_compatible_bytes: u64,
    /// Where the path is a mount point, what the volume uses that the scan
    /// did not find (its total less its free space, less its bytes found);
    /// null elsewhere.
    pub unaccounted_bytes: Option<u64>,
    /// The logical size of the dataless files (iCloud and other
    /// placeholders), which the allocated bytes leave out.
    pub dataless_bytes: u64,
    /// Non-directories.
    pub file_count: u64,
    /// Directories, the path included.
    pub dir_count: u64,
    /// Directories and entries the scan could not read.
    pub error_count: u64,
    /// Entries that disappeared during the scan: the whole scan's, since
    /// the index does not place them under a path.
    pub vanished_count: u64,
    /// `basic` or `full`.
    #[cfg_attr(test, schemars(extend("enum" = values::ATTRS)))]
    pub attrs: &'static str,
    /// Whether the scan updated an earlier one.
    pub incremental: bool,
    /// The scan it updated; null for a full scan.
    pub parent_scan_id: Option<String>,
    /// When the scan started.
    #[cfg_attr(test, schemars(extend("format" = "date-time")))]
    pub started_at: String,
    /// When it finished.
    #[cfg_attr(test, schemars(extend("format" = "date-time")))]
    pub finished_at: String,
    /// How long it took.
    pub duration_ms: u64,
    /// The worker threads it ran.
    pub thread_count: u64,
}

/// A volume's share in `summary`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct SummaryVolume {
    /// Where it is mounted.
    pub mount_point: String,
    /// Its filesystem.
    pub fs_type: String,
    /// Its size, at the scan's start; null where unknown.
    pub total_bytes: Option<u64>,
    /// Its free space, at the scan's start; null where unknown.
    pub free_bytes: Option<u64>,
    /// Bytes on disk the scan found on it under the path.
    pub allocated_bytes: u64,
    /// Apparent bytes.
    pub apparent_bytes: u64,
    /// Bytes as `du` counts them.
    pub du_compatible_bytes: u64,
    /// Whether the scan entered it.
    pub entered: bool,
    /// Why not, when it did not.
    #[cfg_attr(test, schemars(extend("anyOf" = known(nullable(&values::SKIP_REASONS)))))]
    pub reason: Option<&'static str>,
}

/// A change `diff` found.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct DiffEntry {
    /// The entry's path, absolute: in the new scan, or the old one for
    /// `removed`.
    pub path: String,
    /// The path's bytes, for `--plain`.
    #[serde(skip)]
    pub path_bytes: Vec<u8>,
    /// Its name.
    pub name: String,
    /// The name's bytes in base64 when they are not UTF-8.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name_raw: Option<String>,
    /// `added`, `removed`, `grown`, `shrunk` or `kind_changed`.
    #[cfg_attr(test, schemars(extend("enum" = values::CHANGES)))]
    pub change: &'static str,
    /// What it is: in the new scan, or the old one for `removed`.
    #[cfg_attr(test, schemars(extend("enum" = values::KINDS)))]
    pub kind: &'static str,
    /// Its bytes in the old scan; null when it was not there.
    pub old_bytes: Option<u64>,
    /// Its bytes in the new scan; null when it is not there.
    pub new_bytes: Option<u64>,
    /// New less old, in the size compared.
    pub delta_bytes: i64,
    /// For a directory added or removed whole, the entries in it, itself
    /// included; null otherwise.
    pub collapsed_count: Option<u64>,
}

/// What `diff` adds up: bytes and entries come and gone, whatever
/// `--min-delta` left out of the rows.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct DiffTotals {
    /// The old scan's id.
    pub old_scan: String,
    /// The new scan's id.
    pub new_scan: String,
    /// Bytes in the entries added.
    pub added_bytes: u64,
    /// Bytes in the entries removed.
    pub removed_bytes: u64,
    /// The new total less the old.
    pub net_bytes: i64,
    /// Entries in the new scan only, those in added directories included.
    pub added_count: u64,
    /// Entries in the old scan only.
    pub removed_count: u64,
    /// Files in both whose own size differs, and entries in both whose
    /// kind differs (beneath a directory `--depth` compares whole too).
    pub changed_count: u64,
}

/// What `diff` prints under `--json`: a list's envelope (`V`, its rows
/// perhaps narrowed by `--fields`), then the totals.
#[derive(Debug, serde::Serialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct DiffEnvelope<'a, V> {
    /// The list's fields.
    #[serde(flatten)]
    pub envelope: &'a V,
    /// The totals.
    pub diff: &'a DiffTotals,
}

/// `schema`'s answer: the index's layout, each table's rows in the saved
/// scan read (null without one), and the verbs whose envelope
/// `schema --envelope <verb>` prints.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct SchemaInfo {
    /// The index version this duscope writes.
    pub index_version: &'static str,
    /// Each table, in the layout's order.
    pub tables: Vec<TableEntry>,
    /// The verbs with an envelope schema, and `error`.
    pub envelopes: Vec<&'static str>,
}

/// A table of the index.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct TableEntry {
    /// Its name.
    pub name: String,
    /// Its columns, in order.
    pub columns: Vec<ColumnEntry>,
    /// Its rows in the saved scan; null when none was read, or its file
    /// has no such table.
    pub row_count: Option<u64>,
}

/// A column of a table.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct ColumnEntry {
    /// Its name.
    pub name: String,
    /// Its declared type (`INTEGER`, `TEXT`, `BLOB`); empty for none.
    pub r#type: String,
    /// Whether it may be null.
    pub nullable: bool,
    /// The platforms that fill it: `macos`, `linux`.
    #[cfg_attr(test, schemars(extend("items" = {"type": "string", "enum": values::PLATFORMS})))]
    pub platforms: Vec<&'static str>,
}

/// What `query` was asked.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct SqlQueryBlock {
    /// The verb, `query`.
    pub command: &'static str,
    /// The entry `:root_id` names, absolute.
    pub path: String,
    /// The statement, as given.
    pub sql: String,
    /// The most rows the answer holds: 25 (or `max_rows`, if lower) for a
    /// statement without a `LIMIT` of its own, `max_rows` for one with.
    pub limit: u64,
    /// `--max-rows`.
    pub max_rows: u64,
    /// `--timeout`, in milliseconds.
    pub timeout_ms: u64,
}

/// What `query --sql` answers: the statement's columns, and its rows up to
/// the limit.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct SqlResult {
    /// The columns' names, in order.
    pub columns: Vec<String>,
    /// The rows, a value per column.
    pub rows: Vec<Vec<SqlValue>>,
    /// The rows returned.
    pub row_count: u64,
    /// Whether the statement had rows past the limit.
    pub truncated: bool,
    /// The most rows the answer could hold, which the query block says.
    #[serde(skip)]
    pub limit: u64,
    /// Whether the statement had a `LIMIT` of its own.
    #[serde(skip)]
    pub limited: bool,
}

/// A value of a row `query --sql` returns: JSON's own for a null, a
/// number or a string; `{"$bytes": "<base64>"}` for a BLOB, or text that
/// is not UTF-8. Names are BLOBs in the index: `CAST(name AS TEXT)` or
/// `path(id)` reads them as text.
#[derive(Debug, Clone, PartialEq)]
pub enum SqlValue {
    /// NULL.
    Null,
    /// An integer.
    Integer(i64),
    /// A floating-point number; a non-finite one is written as null.
    Real(f64),
    /// UTF-8 text.
    Text(String),
    /// A BLOB, or text that is not UTF-8.
    Bytes(Vec<u8>),
}

impl serde::Serialize for SqlValue {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        match self {
            SqlValue::Integer(n) => serializer.serialize_i64(*n),
            SqlValue::Real(x) if x.is_finite() => serializer.serialize_f64(*x),
            SqlValue::Null | SqlValue::Real(_) => serializer.serialize_none(),
            SqlValue::Text(text) => serializer.serialize_str(text),
            SqlValue::Bytes(bytes) => {
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("$bytes", &base64(bytes))?;
                map.end()
            }
        }
    }
}

// The schema of a value as `Serialize` above writes it. A `$bytes` object
// is named through `propertyNames`, not `properties`: property names in a
// schema keep to MCP's `[A-Za-z0-9_.-]`.
#[cfg(test)]
impl schemars::JsonSchema for SqlValue {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "SqlValue".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "description": "A value of a row: null, a number, a string, or {\"$bytes\": base64} for a BLOB or text that is not UTF-8.",
            "anyOf": [
                {"type": ["null", "integer", "number", "string"]},
                {
                    "type": "object",
                    "propertyNames": {"const": "$bytes"},
                    "additionalProperties": {"type": "string", "contentEncoding": "base64"},
                    "minProperties": 1,
                    "maxProperties": 1
                }
            ]
        })
    }
}

/// The fields of a row as `--fields` names them, in the order the row
/// writes them.
pub trait Fields {
    /// Every field the row writes; `name_raw` is among them, though a row
    /// carries it only for a name that is not UTF-8.
    const NAMES: &'static [&'static str];
}

impl Fields for TopEntry {
    const NAMES: &'static [&'static str] = &[
        "path",
        "name",
        "name_raw",
        "kind",
        "depth",
        "allocated_bytes",
        "apparent_bytes",
        "pct_of_root",
        "file_count",
        "dir_count",
        "max_mtime",
        "via",
        "flags",
    ];
}

impl Fields for TreeEntry {
    const NAMES: &'static [&'static str] = &[
        "path",
        "name",
        "name_raw",
        "kind",
        "depth",
        "allocated_bytes",
        "apparent_bytes",
        "pct_of_root",
        "pct_of_parent",
        "file_count",
        "dir_count",
        "max_mtime",
        "flags",
    ];
}

impl Fields for ByTypeEntry {
    const NAMES: &'static [&'static str] = &[
        "class",
        "allocated_bytes",
        "apparent_bytes",
        "pct_of_root",
        "file_count",
        "largest",
    ];
}

impl Fields for StaleEntry {
    const NAMES: &'static [&'static str] = &[
        "path",
        "name",
        "name_raw",
        "kind",
        "allocated_bytes",
        "apparent_bytes",
        "mtime",
        "ctime",
        "atime",
        "nlink",
        "flags",
    ];
}

impl Fields for CacheEntry {
    const NAMES: &'static [&'static str] = &[
        "path",
        "name",
        "name_raw",
        "rule",
        "regenerable_by",
        "project_root",
        "confirmed",
        "cachedir_tag",
        "allocated_bytes",
        "apparent_bytes",
        "pct_of_root",
        "file_count",
        "max_mtime",
    ];
}

impl Fields for ProjectEntry {
    const NAMES: &'static [&'static str] = &[
        "project_root",
        "allocated_bytes",
        "apparent_bytes",
        "cache_count",
        "max_mtime",
        "caches",
    ];
}

impl Fields for SavedScanEntry {
    const NAMES: &'static [&'static str] = &[
        "id",
        "root",
        "started_at",
        "finished_at",
        "attrs",
        "entry_count",
        "allocated_bytes",
        "apparent_bytes",
        "file_count",
        "dir_count",
        "parent_scan_id",
        "source",
        "index_version",
        "index_bytes",
        "index_path",
    ];
}

impl Fields for PrunedEntry {
    const NAMES: &'static [&'static str] = &["path", "kind", "id", "size_bytes"];
}

impl Fields for CheckedEntry {
    const NAMES: &'static [&'static str] = &["id", "index_path", "quick_check"];
}

impl Fields for DiffEntry {
    const NAMES: &'static [&'static str] = &[
        "path",
        "name",
        "name_raw",
        "change",
        "kind",
        "old_bytes",
        "new_bytes",
        "delta_bytes",
        "collapsed_count",
    ];
}

impl Fields for ScanErrorEntry {
    const NAMES: &'static [&'static str] = &["path", "kind", "errno", "message"];
}

/// What stdout gets in place of an envelope longer than `--max-chars`: the
/// file the whole envelope went to, and its first rows.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct SpilledEnvelope<T> {
    /// Whether the command succeeded.
    pub ok: bool,
    /// The version of the envelope's shape.
    pub schema_version: &'static str,
    /// The program.
    pub tool: Tool,
    /// Where the envelope went.
    pub spilled: Spill,
    /// The envelope's first rows, at most five.
    pub preview: Vec<T>,
    /// The envelope's warnings, and `spilled`.
    pub warnings: Vec<Warning>,
}

/// An envelope written to a file.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Spill {
    /// The file, absolute.
    pub path: String,
    /// The envelope's rows (`returned_count`).
    pub rows: u64,
    /// The file's size.
    pub bytes: u64,
}

#[cfg(test)]
mod tests {
    use super::{base64, name_fields, values};

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

    /// A row's fields as it writes them, in order, against its
    /// [`super::Fields::NAMES`].
    fn assert_names<T: serde::Serialize + super::Fields>(row: &T) {
        let text = serde_json::to_string(row).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        let mut keys: Vec<&String> = value.as_object().unwrap().keys().collect();
        let mut names: Vec<&str> = T::NAMES.to_vec();
        keys.sort();
        names.sort_unstable();
        assert_eq!(keys, names, "{text}");
        let at: Vec<usize> = T::NAMES
            .iter()
            .map(|name| text.find(&format!("\"{name}\":")).unwrap())
            .collect();
        assert!(at.windows(2).all(|w| w[0] < w[1]), "out of order: {text}");
    }

    #[test]
    // One sample of each row, every field spelled.
    #[allow(clippy::too_many_lines)]
    fn p2_qry_02_every_row_names_its_fields_in_order() {
        use super::{
            ByTypeEntry, CacheEntry, CheckedEntry, ProjectEntry, PrunedEntry, SavedScanEntry,
            ScanErrorEntry, StaleEntry, TopEntry, TreeEntry,
        };
        let raw = || Some("bmFtZQ==".to_owned());
        assert_names(&TopEntry {
            path: "/a".into(),
            path_bytes: Vec::new(),
            name: "a".into(),
            name_raw: raw(),
            kind: "dir",
            depth: 1,
            allocated_bytes: 1,
            apparent_bytes: 1,
            pct_of_root: 1.0,
            file_count: 1,
            dir_count: 1,
            max_mtime: "t".into(),
            via: Vec::new(),
            flags: Vec::new(),
        });
        assert_names(&TreeEntry {
            path: "/a".into(),
            path_bytes: Vec::new(),
            name: "a".into(),
            name_raw: raw(),
            kind: "dir",
            depth: 1,
            allocated_bytes: 1,
            apparent_bytes: 1,
            pct_of_root: 1.0,
            pct_of_parent: 1.0,
            file_count: 1,
            dir_count: 1,
            max_mtime: "t".into(),
            flags: Vec::new(),
        });
        assert_names(&ByTypeEntry {
            class: "code",
            allocated_bytes: 1,
            apparent_bytes: 1,
            pct_of_root: 1.0,
            file_count: 1,
            largest: Vec::new(),
        });
        assert_names(&StaleEntry {
            path: "/a".into(),
            path_bytes: Vec::new(),
            name: "a".into(),
            name_raw: raw(),
            kind: "file",
            allocated_bytes: 1,
            apparent_bytes: 1,
            mtime: "t".into(),
            ctime: "t".into(),
            atime: Some("t".into()),
            nlink: 1,
            flags: Vec::new(),
        });
        assert_names(&CacheEntry {
            path: "/a".into(),
            path_bytes: Vec::new(),
            name: "a".into(),
            name_raw: raw(),
            rule: "node_modules".into(),
            regenerable_by: "npm install",
            project_root: Some("/".into()),
            project_bytes: Vec::new(),
            confirmed: true,
            cachedir_tag: false,
            allocated_bytes: 1,
            apparent_bytes: 1,
            pct_of_root: 1.0,
            file_count: 1,
            max_mtime: "t".into(),
        });
        assert_names(&ProjectEntry {
            project_root: Some("/".into()),
            path_bytes: Vec::new(),
            allocated_bytes: 1,
            apparent_bytes: 1,
            cache_count: 1,
            max_mtime: "t".into(),
            caches: Vec::new(),
        });
        assert_names(&SavedScanEntry {
            id: "i".into(),
            root: "/".into(),
            started_at: "t".into(),
            finished_at: "t".into(),
            attrs: "basic",
            entry_count: 1,
            allocated_bytes: 1,
            apparent_bytes: 1,
            file_count: 1,
            dir_count: 1,
            parent_scan_id: None,
            source: "scan".into(),
            index_version: "2.0".into(),
            index_bytes: 1,
            index_path: "/i.db".into(),
            short_id: "i".into(),
        });
        assert_names(&PrunedEntry {
            path: "/p".into(),
            kind: "spill",
            id: None,
            size_bytes: 1,
        });
        assert_names(&CheckedEntry {
            id: "i".into(),
            index_path: "/i.db".into(),
            quick_check: "ok",
        });
        assert_names(&ScanErrorEntry {
            path: "/e".into(),
            path_bytes: Vec::new(),
            kind: "directory",
            errno: 13,
            message: "denied".into(),
        });
    }

    #[test]
    fn p2_cli_01_the_schemas_values_are_the_ones_the_code_writes() {
        use duscope_walk::{Flags, Kind, Skip};

        use crate::query::{Node, flag_names, kind_name};
        use crate::rules::Regenerable;

        for kind in [
            Kind::File,
            Kind::Dir,
            Kind::Symlink,
            Kind::Socket,
            Kind::Fifo,
            Kind::Char,
            Kind::Block,
            Kind::Unknown,
        ] {
            assert!(values::KINDS.contains(&kind_name(kind)), "{kind:?}");
        }
        let skips = [
            Skip::Firmlinked,
            Skip::Kernel,
            Skip::Network,
            Skip::OneFileSystem,
            Skip::BindCycle,
            Skip::Excluded,
            Skip::ExcludedCache,
        ];
        let names: Vec<&str> = skips.iter().map(|s| s.as_str()).collect();
        assert_eq!(names, values::SKIP_REASONS);
        assert!(values::EXCLUDE_REASONS.iter().all(|r| names.contains(r)));
        for command in values::REGENERABLE_BY {
            let rule: Regenerable = serde_json::from_value(command.into()).unwrap();
            assert_eq!(rule.as_str(), command);
        }
        // A file with every flag, charged to another link.
        let node = Node {
            id: 1,
            parent: 0,
            depth: 1,
            kind: Kind::File,
            allocated: 0,
            apparent: 0,
            file_count: 1,
            dir_count: 0,
            mtime_ns: 0,
            max_mtime_ns: 0,
            ctime_ns: 0,
            atime_ns: None,
            btime_ns: None,
            mode: None,
            nlink: 2,
            charged: false,
            linked_away: false,
            flags: Flags::all(),
            upper_bound: true,
            unreadable: true,
            files_end: 1,
            last_id: 1,
        };
        let mut written = flag_names(&node);
        let mut listed = values::FLAGS.to_vec();
        written.sort_unstable();
        listed.sort_unstable();
        assert_eq!(written, listed);
    }
}
