//! `rescan`: a saved scan's root scanned again with the options it was
//! scanned with, which `scan_meta.options` records. The new scan is a new
//! index (snapshots are never edited) whose `parent_scan_id` names the old
//! one; the old one stays until `scans --prune` removes it. Phase 5 adds
//! the incremental path; until then every rescan is a full one.

use std::path::PathBuf;

use duscope_walk::Attrs;
use serde_json::Value;

use crate::CoreError;
use crate::index::Index;

/// The options a scan was taken with, as a rescan takes them again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RescanOpts {
    /// Whether clones were counted.
    pub clones: bool,
    /// Whether the scan stayed on the root's filesystem (`-x`).
    pub one_file_system: bool,
    /// Whether it entered network filesystems.
    pub include_network: bool,
    /// Whether it followed symbolic links.
    pub follow_symlinks: bool,
    /// The exclude patterns, `--exclude`'s and the configuration's then.
    pub excludes: Vec<String>,
    /// The files `--exclude-from` named.
    pub exclude_files: Vec<PathBuf>,
    /// The files `--ignore-from` named.
    pub ignore_files: Vec<PathBuf>,
    /// Whether `--exclude-caches` left the caches out.
    pub exclude_caches: bool,
    /// The worker threads asked for; 0 for the platform's default.
    pub threads: usize,
    /// Whether sizes were shown apparent.
    pub apparent: bool,
    /// The attributes collected.
    pub attrs: Attrs,
}

/// The options `index`'s scan was taken with. Options that are not the
/// JSON object a scan writes are [`CoreError::IndexCorrupt`]; a key a scan
/// of an older build did not write reads as its default.
pub fn options(index: &Index) -> Result<RescanOpts, CoreError> {
    let corrupt = |message: String| CoreError::IndexCorrupt {
        file: index.file().display().to_string(),
        message,
    };
    let value: Value = serde_json::from_str(&index.meta().options)
        .map_err(|e| corrupt(format!("scan_meta.options is not JSON: {e}")))?;
    let Value::Object(map) = value else {
        return Err(corrupt("scan_meta.options is not an object".to_owned()));
    };
    let flag = |key: &str, default: bool| map.get(key).and_then(Value::as_bool).unwrap_or(default);
    let strings = |key: &str| -> Vec<String> {
        map.get(key)
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    };
    let paths = |key: &str| strings(key).into_iter().map(PathBuf::from).collect();
    Ok(RescanOpts {
        clones: flag("clones", true),
        one_file_system: flag("one_file_system", false),
        include_network: flag("include_network", false),
        follow_symlinks: flag("follow_symlinks", false),
        excludes: strings("excludes"),
        exclude_files: paths("exclude_files"),
        ignore_files: paths("ignore_files"),
        exclude_caches: flag("exclude_caches", false),
        threads: map
            .get("threads")
            .and_then(Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(0),
        apparent: map.get("size_kind").and_then(Value::as_str) == Some("apparent"),
        attrs: index.attrs(),
    })
}
