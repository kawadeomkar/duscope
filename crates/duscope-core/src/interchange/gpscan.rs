//! GrandPerspective's scan dump, format 7: gzip-compressed XML,
//! `GrandPerspectiveScanDump` → `ScanInfo` → nested `Folder` and `File`.
//!
//! - **The measure** is `physical` (bytes on disk) or `logical` (apparent
//!   bytes), as GrandPerspective names them. A file whose bytes count at
//!   another entry is left out, so the files add up to duscope's folder
//!   totals: under both, a later link of a hard-linked file, which
//!   GrandPerspective's own scan counts once too; under `physical`, also a
//!   clone whose shared blocks are charged to another member.
//! - **Zero-size entries are left out**, a file of no bytes and a folder
//!   with none under it, as GrandPerspective drops them; a folder's files
//!   come before its folders, the order the index already has.
//! - **Times** are RFC 3339 with a `Z`, whole seconds: `created` is the
//!   creation time, or the modification time where the scan has none;
//!   `accessed` is left out where the scan did not collect it.
//! - **Names** are UTF-8 (a name that is not is read lossily), with `&`,
//!   `<` and `"` escaped, tab, line feed and carriage return written as a
//!   space and other control characters as `?`, as GrandPerspective writes
//!   them.
//! - **The root folder** is named by its path below its volume's mount
//!   point, which `ScanInfo` gives with the volume's size and free space
//!   at the scan's start. `flags` marks a hard-linked file (2).

// GrandPerspective is a name, not code.
#![allow(clippy::doc_markdown)]

use std::io::Write;

use flate2::Compression;
use flate2::write::GzEncoder;

use super::{Row, walk};
use crate::CoreError;
use crate::index::Index;
use crate::query::timegrammar::iso8601;

/// GrandPerspective's flag for a hard-linked file.
const HARDLINKED: u32 = 2;

/// What `ScanInfo` says, and the measure.
#[derive(Debug, Clone)]
pub struct GpscanOpts {
    /// Apparent bytes (`logical`) rather than bytes on disk (`physical`).
    pub apparent: bool,
    /// The mount point of the root's volume.
    pub volume_path: Vec<u8>,
    /// The volume's size.
    pub volume_size: u64,
    /// Its free space at the scan's start.
    pub free_space: u64,
    /// When the scan was taken, ns.
    pub scan_time_ns: i64,
    /// The writing program's version, for `appVersion`.
    pub app_version: &'static str,
}

/// Writes the entries from `base` down to `out`, gzipped.
///
/// # Errors
///
/// When the index cannot be read or `out` written.
pub fn write(
    index: &Index,
    base: u32,
    opts: &GpscanOpts,
    out: &mut dyn Write,
) -> Result<(), CoreError> {
    let mut gz = GzEncoder::new(out, Compression::default());
    write!(
        gz,
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <GrandPerspectiveScanDump appVersion=\"{}\" formatVersion=\"7\">\n\
         <ScanInfo volumePath=\"{}\" volumeSize=\"{}\" freeSpace=\"{}\" scanTime=\"{}\" \
         fileSizeMeasure=\"{}\">\n",
        opts.app_version,
        escape(&opts.volume_path),
        opts.volume_size,
        opts.free_space,
        iso8601(opts.scan_time_ns),
        if opts.apparent { "logical" } else { "physical" },
    )?;
    let size = |row: &Row| match (row.dir, opts.apparent) {
        (Some(dir), true) => dir.cum_apparent,
        (Some(dir), false) => dir.cum_allocated,
        (None, true) => row.apparent,
        (None, false) => row.allocated,
    };
    let mut open: Vec<u32> = Vec::new();
    // Inside a folder left out, until its last id.
    let mut skip_until: Option<u32> = None;
    walk(index, base, |row| {
        if skip_until.is_some_and(|last| row.id <= last) {
            return Ok(());
        }
        skip_until = None;
        while open.last().is_some_and(|&last| last < row.id) {
            gz.write_all(b"</Folder>\n")?;
            open.pop();
        }
        if let Some(dir) = row.dir {
            if size(row) == 0 {
                skip_until = Some(dir.last_id);
                return Ok(());
            }
            let name = if row.id == base {
                root_name(&row.name, &opts.volume_path)
            } else {
                row.name.clone()
            };
            write!(gz, "<Folder name=\"{}\"", escape(&name))?;
            times(&mut gz, row)?;
            gz.write_all(b">\n")?;
            open.push(dir.last_id);
            return Ok(());
        }
        let elsewhere = if opts.apparent {
            row.later_link
        } else {
            !row.charged
        };
        if size(row) == 0 || elsewhere {
            return Ok(());
        }
        write!(
            gz,
            "<File name=\"{}\" size=\"{}\"",
            escape(&row.name),
            size(row)
        )?;
        if row.nlink > 1 {
            write!(gz, " flags=\"{HARDLINKED}\"")?;
        }
        times(&mut gz, row)?;
        gz.write_all(b"/>\n")?;
        Ok(())
    })?;
    for _ in &open {
        gz.write_all(b"</Folder>\n")?;
    }
    gz.write_all(b"</ScanInfo>\n</GrandPerspectiveScanDump>\n")?;
    gz.finish()?;
    Ok(())
}

/// A row's `created`, `modified` and `accessed` attributes.
fn times(out: &mut impl Write, row: &Row) -> std::io::Result<()> {
    let created = row.btime_ns.unwrap_or(row.mtime_ns);
    write!(
        out,
        " created=\"{}\" modified=\"{}\"",
        iso8601(created),
        iso8601(row.mtime_ns)
    )?;
    if let Some(atime) = row.atime_ns {
        write!(out, " accessed=\"{}\"", iso8601(atime))?;
    }
    Ok(())
}

/// The root folder's name: its path below the volume's mount point, or
/// its last component at the mount point itself.
fn root_name(path: &[u8], volume: &[u8]) -> Vec<u8> {
    let below = path
        .strip_prefix(volume)
        .map(|rest| rest.strip_prefix(b"/").unwrap_or(rest))
        .filter(|rest| !rest.is_empty());
    match below {
        Some(rest) => rest.to_vec(),
        None => path
            .rsplit(|&b| b == b'/')
            .find(|part| !part.is_empty())
            .unwrap_or(path)
            .to_vec(),
    }
}

/// `bytes` as GrandPerspective writes an attribute value.
fn escape(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '"' => out.push_str("&quot;"),
            '\t' | '\n' | '\r' => out.push(' '),
            c if c < ' ' || c == '\u{7f}' => out.push('?'),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{escape, root_name};

    #[test]
    fn p2_cli_02_gpscan_names_are_escaped_as_grandperspective_writes_them() {
        assert_eq!(escape(b"a&b<c\"d\te\x01f>"), "a&amp;b&lt;c&quot;d e?f>");
        assert_eq!(escape(b"caf\xc3\xa9 \xff"), "caf\u{e9} \u{fffd}");
        assert_eq!(root_name(b"/Users/a/b", b"/"), b"Users/a/b");
        assert_eq!(root_name(b"/Volumes/X/b", b"/Volumes/X"), b"b");
        assert_eq!(root_name(b"/Volumes/X", b"/Volumes/X"), b"X");
        assert_eq!(root_name(b"/", b"/"), b"/");
    }
}
