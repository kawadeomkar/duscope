//! The reader: a saved scan, opened read-only, as a [`TreeView`].
//!
//! An index is opened through a `mode=ro&immutable=1` URI: snapshots are
//! never edited, so the read takes no locks and reads no journal, and with
//! `SQLITE_DBCONFIG_DEFENSIVE` and untrusted schema nothing in the file can
//! change it or run outside plain reads. The version is checked first: a
//! reader opens every minor of its major and refuses the others.
//!
//! Entries are read in blocks of 256 ids, a query for each table, and the
//! 32 blocks used last are kept: a query that walks the ids in
//! order reads each block once, and one that climbs from an entry to the
//! root finds the ancestors' blocks again. Paths and subdirectories come
//! from the nodes, as the trait's defaults read them.
//!
//! The queries' views cannot return errors, so a read that fails after the
//! file opened is kept and the entry reads as empty; the verb asks
//! [`Index::failure`] once its query is done.

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::fmt::Write as _;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

use duscope_walk::{Attrs, Flags, Kind};
use rusqlite::config::DbConfig;
use rusqlite::types::Value;
use rusqlite::{Connection, OpenFlags, OptionalExtension, Row};
use unicode_normalization::UnicodeNormalization;

use super::scanid::ScanId;
use super::writer::{EXCLUDED, EXCLUDED_CACHE, LATER_LINK, ScanMeta, UPPER_BOUND};
use crate::CoreError;
use crate::query::{Node, TreeView};

/// The major version of the index this build reads.
const MAJOR: u32 = 2;

/// Entries read at once.
const BLOCK: u32 = 256;

/// Blocks kept, the one used last first.
const BLOCKS: usize = 32;

/// Consecutive entries, from `start`: each one's node and name.
#[derive(Debug)]
struct Block {
    start: u32,
    entries: Vec<(Node, Vec<u8>)>,
}

/// A volume the scan met, as its index records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexVolume {
    /// Where it is mounted.
    pub mount_point: Vec<u8>,
    /// The filesystem type.
    pub fs_type: String,
    /// Mounted read-only.
    pub read_only: bool,
    /// Whether the scan entered it.
    pub entered: bool,
    /// Why not; `None` when it did.
    pub reason: Option<String>,
    /// Its size, where recorded.
    pub total_bytes: Option<u64>,
    /// The bytes free on it, where recorded.
    pub free_bytes: Option<u64>,
    /// Its mount options.
    pub options: Vec<String>,
}

/// A saved scan, open for reading.
#[derive(Debug)]
pub struct Index {
    db: Connection,
    path: PathBuf,
    meta: ScanMeta,
    version: String,
    attrs: Attrs,
    len: u32,
    root: PathBuf,
    /// The blocks read last, the latest first.
    blocks: RefCell<Vec<Block>>,
    /// The first read that failed after the file opened.
    failed: RefCell<Option<String>>,
}

