//! The bulk writer: a scan's index, written once, after the scan.
//!
//! The file is built as `scans/<id>.db.tmp-<pid>` beside its final name, with
//! no journal and no syncing while it is written: the tables, then every row
//! in one transaction of prepared inserts, then the indexes, which cost less
//! built once than kept up to date row by row. The file is then synced and
//! renamed into place, so a crash or a kill leaves no `<id>.db`, only a
//! temporary file `scans --prune` removes. The rows come from the arena and
//! its roll-up, which numbered the entries and charged the hard links and
//! clones (DEV-36).

use std::collections::HashMap;
use std::fmt::Display;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use duscope_walk::{Attrs, Boundary, DirId, Skip};
use rusqlite::{Connection, Statement, Transaction, params};

use super::scanid::ScanId;
use super::schema_parts;
use crate::CoreError;
use crate::arena::{Arena, DirRec, EntryIdx};
use crate::query::kind_name;
use crate::rollup::RollUp;
use crate::rules::ext;

/// Flag bit 8 of `dirs.flags`: the directory is a firmlink into the Data
/// volume (`boundary = firmlink`), beside the eight bits of `Flags`
/// (DEV-64).
pub const FIRMLINK: u32 = 1 << 8;

/// Flag bit 9 of `dirs.flags`: the directory's totals are an upper bound,
/// since a file under it may share blocks with another that no clone id
/// names (DEV-68).
pub const UPPER_BOUND: u32 = 1 << 9;

/// Flag bit 10 of `dirs.flags`: the scan listed the directory but did not
/// enter it, because an exclude pattern matched it (DEV-68).
pub const EXCLUDED: u32 = 1 << 10;

/// Flag bit 11 of `dirs.flags`: the scan listed the directory but did not
/// enter it, because a cache rule matched it (`--exclude-caches`; DEV-68).
pub const EXCLUDED_CACHE: u32 = 1 << 11;

/// Flag bit 12 of `files.flags`: a later link of a hard-linked inode, whose
/// bytes, apparent ones included, count at its first link (DEV-68).
pub const LATER_LINK: u32 = 1 << 12;

/// What `scan_meta` records that the arena does not hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanMeta {
    /// The scan's id, which names the file.
    pub scan_id: ScanId,
    /// The root, symbolic links resolved.
    pub root: Vec<u8>,
    /// The root as given.
    pub root_as_given: Vec<u8>,
    /// When the scan started, Unix nanoseconds.
    pub started_at: i64,
    /// When it finished.
    pub finished_at: i64,
    /// The scan's options as JSON, for a rescan.
    pub options: String,
    /// The operating system and its kernel or Darwin version.
    pub platform: String,
    /// The user id the scan ran as.
    pub scanning_uid: u32,
    /// The worker threads the walk ran.
    pub thread_count: u64,
    /// The size output shows by default: `allocated` or `apparent`.
    pub size_kind: &'static str,
    /// The size of the root's volume.
    pub volume_total: u64,
    /// The bytes free on it for an unprivileged user.
    pub volume_free: u64,
    /// For a root that is a mount point, the used bytes the scan did not
    /// find.
    pub unaccounted_bytes: Option<u64>,
    /// Directories and entries the scan could not read.
    pub error_count: u64,
    /// Entries that disappeared during the scan.
    pub vanished_count: u64,
    /// Files left out and directories not entered by the excludes.
    pub excluded_count: u64,
    /// Whether an incremental rescan wrote the file.
    pub incremental: bool,
    /// The scan a rescan started from.
    pub parent_scan_id: Option<String>,
    /// `scan`, or `import:<format>`.
    pub source: String,
    /// The duscope version writing the file.
    pub tool_version: String,
}

/// The index file of scan `id` under the index directory `dir`.
pub fn index_path(dir: &Path, id: &ScanId) -> PathBuf {
    dir.join("scans").join(format!("{id}.db"))
}

/// Writes indexes.
#[derive(Debug)]
pub struct IndexWriter;

