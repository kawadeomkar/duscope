//! `rescan`: a saved scan's root scanned again with the options it was
//! scanned with, which `scan_meta.options` records. The new scan is a new
//! index (snapshots are never edited) whose `parent_scan_id` names the old
//! one; the old one stays until `scans --prune` removes it. Phase 5 adds
//! the incremental path; until then every rescan is a full one.

use duscope_walk::Attrs;
use serde::{Deserialize, Serialize};

use crate::CoreError;
use crate::index::Index;

/// The options a scan was taken with, as `scan_meta.options` records them
/// (JSON) and a rescan takes them again: what decides which tree a scan
/// measures. A key a scan of an older build did not write reads as its
/// default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanOptions {
    /// Whether clones were counted.
    #[serde(default = "counted")]
    pub clones: bool,
    /// Whether the scan stayed on the root's filesystem (`-x`).
    #[serde(default)]
    pub one_file_system: bool,
    /// Whether it entered network filesystems.
    #[serde(default)]
    pub include_network: bool,
    /// Whether it followed symbolic links.
    #[serde(default)]
    pub follow_symlinks: bool,
    /// The exclude patterns, `--exclude`'s and the configuration's then.
    #[serde(default)]
    pub excludes: Vec<String>,
    /// The files `--exclude-from` named, absolute.
    #[serde(default)]
    pub exclude_files: Vec<String>,
    /// The files `--ignore-from` named, absolute.
    #[serde(default)]
    pub ignore_files: Vec<String>,
    /// Whether `--exclude-caches` left the caches out.
    #[serde(default)]
    pub exclude_caches: bool,
    /// The cache rules merged over the vendored ones then (the
    /// configuration's `[caches] extra`); null without `--exclude-caches`,
    /// where no cache rule decides what the scan reads.
    #[serde(default)]
    pub caches_extra: Option<String>,
    /// The worker threads asked for; 0 for the platform's default.
    #[serde(default)]
    pub threads: usize,
    /// The size output showed: `allocated` or `apparent`.
    #[serde(default = "allocated")]
    pub size_kind: String,
}

fn counted() -> bool {
    true
}

fn allocated() -> String {
    "allocated".to_owned()
}

impl ScanOptions {
    /// The options as `scan_meta.options` records them.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// The options `index`'s scan was taken with, and the attributes it
/// collected. Options that are not the JSON object a scan writes are
/// [`CoreError::IndexCorrupt`].
pub fn options(index: &Index) -> Result<(ScanOptions, Attrs), CoreError> {
    let options =
        serde_json::from_str(&index.meta().options).map_err(|e| CoreError::IndexCorrupt {
            file: index.file().display().to_string(),
            message: format!("scan_meta.options: {e}"),
        })?;
    Ok((options, index.attrs()))
}

#[cfg(test)]
mod tests {
    use super::ScanOptions;

    #[test]
    fn p2_idx_05_options_read_back_as_written_and_missing_keys_as_defaults() {
        let written = ScanOptions {
            clones: false,
            one_file_system: true,
            include_network: false,
            follow_symlinks: true,
            excludes: vec!["*.tmp".to_owned()],
            exclude_files: vec!["/etc/x".to_owned()],
            ignore_files: Vec::new(),
            exclude_caches: true,
            caches_extra: Some("/home/a/caches.toml".to_owned()),
            threads: 4,
            size_kind: "apparent".to_owned(),
        };
        let read: ScanOptions = serde_json::from_str(&written.to_json()).unwrap();
        assert_eq!(read, written);
        // A scan before exclude_caches and caches_extra were recorded.
        let older: ScanOptions = serde_json::from_str(r#"{"excludes": ["a"]}"#).unwrap();
        assert!(older.clones && !older.exclude_caches && older.caches_extra.is_none());
        assert_eq!(older.size_kind, "allocated");
        assert_eq!(older.excludes, ["a"]);
    }
}