impl Index {
    /// Opens the index at `path` read-only. A file that is not an index,
    /// or is damaged, is [`CoreError::IndexCorrupt`]; one of another major
    /// version [`CoreError::IndexVersion`].
    pub fn open(path: &Path) -> Result<Index, CoreError> {
        let file = path.display().to_string();
        let corrupt = |message: String| CoreError::IndexCorrupt {
            file: file.clone(),
            message,
        };
        let db = Connection::open_with_flags(
            uri(path),
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_URI
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|e| corrupt(e.to_string()))?;
        db.set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)
            .and_then(|_| db.set_db_config(DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA, false))
            .map_err(|e| corrupt(e.to_string()))?;
        // The version first: a file of another major may lay out its other
        // tables differently.
        let version: String = db
            .query_row(
                "SELECT value FROM scan_meta WHERE key = 'index_version'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| corrupt(e.to_string()))?;
        if major(&version) != Some(MAJOR) {
            return Err(CoreError::IndexVersion {
                file,
                found: version,
            });
        }
        let mut keys: HashMap<String, Value> = HashMap::new();
        {
            let mut stmt = db
                .prepare("SELECT key, value FROM scan_meta")
                .map_err(|e| corrupt(e.to_string()))?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, Value>(1)?))
                })
                .map_err(|e| corrupt(e.to_string()))?;
            for row in rows {
                let (key, value) = row.map_err(|e| corrupt(e.to_string()))?;
                keys.insert(key, value);
            }
        }
        let meta = Meta(&keys);
        let id = meta.text("scan_id");
        let scan_id = ScanId::parse(&id)
            .ok_or_else(|| corrupt(format!("scan_meta.scan_id {id:?} is not a scan id")))?;
        let attrs = match meta.text("attrs").as_str() {
            "full" => Attrs::Full,
            _ => Attrs::Basic,
        };
        let len = u32::try_from(meta.int("n_entries")).unwrap_or(0);
        let root_bytes = meta.bytes("root");
        let read = ScanMeta {
            scan_id,
            root: root_bytes.clone(),
            root_as_given: meta.bytes("root_as_given"),
            started_at: meta.int("started_at"),
            finished_at: meta.int("finished_at"),
            options: meta.text("options"),
            platform: meta.text("platform"),
            scanning_uid: u32::try_from(meta.int("scanning_uid")).unwrap_or(0),
            thread_count: unsigned(meta.int("thread_count")),
            size_kind: if meta.text("size_kind") == "apparent" {
                "apparent"
            } else {
                "allocated"
            },
            volume_total: unsigned(meta.int("volume_total")),
            volume_free: unsigned(meta.int("volume_free")),
            unaccounted_bytes: meta.opt_int("unaccounted_bytes").map(unsigned),
            error_count: unsigned(meta.int("error_count")),
            vanished_count: unsigned(meta.int("vanished_count")),
            excluded_count: unsigned(meta.int("excluded_count")),
            clones_unavailable: meta.int("clones_unavailable") != 0,
            incremental: meta.int("incremental") != 0,
            parent_scan_id: meta.opt_text("parent_scan_id"),
            source: meta.text("source"),
            tool_version: meta.text("tool_version"),
        };
        Ok(Index {
            db,
            path: path.to_owned(),
            root: PathBuf::from(OsStr::from_bytes(&root_bytes)),
            meta: read,
            version,
            attrs,
            len,
            blocks: RefCell::new(Vec::new()),
            failed: RefCell::new(None),
        })
    }

    /// What `scan_meta` records.
    pub fn meta(&self) -> &ScanMeta {
        &self.meta
    }

    /// The index file's path.
    pub fn file(&self) -> &Path {
        &self.path
    }

    /// The connection, for the query plans.
    pub(crate) fn db(&self) -> &Connection {
        &self.db
    }

    /// The index version the file records.
    pub fn version(&self) -> &str {
        &self.version
    }

    /// The attributes the scan collected.
    pub fn attrs(&self) -> Attrs {
        self.attrs
    }

    /// The first read that failed after the file opened, which the queries
    /// read as an empty entry.
    pub fn failure(&self) -> Option<String> {
        self.failed.borrow().clone()
    }

    /// Every volume the scan met, in the order it met them.
    pub fn volumes(&self) -> Vec<IndexVolume> {
        let read = || -> rusqlite::Result<Vec<IndexVolume>> {
            let mut stmt = self.db.prepare(
                "SELECT mount_point, fs_type, read_only, entered, reason, total_bytes, \
                 free_bytes, options FROM scan_volumes ORDER BY rowid",
            )?;
            let rows = stmt.query_map([], |row| {
                let options: Option<String> = row.get(7)?;
                Ok(IndexVolume {
                    mount_point: row.get(0)?,
                    fs_type: row.get(1)?,
                    read_only: row.get(2)?,
                    entered: row.get(3)?,
                    reason: row.get(4)?,
                    total_bytes: row.get::<_, Option<i64>>(5)?.map(unsigned),
                    free_bytes: row.get::<_, Option<i64>>(6)?.map(unsigned),
                    options: options
                        .filter(|o| !o.is_empty())
                        .map(|o| o.split(',').map(str::to_owned).collect())
                        .unwrap_or_default(),
                })
            })?;
            rows.collect()
        };
        read().unwrap_or_else(|e| {
            self.fail(&e);
            Vec::new()
        })
    }

    /// The directories the excludes kept the scan out of: each one's id and
    /// why (`excluded` or `excluded_cache`), in id order.
    pub fn excluded(&self) -> Vec<(u32, &'static str)> {
        let read = || -> rusqlite::Result<Vec<(u32, &'static str)>> {
            let mut stmt = self
                .db
                .prepare("SELECT id, flags FROM dirs WHERE flags & ?1 != 0 ORDER BY id")?;
            let rows = stmt.query_map([EXCLUDED | EXCLUDED_CACHE], |row| {
                let flags: u32 = row.get(1)?;
                let reason = if flags & EXCLUDED_CACHE != 0 {
                    "excluded_cache"
                } else {
                    "excluded"
                };
                Ok((row.get(0)?, reason))
            })?;
            rows.collect()
        };
        read().unwrap_or_else(|e| {
            self.fail(&e);
            Vec::new()
        })
    }

    /// The directories with ids from `first` to `last` that could not be
    /// read: the whole scan's, or a subtree's.
    pub fn unreadable_dirs(&self, first: u32, last: u32) -> u64 {
        self.db
            .query_row(
                "SELECT COUNT(*) FROM dirs WHERE errors IS NOT NULL AND id BETWEEN ?1 AND ?2",
                [first, last],
                |row| row.get::<_, i64>(0),
            )
            .map_or_else(
                |e| {
                    self.fail(&e);
                    0
                },
                unsigned,
            )
    }

    /// `PRAGMA quick_check`: empty when the file is sound, else each
    /// problem it found.
    pub fn quick_check(&self) -> Result<Vec<String>, CoreError> {
        let mut stmt = self
            .db
            .prepare("PRAGMA quick_check")
            .map_err(|e| self.corrupt(&e))?;
        let rows: rusqlite::Result<Vec<String>> = stmt
            .query_map([], |row| row.get(0))
            .map_err(|e| self.corrupt(&e))?
            .collect();
        let rows = rows.map_err(|e| self.corrupt(&e))?;
        Ok(rows.into_iter().filter(|r| r != "ok").collect())
    }

    /// The entry at `rel`, a path below the root given as its components;
    /// `None` when the scan holds no such entry. A component matches byte
    /// for byte, or, failing that, as its volume compares names: without
    /// case where the volume is case-insensitive, in NFC where it treats a
    /// name's composed and decomposed forms as one. Subdirectories are found
    /// through `dirs (parent_id, name)`, files in their directory's id
    /// range.
    pub fn lookup<'a>(&self, rel: impl IntoIterator<Item = &'a [u8]>) -> Option<u32> {
        let found = (|| -> rusqlite::Result<Option<u32>> {
            let mut dirs = self
                .db
                .prepare("SELECT id, files_end FROM dirs WHERE parent_id = ?1 AND name = ?2")?;
            let mut files = self
                .db
                .prepare("SELECT id FROM files WHERE id BETWEEN ?1 AND ?2 AND name = ?3")?;
            let root = self.node(0);
            // The entry reached, and the end of its own files when it is a
            // directory.
            let (mut at, mut files_end) = (0u32, root.is_dir().then_some(root.files_end));
            for name in rel {
                let Some(end) = files_end else {
                    return Ok(None);
                };
                let exact = match dirs
                    .query_row(rusqlite::params![at, name], |row| {
                        Ok((row.get(0)?, Some(row.get(1)?)))
                    })
                    .optional()?
                {
                    Some(dir) => Some(dir),
                    None => files
                        .query_row(rusqlite::params![at + 1, end, name], |row| {
                            Ok((row.get(0)?, None))
                        })
                        .optional()?,
                };
                let found = match exact {
                    Some(found) => Some(found),
                    None => self.folded(at, end, name)?,
                };
                let Some(found) = found else {
                    return Ok(None);
                };
                (at, files_end) = found;
            }
            Ok(Some(at))
        })();
        found.unwrap_or_else(|e| {
            self.fail(&e);
            None
        })
    }

    /// The entry in directory `at` (its own files up to `files_end`) whose
    /// name is `name` as the directory's volume compares names; `None` on a
    /// volume that compares bytes, or when none matches.
    fn folded(
        &self,
        at: u32,
        files_end: u32,
        name: &[u8],
    ) -> rusqlite::Result<Option<(u32, Option<u32>)>> {
        let Some(fold) = self.fold_of(at)? else {
            return Ok(None);
        };
        let want = fold.apply(name);
        let mut dirs = self
            .db
            .prepare("SELECT id, files_end, name FROM dirs WHERE parent_id = ?1 ORDER BY id")?;
        let mut rows = dirs.query([at])?;
        while let Some(row) = rows.next()? {
            let candidate: Vec<u8> = row.get(2)?;
            if fold.apply(&candidate) == want {
                return Ok(Some((row.get(0)?, Some(row.get(1)?))));
            }
        }
        let mut files = self
            .db
            .prepare("SELECT id, name FROM files WHERE id BETWEEN ?1 AND ?2 ORDER BY id")?;
        let mut rows = files.query([at + 1, files_end])?;
        while let Some(row) = rows.next()? {
            let candidate: Vec<u8> = row.get(1)?;
            if fold.apply(&candidate) == want {
                return Ok(Some((row.get(0)?, None)));
            }
        }
        Ok(None)
    }

    /// How the volume directory `dir` is on compares names; `None` where it
    /// compares bytes.
    fn fold_of(&self, dir: u32) -> rusqlite::Result<Option<Fold>> {
        let volume: Option<(Option<bool>, String)> = self
            .db
            .query_row(
                "SELECT v.case_sensitive, v.normalization FROM dirs AS d \
                 JOIN scan_volumes AS v ON v.dev = d.dev WHERE d.id = ?1 LIMIT 1",
                [dir],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        Ok(volume.and_then(|(case_sensitive, normalization)| {
            let fold = Fold {
                case: case_sensitive == Some(false),
                nfc: normalization == "insensitive",
            };
            (fold.case || fold.nfc).then_some(fold)
        }))
    }

    /// `path`, absolute, as components below the scan's root: matched byte
    /// for byte, or as the root's volume compares names, so a root typed in
    /// another case still holds the path. `None` when the root does not
    /// contain it.
    pub fn below_root<'p>(&self, path: &'p Path) -> Option<Vec<&'p [u8]>> {
        let names = |rest: &mut dyn Iterator<Item = Component<'p>>| -> Option<Vec<&'p [u8]>> {
            let mut out = Vec::new();
            for component in rest {
                match component {
                    Component::Normal(name) => out.push(name.as_bytes()),
                    Component::CurDir => {}
                    _ => return None,
                }
            }
            Some(out)
        };
        if let Ok(rel) = path.strip_prefix(&self.root) {
            return names(&mut rel.components());
        }
        let fold = match self.fold_of(0) {
            Ok(fold) => fold?,
            Err(e) => {
                self.fail(&e);
                return None;
            }
        };
        let mut asked = path.components();
        for root in self.root.components() {
            let given = asked.next()?;
            let same = root == given
                || fold.apply(root.as_os_str().as_bytes())
                    == fold.apply(given.as_os_str().as_bytes());
            if !same {
                return None;
            }
        }
        names(&mut asked)
    }

    /// Whether the scan read the entry `id`: not a directory it listed but
    /// kept out of (an exclude, a cache rule), could not read, or a mount
    /// point it did not enter.
    pub fn entered(&self, id: u32) -> bool {
        let skipped = self
            .db
            .query_row(
                "SELECT flags & ?2 != 0 OR errors IS NOT NULL FROM dirs WHERE id = ?1",
                [id, EXCLUDED | EXCLUDED_CACHE],
                |row| row.get::<_, bool>(0),
            )
            .optional();
        match skipped {
            Ok(Some(true)) => return false,
            Ok(_) => {}
            Err(e) => {
                self.fail(&e);
                return false;
            }
        }
        // Its path as the scan stored it, which a mount point is spelt as.
        let mut path = self.root.as_os_str().as_bytes().to_vec();
        let rel = self.path(id);
        if !rel.is_empty() {
            path.push(b'/');
            path.extend_from_slice(&rel);
        }
        !self
            .volumes()
            .iter()
            .any(|v| !v.entered && v.mount_point == path)
    }

    /// Keeps the first failed read.
    fn fail(&self, e: &rusqlite::Error) {
        let mut failed = self.failed.borrow_mut();
        if failed.is_none() {
            *failed = Some(e.to_string());
        }
    }

    fn corrupt(&self, e: &rusqlite::Error) -> CoreError {
        CoreError::IndexCorrupt {
            file: self.path.display().to_string(),
            message: e.to_string(),
        }
    }

    /// Entry `id`'s node and name, from its block: read and kept first if
    /// it is not kept already.
    fn entry<T>(&self, id: u32, f: impl FnOnce(&(Node, Vec<u8>)) -> T) -> Option<T> {
        if id >= self.len {
            self.fail(&rusqlite::Error::QueryReturnedNoRows);
            return None;
        }
        let start = id - id % BLOCK;
        let mut blocks = self.blocks.borrow_mut();
        let at = if let Some(at) = blocks.iter().position(|b| b.start == start) {
            at
        } else {
            let block = match self.read_block(start) {
                Ok(block) => block,
                Err(e) => {
                    drop(blocks);
                    self.fail(&e);
                    return None;
                }
            };
            blocks.truncate(BLOCKS - 1);
            blocks.push(block);
            blocks.len() - 1
        };
        let block = blocks.remove(at);
        let found = block.entries.get((id - start) as usize).map(f);
        blocks.insert(0, block);
        found
    }

    /// The block of ids from `start`, every id in it found in one table or
    /// the other.
    fn read_block(&self, start: u32) -> rusqlite::Result<Block> {
        let end = start.saturating_add(BLOCK).min(self.len);
        let mut slots: Vec<Option<(Node, Vec<u8>)>> = (start..end).map(|_| None).collect();
        let mut put = |id: u32, entry: (Node, Vec<u8>)| {
            if let Some(slot) = id
                .checked_sub(start)
                .and_then(|i| slots.get_mut(i as usize))
            {
                *slot = Some(entry);
            }
        };
        let mut dirs = self.db.prepare(
            "SELECT id, last_id, files_end, parent_id, depth, cum_apparent, cum_allocated, \
             n_files, n_dirs, mtime, ctime, max_mtime, flags, errors IS NOT NULL, name, \
             atime, btime, mode FROM dirs WHERE id >= ?1 AND id < ?2",
        )?;
        let mut rows = dirs.query([start, end])?;
        while let Some(row) = rows.next()? {
            let id: u32 = row.get(0)?;
            put(id, (dir_from_row(id, row)?, row.get(14)?));
        }
        let mut files = self.db.prepare(
            "SELECT f.id, f.parent_id, f.apparent, f.allocated, f.charged, f.mtime, f.ctime, \
             f.atime, f.btime, f.mode, f.nlink, f.kind, f.flags, d.depth, f.name \
             FROM files AS f LEFT JOIN dirs AS d ON d.id = f.parent_id \
             WHERE f.id >= ?1 AND f.id < ?2",
        )?;
        let mut rows = files.query([start, end])?;
        while let Some(row) = rows.next()? {
            let id: u32 = row.get(0)?;
            put(id, (file_from_row(id, row)?, row.get(14)?));
        }
        let entries: Option<Vec<(Node, Vec<u8>)>> = slots.into_iter().collect();
        let entries = entries.ok_or(rusqlite::Error::QueryReturnedNoRows)?;
        Ok(Block { start, entries })
    }
}

