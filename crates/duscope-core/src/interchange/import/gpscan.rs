//! GrandPerspective's scan dump, as `export --format gpscan` writes it
//! (format 7; the caller has taken the gzip off): `GrandPerspectiveScanDump`
//! → `ScanInfo` → nested `Folder` and `File` elements.
//!
//! - **The root** is the top folder's name joined onto `ScanInfo`'s
//!   `volumePath`, as GrandPerspective names it below its volume.
//! - **`size`** is the measure `fileSizeMeasure` names, and stands for both
//!   sizes; folders have none of their own.
//! - **`modified`** (RFC 3339) is the modification time; `created` and
//!   `accessed` are not kept, an import being a basic scan. `scanTime` is
//!   when the tree was read.
//! - **Other elements** are passed over with what is in them.

// GrandPerspective is a name, not code.
#![allow(clippy::doc_markdown)]

use std::io::BufRead;

use duscope_walk::Kind;
use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, XmlVersion};

use super::{Builder, ImportFormat, Imported, Item, invalid, read_error};
use crate::CoreError;

/// Reads the tree.
pub(super) fn read(input: impl BufRead) -> Result<Imported, CoreError> {
    let mut reader = Reader::from_reader(input);
    let mut buf = Vec::new();
    let mut tree = Builder::new();
    let mut volume: Option<Vec<u8>> = None;
    let mut scanned_at = None;
    let mut dump = false;
    // Inside an element passed over, as deep as this.
    let mut skipping = 0usize;
    loop {
        buf.clear();
        let event = reader
            .read_event_into(&mut buf)
            .map_err(|e| xml_error(&e, reader.error_position()))?;
        if skipping > 0 {
            match event {
                Event::Start(_) => skipping += 1,
                Event::End(_) => skipping -= 1,
                Event::Eof => return Err(invalid("the file ends inside an element")),
                _ => {}
            }
            continue;
        }
        match event {
            Event::Start(e) => match e.name().as_ref() {
                "GrandPerspectiveScanDump" => dump = true,
                "ScanInfo" if dump => {
                    let info = attributes(&e, &["volumePath", "scanTime"])?;
                    volume.clone_from(&info[0]);
                    scanned_at = info[1].as_deref().map(time).transpose()?;
                }
                "Folder" if dump => {
                    let item = folder(&e, &tree, volume.as_deref())?;
                    tree.open(&item)?;
                }
                "File" if dump && tree.depth() > 0 => {
                    tree.leaf(&file(&e)?)?;
                    skipping = 1;
                }
                _ => skipping = 1,
            },
            Event::Empty(e) => match e.name().as_ref() {
                "Folder" if dump => {
                    let item = folder(&e, &tree, volume.as_deref())?;
                    if tree.depth() == 0 {
                        // A root with nothing under it.
                        tree.open(&item)?;
                        tree.close()?;
                    } else {
                        tree.leaf(&item)?;
                    }
                }
                "File" if dump && tree.depth() > 0 => tree.leaf(&file(&e)?)?,
                "File" if dump => return Err(invalid("a File stands outside every Folder")),
                _ => {}
            },
            Event::End(e) => {
                if e.name().as_ref() == "Folder" {
                    tree.close()?;
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if !dump {
        return Err(invalid("the file is not a GrandPerspectiveScanDump"));
    }
    tree.finish(ImportFormat::Gpscan, scanned_at)
}

/// A folder; the top one named by its path below the volume.
fn folder(e: &BytesStart<'_>, tree: &Builder, volume: Option<&[u8]>) -> Result<Item, CoreError> {
    let [name, modified] = attributes(e, &["name", "modified"])?;
    let name = name.ok_or_else(|| invalid("a Folder has no name"))?;
    let name = if tree.depth() == 0 {
        root_path(volume, &name)
    } else {
        name
    };
    let mut item = Item::new(name, Kind::Dir);
    item.mtime_ns = modified.as_deref().map(time).transpose()?.unwrap_or(0);
    Ok(item)
}

/// A file.
fn file(e: &BytesStart<'_>) -> Result<Item, CoreError> {
    let [name, size, modified] = attributes(e, &["name", "size", "modified"])?;
    let name = name.ok_or_else(|| invalid("a File has no name"))?;
    let size = match size {
        Some(size) => std::str::from_utf8(&size)
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .ok_or_else(|| invalid("a File's size is not a whole number"))?,
        None => 0,
    };
    let mut item = Item::new(name, Kind::File);
    item.apparent = size;
    item.allocated = size;
    item.mtime_ns = modified.as_deref().map(time).transpose()?.unwrap_or(0);
    Ok(item)
}

/// The values of the attributes named `keys`, in that order, unescaped.
fn attributes<const N: usize>(
    e: &BytesStart<'_>,
    keys: &[&str; N],
) -> Result<[Option<Vec<u8>>; N], CoreError> {
    let mut values: [Option<Vec<u8>>; N] = std::array::from_fn(|_| None);
    for attr in e.attributes() {
        let attr = attr.map_err(|e| invalid(&format!("an attribute is malformed: {e}")))?;
        if let Some(i) = keys.iter().position(|k| *k == attr.key.as_ref()) {
            let value = attr
                .normalized_value(XmlVersion::Implicit1_0)
                .map_err(|e| invalid(&format!("an attribute's value is malformed: {e}")))?;
            values[i] = Some(value.into_owned().into_bytes());
        }
    }
    Ok(values)
}

/// The root's path: `name` below `volume`, or `name` itself when it is
/// absolute or no volume is given.
fn root_path(volume: Option<&[u8]>, name: &[u8]) -> Vec<u8> {
    match volume {
        Some(volume) if !name.starts_with(b"/") => {
            let mut path = volume.to_vec();
            if !name.is_empty() {
                if !path.ends_with(b"/") {
                    path.push(b'/');
                }
                path.extend_from_slice(name);
            }
            path
        }
        _ => name.to_vec(),
    }
}

/// An RFC 3339 time, in Unix nanoseconds.
fn time(text: &[u8]) -> Result<i64, CoreError> {
    let not = || {
        invalid(&format!(
            "{:?} is not an RFC 3339 time, as a .gpscan of format 7 writes",
            String::from_utf8_lossy(&text[..text.len().min(64)])
        ))
    };
    let stamp: jiff::Timestamp = std::str::from_utf8(text)
        .map_err(|_| not())?
        .parse()
        .map_err(|_| not())?;
    i64::try_from(stamp.as_nanosecond()).map_err(|_| not())
}

/// A quick-xml error: the file's, or the XML's.
fn xml_error(error: &quick_xml::Error, at: u64) -> CoreError {
    match error {
        quick_xml::Error::Io(e) => read_error(e),
        e => invalid(&format!("the XML is malformed at byte {at}: {e}")),
    }
}