impl IndexWriter {
    /// Writes the index of a rolled-up arena under the index directory
    /// `dir` (`scans/<id>.db`, mode 0600 in a 0700 directory) and returns
    /// its id. On failure no file is left behind.
    pub fn write(
        arena: &Arena,
        rollup: &RollUp,
        meta: &ScanMeta,
        dir: &Path,
    ) -> Result<ScanId, CoreError> {
        let scans = dir.join("scans");
        let path = index_path(dir, &meta.scan_id);
        let fail = |e: &dyn Display| CoreError::IndexWrite {
            file: path.display().to_string(),
            message: e.to_string(),
        };
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&scans)
            .map_err(|e| fail(&e))?;
        let tmp = scans.join(format!("{}.db.tmp-{}", meta.scan_id, std::process::id()));
        let written = write_file(arena, rollup, meta, &tmp)
            .map_err(|e| fail(&e))
            .and_then(|()| {
                kill_before_rename();
                fs::rename(&tmp, &path).map_err(|e| fail(&e))?;
                // The rename itself is durable once the directory is synced.
                File::open(&scans)
                    .and_then(|d| d.sync_all())
                    .map_err(|e| fail(&e))
            });
        if written.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        written.map(|()| meta.scan_id.clone())
    }
}

/// `DUSCOPE_TEST_KILL_BEFORE_RENAME`, in a debug build: the process dies
/// between the write and the rename, as a kill would.
fn kill_before_rename() {
    if cfg!(debug_assertions) && std::env::var_os("DUSCOPE_TEST_KILL_BEFORE_RENAME").is_some() {
        std::process::abort();
    }
}

/// Builds the whole file at `tmp`, synced, ready to rename.
fn write_file(arena: &Arena, rollup: &RollUp, meta: &ScanMeta, tmp: &Path) -> Result<(), String> {
    let text = |e: &dyn Display| e.to_string();
    // Created empty first, so the file is 0600 from the start whatever the
    // umask; SQLite takes an empty file as an empty database.
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(tmp)
        .map_err(|e| text(&e))?;
    let mut db = Connection::open(tmp).map_err(|e| text(&e))?;
    db.execute_batch(
        "PRAGMA journal_mode = OFF; PRAGMA synchronous = OFF; \
         PRAGMA locking_mode = EXCLUSIVE; PRAGMA temp_store = MEMORY; \
         PRAGMA cache_size = -65536;",
    )
    .map_err(|e| text(&e))?;
    let (tables, indexes) = schema_parts();
    let tx = db.transaction().map_err(|e| text(&e))?;
    tx.execute_batch(tables).map_err(|e| text(&e))?;
    rows(&tx, arena, rollup, meta).map_err(|e| text(&e))?;
    tx.commit().map_err(|e| text(&e))?;
    db.execute_batch(indexes).map_err(|e| text(&e))?;
    db.close().map_err(|(_, e)| text(&e))?;
    File::open(tmp)
        .and_then(|f| f.sync_all())
        .map_err(|e| text(&e))
}

/// Every row of the index.
fn rows(
    tx: &Transaction<'_>,
    arena: &Arena,
    rollup: &RollUp,
    meta: &ScanMeta,
) -> rusqlite::Result<()> {
    meta_rows(tx, arena, rollup, meta)?;
    volume_rows(tx, arena)?;
    entry_rows(tx, arena, rollup, meta)?;
    let charges = rollup.charges();
    let first = |members: &[EntryIdx]| {
        members
            .iter()
            .map(|&e| rollup.id_of(e))
            .min()
            .unwrap_or_default()
    };
    let mut link = tx.prepare("INSERT INTO hardlinks VALUES (?1, ?2, ?3, ?4, ?5)")?;
    for group in charges.hardlinks(arena) {
        link.execute(params![
            signed(group.dev),
            signed(group.ino),
            first(&group.sites),
            group.nlink,
            count(group.sites.len()),
        ])?;
    }
    let mut clone = tx.prepare("INSERT INTO clones VALUES (?1, ?2, ?3)")?;
    for group in charges.clones(arena) {
        clone.execute(params![
            signed(group.dev),
            signed(group.clone_id),
            first(&group.members)
        ])?;
    }
    let mut error = tx.prepare("INSERT OR IGNORE INTO scan_errors VALUES (?1, ?2, ?3)")?;
    for e in arena.errors() {
        if arena.has_dir(e.dir) {
            let dir = rollup.id_of(EntryIdx::from_raw(arena.dir(e.dir).entry));
            error.execute(params![dir, e.name.as_bytes(), e.errno])?;
        }
    }
    Ok(())
}