impl TreeView for Index {
    fn len(&self) -> u32 {
        self.len
    }

    fn node(&self, id: u32) -> Node {
        self.entry(id, |&(node, _)| node)
            .unwrap_or_else(|| empty(id))
    }

    fn root_path(&self) -> &Path {
        &self.root
    }

    fn full_attrs(&self) -> bool {
        self.attrs == Attrs::Full
    }

    /// The root's name is its last component, as the walk names it (the
    /// whole path for `/`); the index keeps the whole path in its row.
    fn name(&self, id: u32) -> Cow<'_, [u8]> {
        if id == 0 {
            let name = self
                .root
                .file_name()
                .unwrap_or(self.root.as_os_str())
                .as_bytes();
            return Cow::Borrowed(name);
        }
        self.entry(id, |(_, name)| Cow::Owned(name.clone()))
            .unwrap_or(Cow::Borrowed(&[]))
    }
}

/// A directory's node from its `dirs` row, as [`Index::read_block`] selects
/// it (`id` first).
fn dir_from_row(id: u32, row: &Row<'_>) -> rusqlite::Result<Node> {
    let flags: u32 = row.get(12)?;
    Ok(Node {
        id,
        parent: row.get::<_, Option<u32>>(3)?.unwrap_or(0),
        depth: row.get(4)?,
        kind: Kind::Dir,
        allocated: unsigned(row.get(6)?),
        apparent: unsigned(row.get(5)?),
        file_count: unsigned(row.get(7)?),
        dir_count: unsigned(row.get(8)?) + 1,
        mtime_ns: row.get(9)?,
        max_mtime_ns: row.get(11)?,
        ctime_ns: row.get(10)?,
        atime_ns: row.get(15)?,
        btime_ns: row.get(16)?,
        mode: row.get(17)?,
        nlink: 0,
        charged: true,
        linked_away: false,
        flags: Flags::from_bits_truncate(flags),
        upper_bound: flags & UPPER_BOUND != 0,
        unreadable: row.get(13)?,
        files_end: row.get(2)?,
        last_id: row.get(1)?,
    })
}

