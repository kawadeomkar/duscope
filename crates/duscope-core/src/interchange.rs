//! Interchange: a saved scan written in the formats other tools read.
//! [`ncdu`] JSON (`ncdu -f`), `GrandPerspective`'s [`gpscan`] (gzip XML,
//! format 7), `QDirStat`'s [`qdirstat`] cache file, and [`flat`] NDJSON and
//! CSV, a row an entry, for `jq` and pandas.
//!
//! Every format is written from [`walk`]: the entries under one, in
//! pre-order (a directory, its files, then its subdirectories, each
//! bytewise, as the index numbers them), read from the index's two entry
//! tables in id order and merged, so an export holds a row in memory at a
//! time.

pub mod flat;
pub mod gpscan;
pub mod ncdu;
pub mod qdirstat;

use std::collections::HashMap;
use std::os::unix::ffi::OsStringExt;

use crate::CoreError;
use crate::index::{EXCLUDED, EXCLUDED_CACHE, Index, LATER_LINK};
use crate::query::TreeView;

/// What a row's kind is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    /// A directory.
    Dir,
    /// A regular file.
    File,
    /// A symbolic link.
    Symlink,
    /// Anything else: a socket, a FIFO, a device.
    Other,
}

/// A directory's facts, beside its row's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirFacts {
    /// The last id in its subtree.
    pub last_id: u32,
    /// Its device.
    pub dev: u64,
    /// Apparent bytes under it, hard links once.
    pub cum_apparent: u64,
    /// Bytes on disk under it, hard links and clones once.
    pub cum_allocated: u64,
    /// Whether the scan could not read it.
    pub unreadable: bool,
}

/// An entry as the exports write it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// Its pre-order id.
    pub id: u32,
    /// How far below the exported entry it is; the exported entry is 0.
    pub depth: u32,
    /// Its name; the exported entry's absolute path for it.
    pub name: Vec<u8>,
    /// What it is.
    pub kind: RowKind,
    /// Its own apparent bytes.
    pub apparent: u64,
    /// Its own bytes on disk.
    pub allocated: u64,
    /// Whether its bytes are in the totals (false for a later hard link,
    /// a clone member charged elsewhere).
    pub charged: bool,
    /// A later link of a hard-linked file: its bytes count at the first.
    pub later_link: bool,
    /// Its modification time, ns.
    pub mtime_ns: i64,
    /// Its access time, ns, where collected.
    pub atime_ns: Option<i64>,
    /// Its creation time, ns, where collected and kept.
    pub btime_ns: Option<i64>,
    /// `st_mode`, where collected.
    pub mode: Option<u32>,
    /// Its owner, where collected.
    pub uid: Option<u32>,
    /// Its group, where collected.
    pub gid: Option<u32>,
    /// Its link count.
    pub nlink: u64,
    /// A hard-linked file's device and inode.
    pub inode: Option<(u64, u64)>,
    /// Why the scan listed it without entering it, as ncdu names it:
    /// `pattern` (an exclude), `otherfs`, `kernfs`, or `frmlink` (the
    /// volume a macOS scan reaches through its firmlinks).
    pub excluded: Option<&'static str>,
    /// A directory's facts.
    pub dir: Option<DirFacts>,
}