/// `scan_meta`: the keys after `index_version`, which the DDL writes.
fn meta_rows(
    tx: &Transaction<'_>,
    arena: &Arena,
    rollup: &RollUp,
    meta: &ScanMeta,
) -> rusqlite::Result<()> {
    let mut row = tx.prepare("INSERT INTO scan_meta VALUES (?1, ?2)")?;
    let attrs = match arena.attrs() {
        Attrs::Basic => "basic",
        Attrs::Full => "full",
    };
    row.execute(params!["scan_id", meta.scan_id.as_str()])?;
    row.execute(params!["root", meta.root])?;
    row.execute(params!["root_as_given", meta.root_as_given])?;
    row.execute(params!["started_at", meta.started_at])?;
    row.execute(params!["finished_at", meta.finished_at])?;
    row.execute(params!["attrs", attrs])?;
    row.execute(params!["options", meta.options])?;
    row.execute(params!["platform", meta.platform])?;
    row.execute(params!["scanning_uid", meta.scanning_uid])?;
    row.execute(params!["thread_count", signed(meta.thread_count)])?;
    row.execute(params!["size_kind", meta.size_kind])?;
    row.execute(params!["volume_total", signed(meta.volume_total)])?;
    row.execute(params!["volume_free", signed(meta.volume_free)])?;
    row.execute(params![
        "unaccounted_bytes",
        meta.unaccounted_bytes.map(signed)
    ])?;
    row.execute(params!["error_count", signed(meta.error_count)])?;
    row.execute(params!["vanished_count", signed(meta.vanished_count)])?;
    row.execute(params!["excluded_count", signed(meta.excluded_count)])?;
    row.execute(params!["n_entries", count(rollup.len())])?;
    row.execute(params!["incremental", meta.incremental])?;
    row.execute(params!["parent_scan_id", meta.parent_scan_id])?;
    row.execute(params!["source", meta.source])?;
    row.execute(params!["tool_version", meta.tool_version])?;
    row.execute(params!["sibling_order", "bytewise"])?;
    Ok(())
}

/// `scan_volumes`: every volume the walk met.
fn volume_rows(tx: &Transaction<'_>, arena: &Arena) -> rusqlite::Result<()> {
    let mut row = tx.prepare(
        "INSERT INTO scan_volumes VALUES \
         (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL, NULL, NULL, ?10, ?11, ?12)",
    )?;
    for v in arena.volumes() {
        row.execute(params![
            signed(v.dev),
            signed(v.mnt_id),
            v.mount_point.as_os_str().as_encoded_bytes(),
            v.fs_type,
            v.read_only,
            v.entered(),
            v.reason.map(Skip::as_str),
            v.case_sensitive,
            v.normalization.as_str(),
            signed(v.total_bytes),
            signed(v.free_bytes),
            v.options.join(","),
        ])?;
    }
    Ok(())
}

/// One extension's tally for `type_stats`.
#[derive(Default)]
struct Tally {
    n: u64,
    allocated: u64,
    apparent: u64,
}

/// `dirs`, `files` and `type_stats`, in pre-order.
fn entry_rows(
    tx: &Transaction<'_>,
    arena: &Arena,
    rollup: &RollUp,
    meta: &ScanMeta,
) -> rusqlite::Result<()> {
    let mut dir_row = tx.prepare(
        "INSERT INTO dirs VALUES \
         (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)",
    )?;
    let mut file_row = tx.prepare(
        "INSERT INTO files VALUES \
         (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
    )?;
    let charges = rollup.charges();
    // The device and the boundary of each directory where one starts.
    let crossings: HashMap<EntryIdx, (u64, Boundary)> = arena
        .crossings()
        .iter()
        .map(|c| (c.entry, (c.dev, c.boundary)))
        .collect();
    let mut devs: HashMap<DirId, u64> = HashMap::new();
    let mut types: HashMap<String, Tally> = HashMap::new();
    for (id, idx) in rollup.preorder().enumerate() {
        let id = u32::try_from(id).unwrap_or(u32::MAX);
        let view = arena.entry(idx);
        let parent =
            (id != 0).then(|| rollup.id_of(EntryIdx::from_raw(arena.dir(view.parent).entry)));
        if let Some(dir) = view.dir {
            let crossing = crossings.get(&idx);
            let dev = crossing.map_or_else(
                || {
                    devs.get(&view.parent)
                        .copied()
                        .unwrap_or(arena.root_volume().0)
                },
                |&(dev, _)| dev,
            );
            devs.insert(dir, dev);
            let firmlink = crossing.is_some_and(|&(_, b)| b == Boundary::Firmlink);
            let flags = dir_flags(arena, dir, view.flags.bits(), firmlink);
            let rec = arena.dir(dir);
            let name = if id == 0 { &meta.root[..] } else { view.name };
            insert_dir(
                &mut dir_row,
                &DirRow {
                    id,
                    last_id: rec.last_id,
                    files_end: rec.files_end,
                    parent,
                    depth: rec.depth,
                    name,
                    apparent: view.apparent,
                    allocated: view.allocated,
                    cum_apparent: rec.cum_apparent,
                    cum_allocated: rec.cum_allocated,
                    cum_du_compatible: charges.du_compatible(arena, dir),
                    n_files: rec.n_files,
                    n_dirs: rec.n_dirs,
                    mtime: view.mtime_ns,
                    ctime: view.ctime_ns,
                    max_mtime: rec.max_mtime_ns,
                    dev,
                    flags,
                    errors: arena.unreadable().get(&dir).copied(),
                },
            )?;
            continue;
        }
        let full = arena.full(idx);
        let is_charged = charges.charged(idx);
        let later_link = charges.linked_away(idx);
        let suffix = ext(view.name);
        // Counted as the totals count: a hard link once, a clone's bytes on
        // disk once and its apparent bytes at every member.
        if !later_link {
            let tally = types.entry(suffix.clone()).or_default();
            tally.n += 1;
            tally.apparent += view.apparent;
            if is_charged {
                tally.allocated += view.allocated;
            }
        }
        file_row.execute(params![
            id,
            parent.unwrap_or(0),
            view.name,
            signed(view.apparent),
            signed(view.allocated),
            is_charged,
            view.mtime_ns,
            view.ctime_ns,
            full.map(|f| f.atime_ns),
            full.and_then(|f| f.btime_ns),
            full.map(|f| f.mode),
            full.map(|f| f.uid),
            full.map(|f| f.gid),
            view.nlink,
            kind_name(view.kind),
            view.flags.bits() | if later_link { LATER_LINK } else { 0 },
            suffix,
        ])?;
    }
    type_rows(tx, &types)
}