/// A non-directory's node from its `files` row and its parent's depth, as
/// [`Index::read_block`] selects them (`id` first).
fn file_from_row(id: u32, row: &Row<'_>) -> rusqlite::Result<Node> {
    let flags: u32 = row.get(12)?;
    let kind: String = row.get(11)?;
    let parent_depth: Option<u32> = row.get(13)?;
    Ok(Node {
        id,
        parent: row.get(1)?,
        depth: if id == 0 {
            0
        } else {
            parent_depth.map_or(0, |d| d + 1)
        },
        kind: match kind.as_str() {
            "file" => Kind::File,
            "dir" => Kind::Dir,
            "symlink" => Kind::Symlink,
            _ => Kind::Unknown,
        },
        allocated: unsigned(row.get(3)?),
        apparent: unsigned(row.get(2)?),
        file_count: 1,
        dir_count: 0,
        mtime_ns: row.get(5)?,
        max_mtime_ns: row.get(5)?,
        ctime_ns: row.get(6)?,
        atime_ns: row.get(7)?,
        btime_ns: row.get(8)?,
        mode: row.get(9)?,
        nlink: row.get(10)?,
        charged: row.get(4)?,
        linked_away: flags & LATER_LINK != 0,
        flags: Flags::from_bits_truncate(flags),
        upper_bound: false,
        unreadable: false,
        files_end: id,
        last_id: id,
    })
}