/// Calls `each` with every entry from `base` down, in pre-order.
///
/// # Errors
///
/// [`CoreError::IndexCorrupt`] when the index cannot be read, and
/// whatever `each` returns.
pub fn walk(
    index: &Index,
    base: u32,
    mut each: impl FnMut(&Row) -> Result<(), CoreError>,
) -> Result<(), CoreError> {
    let corrupt = |e: rusqlite::Error| CoreError::IndexCorrupt {
        file: index.file().display().to_string(),
        message: e.to_string(),
    };
    let last = index.node(base).last_id;
    let facts = Facts {
        index,
        base,
        base_depth: index.node(base).depth,
        inodes: inodes(index, base, last).map_err(corrupt)?,
        unentered: unentered(index),
    };
    let db = index.db();
    let mut dirs = db
        .prepare(
            "SELECT id, last_id, name, apparent, allocated, cum_apparent, cum_allocated, \
             mtime, atime, btime, mode, uid, gid, flags, errors IS NOT NULL, dev, depth \
             FROM dirs WHERE id BETWEEN ?1 AND ?2 ORDER BY id",
        )
        .map_err(corrupt)?;
    let mut files = db
        .prepare(
            "SELECT f.id, f.name, f.apparent, f.allocated, f.charged, f.mtime, f.atime, \
             f.btime, f.mode, f.uid, f.gid, f.nlink, f.kind, f.flags, d.depth + 1 \
             FROM files AS f JOIN dirs AS d ON d.id = f.parent_id \
             WHERE f.id BETWEEN ?1 AND ?2 ORDER BY f.id",
        )
        .map_err(corrupt)?;
    let mut dir_rows = dirs.query([base, last]).map_err(corrupt)?;
    let mut file_rows = files.query([base, last]).map_err(corrupt)?;
    let mut next_dir =
        || -> rusqlite::Result<Option<Row>> { dir_rows.next()?.map(|r| facts.dir(r)).transpose() };
    let mut next_file = || -> rusqlite::Result<Option<Row>> {
        file_rows.next()?.map(|r| facts.file(r)).transpose()
    };
    let mut dir = next_dir().map_err(corrupt)?;
    let mut file = next_file().map_err(corrupt)?;
    // The two tables share one id sequence: the smaller id comes first.
    loop {
        match (dir.take(), file.take()) {
            (Some(d), f) if f.as_ref().is_none_or(|f| d.id < f.id) => {
                each(&d)?;
                dir = next_dir().map_err(corrupt)?;
                file = f;
            }
            (d, Some(f)) => {
                each(&f)?;
                file = next_file().map_err(corrupt)?;
                dir = d;
            }
            _ => break,
        }
    }
    if let Some(message) = index.failure() {
        return Err(CoreError::IndexCorrupt {
            file: index.file().display().to_string(),
            message,
        });
    }
    Ok(())
}

/// What turns the index's rows into [`Row`] values.
struct Facts<'a> {
    index: &'a Index,
    /// The exported entry, named by its absolute path.
    base: u32,
    base_depth: u32,
    inodes: HashMap<u32, (u64, u64)>,
    unentered: HashMap<u32, &'static str>,
}

impl Facts<'_> {
    /// A `dirs` row, in the order `walk` selects its columns.
    fn dir(&self, row: &rusqlite::Row<'_>) -> rusqlite::Result<Row> {
        let id: u32 = row.get(0)?;
        let flags: u32 = row.get(13)?;
        let depth: u32 = row.get(16)?;
        // A firmlink (`FIRMLINK`) is entered: only the volume behind the
        // firmlinks is left alone, as `unentered` finds.
        let excluded = if flags & (EXCLUDED | EXCLUDED_CACHE) != 0 {
            Some("pattern")
        } else {
            self.unentered.get(&id).copied()
        };
        Ok(Row {
            id,
            depth: depth.saturating_sub(self.base_depth),
            name: self.name(id, row.get(2)?),
            kind: RowKind::Dir,
            apparent: unsigned(row.get(3)?),
            allocated: unsigned(row.get(4)?),
            charged: true,
            later_link: false,
            mtime_ns: row.get(7)?,
            atime_ns: row.get(8)?,
            btime_ns: row.get(9)?,
            mode: row.get(10)?,
            uid: row.get(11)?,
            gid: row.get(12)?,
            nlink: 1,
            inode: None,
            excluded,
            dir: Some(DirFacts {
                last_id: row.get(1)?,
                dev: unsigned(row.get(15)?),
                cum_apparent: unsigned(row.get(5)?),
                cum_allocated: unsigned(row.get(6)?),
                unreadable: row.get(14)?,
            }),
        })
    }

    /// A `files` row, in the order `walk` selects its columns.
    fn file(&self, row: &rusqlite::Row<'_>) -> rusqlite::Result<Row> {
        let id: u32 = row.get(0)?;
        let flags: u32 = row.get(13)?;
        let kind: String = row.get(12)?;
        let depth: u32 = row.get(14)?;
        Ok(Row {
            id,
            depth: depth.saturating_sub(self.base_depth),
            name: self.name(id, row.get(1)?),
            kind: match kind.as_str() {
                "file" => RowKind::File,
                "symlink" => RowKind::Symlink,
                _ => RowKind::Other,
            },
            apparent: unsigned(row.get(2)?),
            allocated: unsigned(row.get(3)?),
            charged: row.get(4)?,
            later_link: flags & LATER_LINK != 0,
            mtime_ns: row.get(5)?,
            atime_ns: row.get(6)?,
            btime_ns: row.get(7)?,
            mode: row.get(8)?,
            uid: row.get(9)?,
            gid: row.get(10)?,
            nlink: unsigned(row.get(11)?),
            inode: self.inodes.get(&id).copied(),
            excluded: None,
            dir: None,
        })
    }

    /// An entry's name; the exported entry's absolute path for it.
    fn name(&self, id: u32, name: Vec<u8>) -> Vec<u8> {
        if id == self.base {
            crate::query::abs_path(self.index, id)
        } else {
            name
        }
    }
}

