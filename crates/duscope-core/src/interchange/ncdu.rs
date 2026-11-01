//! ncdu's JSON export format, major 1, minor 2 (dev.yorhel.nl/ncdu/jsonfmt):
//! `[1, 2, {metadata}, [{root's info}, child, …]]`, a directory an array
//! whose first element is its info object and a file an info object.
//!
//! - **Sizes** are each entry's own (`asize` apparent, `dsize` on disk), a
//!   directory's too, as ncdu writes them; ncdu adds them up on import.
//! - **A hard-linked file** carries `ino`, `hlnkc` and `nlink`, and its full
//!   sizes at every link: ncdu counts each inode once.
//! - **Names** are written as the filesystem gave them, as ncdu writes
//!   them: quotes, backslashes and control characters escaped, other bytes
//!   as they are, so a name that is not UTF-8 survives the round trip
//!   (strict JSON readers need UTF-8 names).
//! - **A directory the scan did not enter** is an info object with
//!   `excluded` (`pattern`, `otherfs`, `kernfs`, `frmlink`); one it could
//!   not read has `read_error`.
//! - **`dev`** is on the root and on each directory whose device differs
//!   from its parent's; `mtime` (whole seconds) on every entry, and `uid`,
//!   `gid` and `mode` where the scan collected them.

use std::fmt::Write as _;
use std::io::Write;

use super::{ExportError, Row, RowKind, seconds, walk};
use crate::index::Index;

/// What the metadata object says.
#[derive(Debug, Clone)]
pub struct NcduOpts {
    /// The writing program's version.
    pub progver: &'static str,
    /// When the export was written, Unix seconds.
    pub timestamp: i64,
}

/// Writes the entries from `base` down to `out`.
///
/// # Errors
///
/// When the index cannot be read or `out` written.
pub fn write(
    index: &Index,
    base: u32,
    opts: &NcduOpts,
    out: &mut dyn Write,
) -> Result<(), ExportError> {
    writeln!(
        out,
        "[1,2,{{\"progname\":\"duscope\",\"progver\":\"{}\",\"timestamp\":{}}},",
        opts.progver, opts.timestamp
    )?;
    // The open directories: their last id and device.
    let mut open: Vec<(u32, u64)> = Vec::new();
    let mut first = true;
    walk(index, base, |row| {
        while open.last().is_some_and(|&(last, _)| last < row.id) {
            out.write_all(b"]")?;
            open.pop();
        }
        if !first {
            out.write_all(b",\n")?;
        }
        first = false;
        let parent_dev = open.last().map(|&(_, dev)| dev);
        let info = info(row, parent_dev);
        match row.dir {
            Some(dir) if row.excluded.is_none() => {
                out.write_all(b"[")?;
                out.write_all(&info)?;
                open.push((dir.last_id, dir.dev));
            }
            _ => out.write_all(&info)?,
        }
        Ok(())
    })?;
    for _ in &open {
        out.write_all(b"]")?;
    }
    out.write_all(b"]\n")?;
    Ok(())
}

/// A row's info object; `parent_dev` is the open directory's device.
fn info(row: &Row, parent_dev: Option<u64>) -> Vec<u8> {
    let mut out = Vec::with_capacity(96);
    out.extend_from_slice(b"{\"name\":");
    string(&mut out, &row.name);
    let mut fields = String::new();
    let _ = write!(
        fields,
        ",\"asize\":{},\"dsize\":{}",
        row.apparent, row.allocated
    );
    if let Some(dir) = row.dir
        && parent_dev != Some(dir.dev)
    {
        let _ = write!(fields, ",\"dev\":{}", dir.dev);
    }
    if row.nlink > 1 {
        if let Some((_, ino)) = row.inode {
            let _ = write!(fields, ",\"ino\":{ino}");
        }
        let _ = write!(fields, ",\"hlnkc\":true,\"nlink\":{}", row.nlink);
    }
    if matches!(row.kind, RowKind::Symlink | RowKind::Other) {
        fields.push_str(",\"notreg\":true");
    }
    if row.dir.is_some_and(|d| d.unreadable) {
        fields.push_str(",\"read_error\":true");
    }
    if let Some(why) = row.excluded {
        let _ = write!(fields, ",\"excluded\":\"{why}\"");
    }
    if let (Some(uid), Some(gid), Some(mode)) = (row.uid, row.gid, row.mode) {
        let _ = write!(
            fields,
            ",\"uid\":{uid},\"gid\":{gid},\"mode\":{}",
            mode & 0xffff
        );
    }
    let mtime = seconds(row.mtime_ns);
    if mtime >= 0 {
        let _ = write!(fields, ",\"mtime\":{mtime}");
    }
    out.extend_from_slice(fields.as_bytes());
    out.push(b'}');
    out
}

/// `bytes` as a JSON string, as ncdu writes one: `"` and `\` escaped,
/// control characters as `\u00XX`, every other byte as it is.
fn string(out: &mut Vec<u8>, bytes: &[u8]) {
    out.push(b'"');
    for &b in bytes {
        match b {
            b'"' => out.extend_from_slice(b"\\\""),
            b'\\' => out.extend_from_slice(b"\\\\"),
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\t' => out.extend_from_slice(b"\\t"),
            0..0x20 | 0x7f => {
                let _ = std::io::Write::write_fmt(out, format_args!("\\u{b:04x}"));
            }
            _ => out.push(b),
        }
    }
    out.push(b'"');
}

#[cfg(test)]
mod tests {
    use super::string;

    #[test]
    fn p2_cli_02_ncdu_names_keep_their_bytes() {
        let mut out = Vec::new();
        string(&mut out, b"a\"b\\c\nd\x01\xffe");
        assert_eq!(out, b"\"a\\\"b\\\\c\\nd\\u0001\xffe\"");
    }
}