/// How a volume compares names: without case, in NFC, or both.
#[derive(Debug, Clone, Copy)]
struct Fold {
    case: bool,
    nfc: bool,
}

impl Fold {
    /// `name` in the form the volume compares: NFC, then lower case, for a
    /// name that is UTF-8; ASCII lower case for one that is not.
    fn apply(self, name: &[u8]) -> Cow<'_, [u8]> {
        match std::str::from_utf8(name) {
            Ok(text) => {
                let mut text: String = if self.nfc {
                    text.nfc().collect()
                } else {
                    text.to_owned()
                };
                // Each character's own lower case, not str::to_lowercase,
                // which spells a final sigma differently.
                if self.case {
                    text = text.chars().flat_map(char::to_lowercase).collect();
                }
                Cow::Owned(text.into_bytes())
            }
            Err(_) if self.case => Cow::Owned(name.to_ascii_lowercase()),
            Err(_) => Cow::Borrowed(name),
        }
    }
}

/// The node a failed read leaves: an empty file.
fn empty(id: u32) -> Node {
    Node {
        id,
        parent: 0,
        depth: 0,
        kind: Kind::Unknown,
        allocated: 0,
        apparent: 0,
        file_count: 0,
        dir_count: 0,
        mtime_ns: 0,
        max_mtime_ns: 0,
        ctime_ns: 0,
        atime_ns: None,
        btime_ns: None,
        mode: None,
        nlink: 0,
        charged: true,
        linked_away: false,
        flags: Flags::empty(),
        upper_bound: false,
        unreadable: false,
        files_end: id,
        last_id: id,
    }
}

