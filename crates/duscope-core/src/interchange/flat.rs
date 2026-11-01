//! A flat row an entry, for `jq` and pandas: NDJSON (an object a line) or
//! CSV (a heading line, then a record a line, RFC 4180 quoting).
//!
//! The fields, in order: `path` (absolute), `kind`, `depth` (below the
//! exported entry), `allocated_bytes` and `apparent_bytes` (the entry's
//! own), `charged` (whether its bytes are in the totals: false for a later
//! hard link or a clone member charged elsewhere), `nlink`, `mtime` (RFC
//! 3339), and for a directory `subtree_allocated_bytes` and
//! `subtree_apparent_bytes` (null, or empty in CSV, for anything else).
//! NDJSON adds `path_raw` (base64) for a path that is not UTF-8, as the
//! envelope does for names; CSV writes such a path lossily.

use std::io::Write;

use serde::Serialize;

use super::{ExportError, Paths, Row, RowKind, walk};
use crate::envelope::base64;
use crate::index::Index;
use crate::query::timegrammar::iso8601;

/// The fields, in the order the rows write them.
pub const FIELDS: [&str; 10] = [
    "path",
    "kind",
    "depth",
    "allocated_bytes",
    "apparent_bytes",
    "charged",
    "nlink",
    "mtime",
    "subtree_allocated_bytes",
    "subtree_apparent_bytes",
];

/// NDJSON or CSV.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flat {
    /// A JSON object a line.
    Ndjson,
    /// A heading line, then a record a line.
    Csv,
}

/// A row as the flat formats write it.
#[derive(Serialize)]
struct FlatRow {
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    path_raw: Option<String>,
    kind: &'static str,
    depth: u32,
    allocated_bytes: u64,
    apparent_bytes: u64,
    charged: bool,
    nlink: u64,
    mtime: String,
    subtree_allocated_bytes: Option<u64>,
    subtree_apparent_bytes: Option<u64>,
}

/// Writes the entries from `base` down to `out`.
///
/// # Errors
///
/// When the index cannot be read or `out` written.
pub fn write(index: &Index, base: u32, flat: Flat, out: &mut dyn Write) -> Result<(), ExportError> {
    if flat == Flat::Csv {
        writeln!(out, "{}", FIELDS.join(","))?;
    }
    let mut paths = Paths::default();
    walk(index, base, |row| {
        let path = paths.path(row);
        let (path, path_raw) = match String::from_utf8(path) {
            Ok(path) => (path, None),
            Err(e) => (
                String::from_utf8_lossy(e.as_bytes()).into_owned(),
                Some(base64(e.as_bytes())),
            ),
        };
        let row = FlatRow {
            path,
            path_raw,
            kind: kind(row),
            depth: row.depth,
            allocated_bytes: row.allocated,
            apparent_bytes: row.apparent,
            charged: row.charged,
            nlink: row.nlink,
            mtime: iso8601(row.mtime_ns),
            subtree_allocated_bytes: row.dir.map(|d| d.cum_allocated),
            subtree_apparent_bytes: row.dir.map(|d| d.cum_apparent),
        };
        match flat {
            Flat::Ndjson => {
                serde_json::to_writer(&mut *out, &row).map_err(std::io::Error::other)?;
                out.write_all(b"\n")?;
            }
            Flat::Csv => {
                let optional = |n: Option<u64>| n.map(|n| n.to_string()).unwrap_or_default();
                let fields = [
                    csv(&row.path),
                    row.kind.to_owned(),
                    row.depth.to_string(),
                    row.allocated_bytes.to_string(),
                    row.apparent_bytes.to_string(),
                    row.charged.to_string(),
                    row.nlink.to_string(),
                    row.mtime,
                    optional(row.subtree_allocated_bytes),
                    optional(row.subtree_apparent_bytes),
                ];
                writeln!(out, "{}", fields.join(","))?;
            }
        }
        Ok(())
    })
}

fn kind(row: &Row) -> &'static str {
    match row.kind {
        RowKind::Dir => "dir",
        RowKind::File => "file",
        RowKind::Symlink => "symlink",
        RowKind::Other => "other",
    }
}

/// A CSV field: quoted, with its quotes doubled, when it holds a comma, a
/// quote or a line break.
fn csv(field: &str) -> String {
    if field.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::csv;

    #[test]
    fn p2_cli_02_csv_fields_are_quoted_as_rfc_4180_says() {
        assert_eq!(csv("/a/b"), "/a/b");
        assert_eq!(csv("/a,b"), "\"/a,b\"");
        assert_eq!(csv("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(csv("line\nbreak"), "\"line\nbreak\"");
    }
}
