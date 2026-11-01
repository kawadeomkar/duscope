//! The reader: a saved scan, opened read-only, as a [`TreeView`].
//!
//! An index is opened through a `mode=ro&immutable=1` URI: snapshots are
//! never edited, so the read takes no locks and reads no journal, and with
//! `SQLITE_DBCONFIG_DEFENSIVE` and untrusted schema nothing in the file can
//! change it or run outside plain reads. Any index file may have been made
//! by someone else (SECURITY.md), so its views and triggers are switched
//! off rather than trusted, and a schema no duscope writes is refused: a
//! virtual table, a statement longer than [`SQL_LENGTH`], or more than
//! [`SCHEMA_OBJECTS`] objects, where loading stops early. The version is
//! checked first: a reader opens every minor of its major and refuses the
//! others; `tool_version` must name a duscope version.
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
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use duscope_walk::{Attrs, Flags, Kind};
use rusqlite::config::DbConfig;
use rusqlite::limits::Limit;
use rusqlite::types::Value;
use rusqlite::{Connection, OpenFlags, OptionalExtension, Row};
use unicode_normalization::UnicodeNormalization;

use super::scanid::ScanId;
use super::writer::{EXCLUDED, EXCLUDED_CACHE, LATER_LINK, ScanMeta, UPPER_BOUND};
use crate::CoreError;
use crate::query::{Node, TreeView};

/// The major version of the index this build reads.
const MAJOR: u32 = 2;

/// The longest statement the connection parses, the schema's included: an
/// index's longest is under 2 KiB.
const SQL_LENGTH: i32 = 64 * 1024;

/// The longest string or BLOB the connection reads: a name or a path is
/// at most a few KiB.
const VALUE_LENGTH: i32 = 1_000_000;

/// The most objects (tables and indexes) a schema may have: an index has
/// about 25.
const SCHEMA_OBJECTS: i64 = 256;

/// How often, in virtual machine steps, the progress handler counts while
/// the schema loads.
const STEPS_PER_CALL: i32 = 100;

/// The progress handler's calls the schema may take to load and count, at
/// [`STEPS_PER_CALL`] steps each: loading takes about 8 steps a row, so a
/// schema of a few hundred objects stays well inside the budget and one of
/// millions stops early.
const SCHEMA_CALLS: u32 = 200;

/// Entries read at once.
const BLOCK: u32 = 256;

/// Blocks kept, the one used last first.
const BLOCKS: usize = 32;

/// Entries read one at a time and kept, beside the blocks; the whole set is
/// dropped when it fills.
const SINGLES: usize = 16_384;

/// Entries of one block read one at a time before the block is read whole,
/// however far apart: a walk that strides reaches it, scattered reads (a
/// directory's subdirectories, a path's ancestors, the rows a plan returns)
/// seldom do.
const MISSES: u8 = 8;

/// The columns of a directory's node and name, as [`dir_from_row`] reads
/// them.
const DIR_COLUMNS: &str = "id, last_id, files_end, parent_id, depth, cum_apparent, \
     cum_allocated, n_files, n_dirs, mtime, ctime, max_mtime, flags, errors IS NOT NULL, name, \
     atime, btime, mode";

/// The columns of a file's node and name, as [`file_from_row`] reads them,
/// from `files AS f` joined to its parent `dirs AS d`.
const FILE_COLUMNS: &str = "f.id, f.parent_id, f.apparent, f.allocated, f.charged, f.mtime, \
     f.ctime, f.atime, f.btime, f.mode, f.nlink, f.kind, f.flags, d.depth, f.name";

/// Consecutive entries, from `start`: each one's node and name.
#[derive(Debug)]
struct Block {
    start: u32,
    entries: Vec<(Node, Vec<u8>)>,
}

/// What a scan could not read: a directory's id, the entry's name (none
/// for the directory itself), and the error number.
pub type ScanErrorRow = (u32, Option<Vec<u8>>, i32);