/// The read-only, immutable URI of `path`: every byte but the unreserved
/// ones and `/` percent-encoded, so `?`, `#` and `%` in a name stay in the
/// path.
fn uri(path: &Path) -> String {
    let mut uri = String::from("file:");
    for &b in path.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) {
            uri.push(char::from(b));
        } else {
            let _ = write!(uri, "%{b:02X}");
        }
    }
    uri.push_str("?mode=ro&immutable=1");
    uri
}

/// The major of a `major.minor` version.
fn major(version: &str) -> Option<u32> {
    let (major, minor) = version.split_once('.')?;
    minor.parse::<u32>().ok()?;
    major.parse().ok()
}

/// An unsigned value from the index's signed integer with the same bits.
#[allow(clippy::cast_sign_loss)]
fn unsigned(value: i64) -> u64 {
    value as u64
}

/// `scan_meta`'s values, by key, read leniently: a missing key or a value
/// of another type reads as empty.
struct Meta<'a>(&'a HashMap<String, Value>);

impl Meta<'_> {
    fn int(&self, key: &str) -> i64 {
        self.opt_int(key).unwrap_or(0)
    }

    fn opt_int(&self, key: &str) -> Option<i64> {
        match self.0.get(key) {
            Some(Value::Integer(n)) => Some(*n),
            Some(Value::Text(t)) => t.parse().ok(),
            _ => None,
        }
    }

    fn text(&self, key: &str) -> String {
        self.opt_text(key).unwrap_or_default()
    }

    fn opt_text(&self, key: &str) -> Option<String> {
        match self.0.get(key) {
            Some(Value::Text(t)) => Some(t.clone()),
            Some(Value::Integer(n)) => Some(n.to_string()),
            Some(Value::Blob(b)) => Some(String::from_utf8_lossy(b).into_owned()),
            _ => None,
        }
    }

    fn bytes(&self, key: &str) -> Vec<u8> {
        match self.0.get(key) {
            Some(Value::Blob(b)) => b.clone(),
            Some(Value::Text(t)) => t.as_bytes().to_vec(),
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests;