/// A directory's `flags`: its storage flags (`own`), then the bits the
/// index adds: a firmlink, totals that are an upper bound, and an exclude
/// that kept the scan out.
fn dir_flags(arena: &Arena, dir: DirId, own: u32, firmlink: bool) -> u32 {
    let mut flags = own;
    if firmlink {
        flags |= FIRMLINK;
    }
    if arena.dir(dir).flags & DirRec::UPPER_BOUND != 0 {
        flags |= UPPER_BOUND;
    }
    match arena.skipped().get(&dir) {
        Some(Skip::Excluded) => flags |= EXCLUDED,
        Some(Skip::ExcludedCache) => flags |= EXCLUDED_CACHE,
        _ => {}
    }
    flags
}

/// `type_stats`: each extension's tally.
fn type_rows(tx: &Transaction<'_>, types: &HashMap<String, Tally>) -> rusqlite::Result<()> {
    let mut row = tx.prepare("INSERT INTO type_stats VALUES (?1, ?2, ?3, ?4)")?;
    for (suffix, t) in types {
        row.execute(params![
            suffix,
            signed(t.n),
            signed(t.allocated),
            signed(t.apparent)
        ])?;
    }
    Ok(())
}

/// A `dirs` row.
struct DirRow<'a> {
    id: u32,
    last_id: u32,
    files_end: u32,
    parent: Option<u32>,
    depth: u16,
    name: &'a [u8],
    apparent: u64,
    allocated: u64,
    cum_apparent: u64,
    cum_allocated: u64,
    cum_du_compatible: u64,
    n_files: u32,
    n_dirs: u32,
    mtime: i64,
    ctime: i64,
    max_mtime: i64,
    dev: u64,
    flags: u32,
    errors: Option<i32>,
}

fn insert_dir(stmt: &mut Statement<'_>, r: &DirRow<'_>) -> rusqlite::Result<usize> {
    stmt.execute(params![
        r.id,
        r.last_id,
        r.files_end,
        r.parent,
        r.depth,
        r.name,
        signed(r.apparent),
        signed(r.allocated),
        signed(r.cum_apparent),
        signed(r.cum_allocated),
        signed(r.cum_du_compatible),
        r.n_files,
        r.n_dirs,
        r.mtime,
        r.ctime,
        r.max_mtime,
        signed(r.dev),
        r.flags,
        r.errors,
    ])
}

/// A count, as the index's signed 64-bit integer.
fn count(n: usize) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

/// An unsigned 64-bit value as the index stores it: the signed integer with
/// the same bits (`docs/index-format.md`, Conventions).
#[allow(clippy::cast_possible_wrap)]
fn signed(value: u64) -> i64 {
    value as i64
}

#[cfg(test)]
mod tests;