/// A volume the scan met, as its index records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexVolume {
    /// Its device.
    pub dev: u64,
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
    /// Entries read one at a time, until their block has been missed
    /// [`MISSES`] times.
    singles: RefCell<HashMap<u32, (Node, Vec<u8>)>>,
    /// Each block's entries read one at a time, by the block's first id.
    misses: RefCell<HashMap<u32, u8>>,
    /// The entry read alone last: the next one beside it is a walk.
    last_miss: std::cell::Cell<Option<u32>>,
    /// The first read that failed after the file opened.
    failed: RefCell<Option<String>>,
}

impl Index {
    /// Opens the index at `path` read-only. A file that is not an index,
    /// is damaged, or has a schema no duscope writes is
    /// [`CoreError::IndexCorrupt`]; one of another major version
    /// [`CoreError::IndexVersion`].
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
        harden(&db).map_err(|e| corrupt(e.to_string()))?;
        check_schema(&db).map_err(corrupt)?;
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
        let tool = meta.text("tool_version");
        if !is_version(&tool) {
            return Err(corrupt(format!(
                "scan_meta.tool_version {tool:?} is not a duscope version"
            )));
        }
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
            singles: RefCell::new(HashMap::new()),
            misses: RefCell::new(HashMap::new()),
            last_miss: std::cell::Cell::new(None),
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