/// A size or count the index stores as an integer, which is never negative.
fn unsigned(n: i64) -> u64 {
    u64::try_from(n).unwrap_or(0)
}

/// Each hard-linked file's device and inode between `first` and `last`.
fn inodes(index: &Index, first: u32, last: u32) -> rusqlite::Result<HashMap<u32, (u64, u64)>> {
    let mut stmt = index.db().prepare(
        "SELECT s.id, h.dev, h.ino FROM shared_members AS s \
         JOIN hardlinks AS h ON h.first_id = s.link_first \
         WHERE s.id BETWEEN ?1 AND ?2",
    )?;
    let rows = stmt.query_map([first, last], |row| {
        let dev: i64 = row.get(1)?;
        let ino: i64 = row.get(2)?;
        Ok((
            row.get::<_, u32>(0)?,
            (
                u64::from_ne_bytes(dev.to_ne_bytes()),
                u64::from_ne_bytes(ino.to_ne_bytes()),
            ),
        ))
    })?;
    rows.collect()
}

/// The directories the scan did not enter because they are other volumes
/// it left alone, by id, as ncdu names the reason.
fn unentered(index: &Index) -> HashMap<u32, &'static str> {
    let root = index.root_path().to_owned();
    index
        .volumes()
        .into_iter()
        .filter(|v| !v.entered)
        .filter_map(|v| {
            let reason = match v.reason.as_deref() {
                Some("kernel") => "kernfs",
                Some("firmlinked") => "frmlink",
                Some("excluded" | "excluded_cache") => "pattern",
                _ => "otherfs",
            };
            let mount = std::path::PathBuf::from(std::ffi::OsString::from_vec(v.mount_point));
            let rel = mount.strip_prefix(&root).ok()?;
            let parts: Vec<&[u8]> = rel
                .components()
                .map(|c| c.as_os_str().as_encoded_bytes())
                .collect();
            if parts.is_empty() {
                return None;
            }
            index.lookup(parts).map(|id| (id, reason))
        })
        .collect()
}

/// The absolute path of each row, read from the rows in pre-order.
#[derive(Debug, Default)]
pub(crate) struct Paths {
    /// The open directories: their last id and path.
    open: Vec<(u32, Vec<u8>)>,
}

impl Paths {
    /// `row`'s absolute path; a directory's opens for the rows under it.
    pub(crate) fn path(&mut self, row: &Row) -> Vec<u8> {
        while self.open.last().is_some_and(|(last, _)| *last < row.id) {
            self.open.pop();
        }
        let path = match self.open.last() {
            // The exported entry, named by its absolute path.
            None => row.name.clone(),
            Some((_, parent)) => {
                let mut path = parent.clone();
                if path.last() != Some(&b'/') {
                    path.push(b'/');
                }
                path.extend_from_slice(&row.name);
                path
            }
        };
        if let Some(dir) = row.dir {
            self.open.push((dir.last_id, path.clone()));
        }
        path
    }
}

/// A time in Unix seconds, for the formats that keep whole seconds.
pub(crate) fn seconds(ns: i64) -> i64 {
    ns.div_euclid(1_000_000_000)
}