    /// The rows of `table`, one of the layout's; `None` when the file has
    /// no such table (an index of another minor). `dirs` and `files` are
    /// the root's counts, which walking either table would only repeat.
    pub fn rows(&self, table: &str) -> Result<Option<u64>, CoreError> {
        let corrupt = |e: rusqlite::Error| CoreError::IndexCorrupt {
            file: self.path.display().to_string(),
            message: e.to_string(),
        };
        let present: bool = self
            .db
            .query_row(
                "SELECT count(*) FROM sqlite_schema WHERE type = 'table' AND name = ?1",
                [table],
                |row| row.get(0),
            )
            .map_err(corrupt)?;
        if !present {
            return Ok(None);
        }
        match table {
            "dirs" => return Ok(Some(self.node(0).dir_count)),
            "files" => return Ok(Some(self.node(0).file_count)),
            _ => {}
        }
        // The name comes from the built-in layout, never from a user.
        let count: i64 = self
            .db
            .query_row(&format!("SELECT count(*) FROM \"{table}\""), [], |row| {
                row.get(0)
            })
            .map_err(corrupt)?;
        Ok(Some(unsigned(count)))
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
                 free_bytes, options, dev FROM scan_volumes ORDER BY rowid",
            )?;
            let rows = stmt.query_map([], |row| {
                let options: Option<String> = row.get(7)?;
                Ok(IndexVolume {
                    dev: unsigned(row.get(8)?),
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
        if self.meta.excluded_count == 0 {
            return Vec::new();
        }
        let read = || -> rusqlite::Result<Vec<(u32, &'static str)>> {
            // The mask as the partial index `dirs_excluded` spells it, so
            // the database reads that index rather than every row.
            const _: () = assert!(EXCLUDED | EXCLUDED_CACHE == 3072);
            let mut stmt = self
                .db
                .prepare("SELECT id, flags FROM dirs WHERE flags & 3072 != 0 ORDER BY id")?;
            let rows = stmt.query_map([], |row| {
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
        if self.meta.error_count == 0 {
            return 0;
        }
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

    /// The directories with ids from `first` to `last` that could not be
    /// read, and the entries they listed but could not examine.
    pub fn errors_under(&self, first: u32, last: u32) -> u64 {
        if self.meta.error_count == 0 {
            return 0;
        }
        let entries = self
            .db
            .query_row(
                "SELECT COUNT(*) FROM scan_errors WHERE dir_id BETWEEN ?1 AND ?2",
                [first, last],
                |row| row.get::<_, i64>(0),
            )
            .map_or_else(
                |e| {
                    self.fail(&e);
                    0
                },
                unsigned,
            );
        self.unreadable_dirs(first, last) + entries
    }

    /// What the scan could not read: each unreadable directory (its id, no
    /// name, and the error number), and each entry a directory listed but
    /// could not examine (the directory's id, the entry's name, the error
    /// number).
    pub fn scan_errors(&self) -> Result<Vec<ScanErrorRow>, CoreError> {
        let read = || -> rusqlite::Result<Vec<ScanErrorRow>> {
            let mut out: Vec<ScanErrorRow> = self
                .db
                .prepare("SELECT id, errors FROM dirs WHERE errors IS NOT NULL")?
                .query_map([], |row| Ok((row.get(0)?, None, row.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            let entries = self
                .db
                .prepare("SELECT dir_id, name, errno FROM scan_errors")?
                .query_map([], |row| Ok((row.get(0)?, Some(row.get(1)?), row.get(2)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            out.extend(entries);
            Ok(out)
        };
        read().map_err(|e| self.corrupt(&e))
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

    /// Keeps a failed query's error as the first failed read, when it is.
    pub(crate) fn record(&self, e: &CoreError) {
        let mut failed = self.failed.borrow_mut();
        if failed.is_none() {
            *failed = Some(e.to_string());
        }
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

    /// Entry `id`'s node and name: from a block kept, or an entry read
    /// alone; read alone unless the entry read alone before it is its
    /// neighbour (a walk) or its block has been missed [`MISSES`] times;
    /// else from its block, read and kept first.
    fn entry<T>(&self, id: u32, f: impl FnOnce(&(Node, Vec<u8>)) -> T) -> Option<T> {
        if id >= self.len {
            self.fail(&rusqlite::Error::QueryReturnedNoRows);
            return None;
        }
        let start = id - id % BLOCK;
        let mut blocks = self.blocks.borrow_mut();
        if let Some(at) = blocks.iter().position(|b| b.start == start) {
            let block = blocks.remove(at);
            let found = block.entries.get((id - start) as usize).map(f);
            blocks.insert(0, block);
            return found;
        }
        if let Some(entry) = self.singles.borrow().get(&id) {
            return Some(f(entry));
        }
        let walk = self
            .last_miss
            .replace(Some(id))
            .is_some_and(|last| last.abs_diff(id) == 1);
        let alone = !walk && {
            let mut misses = self.misses.borrow_mut();
            if misses.len() >= SINGLES {
                misses.clear();
            }
            let count = misses.entry(start).or_insert(0);
            *count = count.saturating_add(1);
            *count < MISSES
        };
        if alone {
            drop(blocks);
            match self.read_one(id) {
                Ok(entry) => {
                    let found = f(&entry);
                    let mut singles = self.singles.borrow_mut();
                    if singles.len() >= SINGLES {
                        singles.clear();
                    }
                    singles.insert(id, entry);
                    return Some(found);
                }
                Err(e) => {
                    self.fail(&e);
                    return None;
                }
            }
        }
        let block = match self.read_block(start) {
            Ok(block) => block,
            Err(e) => {
                drop(blocks);
                self.fail(&e);
                return None;
            }
        };
        blocks.truncate(BLOCKS - 1);
        let found = block.entries.get((id - start) as usize).map(f);
        blocks.insert(0, block);
        found
    }

    /// Entry `id` alone, from whichever table holds it.
    fn read_one(&self, id: u32) -> rusqlite::Result<(Node, Vec<u8>)> {
        let dir = self
            .db
            .prepare(&format!("SELECT {DIR_COLUMNS} FROM dirs WHERE id = ?1"))?
            .query_row([id], |row| Ok((dir_from_row(id, row)?, row.get(14)?)))
            .optional()?;
        if let Some(entry) = dir {
            return Ok(entry);
        }
        self.db
            .prepare(&format!(
                "SELECT {FILE_COLUMNS} FROM files AS f LEFT JOIN dirs AS d ON d.id = f.parent_id \
                 WHERE f.id = ?1"
            ))?
            .query_row([id], |row| Ok((file_from_row(id, row)?, row.get(14)?)))
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
        let mut dirs = self.db.prepare(&format!(
            "SELECT {DIR_COLUMNS} FROM dirs WHERE id >= ?1 AND id < ?2"
        ))?;
        let mut rows = dirs.query([start, end])?;
        while let Some(row) = rows.next()? {
            let id: u32 = row.get(0)?;
            put(id, (dir_from_row(id, row)?, row.get(14)?));
        }
        let mut files = self.db.prepare(&format!(
            "SELECT {FILE_COLUMNS} FROM files AS f LEFT JOIN dirs AS d ON d.id = f.parent_id \
             WHERE f.id >= ?1 AND f.id < ?2"
        ))?;
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

    fn as_index(&self) -> Option<(&Index, u32)> {
        Some((self, 0))
    }

    /// From `dirs.dev`: a directory on another device than its parent's.
    fn volume_roots(&self, dir: u32) -> Vec<crate::query::VolumeRoot> {
        let last = self.node(dir).last_id;
        let read = || -> rusqlite::Result<Vec<crate::query::VolumeRoot>> {
            let mut stmt = self.db.prepare(
                "SELECT d.id, d.last_id, d.dev, d.cum_allocated, d.cum_apparent, \
                 d.cum_du_compatible FROM dirs AS d LEFT JOIN dirs AS p ON p.id = d.parent_id \
                 WHERE d.id BETWEEN ?1 AND ?2 AND (d.id = ?1 OR p.dev IS NOT d.dev) ORDER BY d.id",
            )?;
            stmt.query_map([dir, last], |row| {
                Ok(crate::query::VolumeRoot {
                    id: row.get(0)?,
                    last: row.get(1)?,
                    dev: unsigned(row.get(2)?),
                    allocated: unsigned(row.get(3)?),
                    apparent: unsigned(row.get(4)?),
                    du_compatible: unsigned(row.get(5)?),
                })
            })?
            .collect()
        };
        read().unwrap_or_else(|e| {
            self.fail(&e);
            Vec::new()
        })
    }

    /// Through the partial index `files_dataless`, whose condition the query
    /// spells as it is; a later link of a dataless file counts once.
    fn dataless_bytes(&self, dir: u32) -> u64 {
        const _: () = assert!(Flags::DATALESS.bits() == 4);
        let last = self.node(dir).last_id;
        self.db
            .query_row(
                &format!(
                    "SELECT COALESCE(SUM(apparent), 0) FROM files \
                     WHERE id BETWEEN ?1 AND ?2 AND flags & 4 != 0 AND flags & {LATER_LINK} = 0"
                ),
                rusqlite::params![dir, last],
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

    /// From `dirs_parent_name`: a directory's subdirectories are rows that
    /// name it their parent, where the default hops from sibling to sibling
    /// through blocks far apart.
    fn child_dirs(&self, dir: u32) -> Vec<(u32, u32)> {
        let read = || -> rusqlite::Result<Vec<(u32, u32)>> {
            let mut stmt = self
                .db
                .prepare("SELECT id, last_id FROM dirs WHERE parent_id = ?1 ORDER BY id")?;
            stmt.query_map([dir], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect()
        };
        read().unwrap_or_else(|e| {
            self.fail(&e);
            Vec::new()
        })
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
pub(crate) fn uri(path: &Path) -> String {
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

/// Sets what every index connection runs under before its first statement:
/// defensive, the schema untrusted, views and triggers off, and the limits.
fn harden(db: &Connection) -> rusqlite::Result<()> {
    db.set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)?;
    db.set_db_config(DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA, false)?;
    db.set_db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_VIEW, false)?;
    db.set_db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_TRIGGER, false)?;
    db.set_limit(Limit::SQLITE_LIMIT_SQL_LENGTH, SQL_LENGTH)?;
    db.set_limit(Limit::SQLITE_LIMIT_LENGTH, VALUE_LENGTH)?;
    db.set_limit(Limit::SQLITE_LIMIT_ATTACHED, 0)?;
    Ok(())
}

/// Loads the schema, the connection's first statement, and refuses one no
/// duscope writes: why, if it does. A progress handler stops a schema too
/// large to be an index while it loads.
fn check_schema(db: &Connection) -> Result<(), String> {
    let calls = Arc::new(AtomicU32::new(0));
    let counted = Arc::clone(&calls);
    db.progress_handler(
        STEPS_PER_CALL,
        Some(move || counted.fetch_add(1, Ordering::Relaxed) >= SCHEMA_CALLS),
    )
    .map_err(|e| e.to_string())?;
    let too_large = || format!("its schema has more than {SCHEMA_OBJECTS} objects");
    let schema = schema_objects(db);
    db.progress_handler(0, None::<fn() -> bool>)
        .map_err(|e| e.to_string())?;
    let (objects, virtual_table) = schema.map_err(|e| {
        if calls.load(Ordering::Relaxed) >= SCHEMA_CALLS {
            too_large()
        } else {
            e.to_string()
        }
    })?;
    if objects > SCHEMA_OBJECTS {
        return Err(too_large());
    }
    match virtual_table {
        Some(name) => Err(format!("its schema has a virtual table, {name}")),
        None => Ok(()),
    }
}

/// The schema's objects, counted up to one past [`SCHEMA_OBJECTS`], and
/// the first virtual table among them. A virtual table is told from its
/// text, since asking `SQLite` about one (`pragma_table_list`,
/// `table_info`) connects it, which runs its module on the file's data.
fn schema_objects(db: &Connection) -> rusqlite::Result<(i64, Option<String>)> {
    let mut stmt = db.prepare("SELECT type, name, sql FROM sqlite_schema")?;
    let mut rows = stmt.query([])?;
    let mut objects = 0;
    let mut virtual_table = None;
    while let Some(row) = rows.next()? {
        objects += 1;
        if objects > SCHEMA_OBJECTS {
            break;
        }
        let sql: Option<String> = row.get(2)?;
        if virtual_table.is_none()
            && row.get::<_, String>(0)? == "table"
            && sql.as_deref().is_some_and(creates_virtual_table)
        {
            virtual_table = Some(row.get::<_, String>(1)?);
        }
    }
    Ok((objects, virtual_table))
}

/// Whether `sql`, a statement of the schema, creates a virtual table:
/// `CREATE` then `VIRTUAL`, in any case, with any blanks and comments
/// between them, as `SQLite`'s tokenizer reads it.
fn creates_virtual_table(sql: &str) -> bool {
    keyword(sql, "CREATE").is_some_and(|rest| keyword(blanks(rest), "VIRTUAL").is_some())
}

/// `sql` past its leading blanks and comments: the characters `SQLite`'s
/// tokenizer takes for space, `--` to the end of the line, and `/* */`
/// (to the end, unclosed).
fn blanks(mut sql: &str) -> &str {
    loop {
        let rest = sql.trim_start_matches([' ', '\t', '\n', '\x0c', '\r']);
        if let Some(comment) = rest.strip_prefix("--") {
            sql = comment.find('\n').map_or("", |at| &comment[at..]);
        } else if let Some(comment) = rest.strip_prefix("/*") {
            sql = comment.find("*/").map_or("", |at| &comment[at + 2..]);
        } else {
            return rest;
        }
    }
}

/// `sql` past the keyword `word` at its start, in any case, when no
/// character of an identifier follows it.
fn keyword<'a>(sql: &'a str, word: &str) -> Option<&'a str> {
    let head = sql.get(..word.len())?;
    let rest = &sql[word.len()..];
    let identifier = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '$' || !c.is_ascii();
    (head.eq_ignore_ascii_case(word) && !rest.starts_with(identifier)).then_some(rest)
}

/// Whether `text` is a version as duscope writes `tool_version`:
/// `major.minor.patch`, perhaps with a pre-release or build suffix.
fn is_version(text: &str) -> bool {
    let core = text.split(['-', '+']).next().unwrap_or("");
    let parts: Vec<&str> = core.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
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
