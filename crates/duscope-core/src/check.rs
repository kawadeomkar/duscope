//! The staleness sweep of `scans --check`: every directory a saved scan
//! holds, its modification and status-change times read again through
//! [`duscope_walk::stamp_dirs`], the later of the two compared with the
//! scan's for exact equality. A directory whose names changed (an entry
//! added, removed or renamed) or whose inode did (its mode, owner, links)
//! differs; one that is gone, is no longer a directory, or cannot be read
//! differs too, since nothing then says it is as the scan found it. A file
//! written in place under an unchanged directory does not: the sweep is
//! structural, and costs one call per directory, never a rescan.
//!
//! A directory touched while the scan ran counts as changed, whatever it
//! reads now (mlocate's rule): its recorded time lies between the scan's
//! start, less two seconds, and its finish, plus two seconds. The
//! walk reads a directory's times before it lists the directory, and a
//! second change in the same clock tick as the first (a few milliseconds
//! on a Linux kernel without fine-grained timestamps, two seconds on FAT)
//! leaves the times as the walk read them: such a change would otherwise
//! read as none for good. The bounds are `scan_meta.started_at` and
//! `finished_at`, which every index holds; they are the system clock's,
//! but in a scan taken with the test clock (`--test-now`, or
//! `DUSCOPE_TEST_NOW` in a debug build), where the window lies around that
//! clock instead.
//!
//! A scan that followed symbolic links recorded a link to a directory as
//! the directory: its directories are read following links, as its walk
//! read them.
//!
//! A directory under a name no listing returns (empty, `.`, `..`, or with a
//! slash: an index file no scan wrote) is not read, and counts as changed,
//! so a crafted file cannot lead the sweep outside its root.
//!
//! Before any of that, every row of `dirs` and `files` is held to the
//! index's layout, as a query holds the rows it reads
//! (`rows_keep_the_layout`). A query reads only the rows its answer needs,
//! so a damaged row it never reaches goes unreported; here it does not, and
//! the verb fails `index_corrupt` with nothing read from the tree.
//!
//! The answer is only ever reported: nothing reads it to skip work.

use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::time::Instant;

use crate::CoreError;
use crate::index::Index;
use crate::index::reader::{dir_ids, file_ids, layout_error, misplaced, missing};
use crate::query::TreeView;

/// Directories read per call to [`duscope_walk::stamp_dirs`]: the paths of
/// a chunk are held at once, never a whole scan's.
const CHUNK: usize = 4096;

/// How far before the scan's start and after its finish a directory's
/// recorded time still counts as touched while the scan ran: the coarsest
/// timestamps a directory gets (two seconds for a modification on FAT, one
/// on HFS+ and on ext3's small inodes, a clock tick on a Linux kernel
/// without fine-grained timestamps), and a clock a little off between the
/// scan and the filesystem.
const TOUCHED_MARGIN_NS: i64 = 2_000_000_000;

/// What the sweep found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sweep {
    /// The directories compared: read again, or counted as changed unread.
    pub dir_count: u64,
    /// Those that differ from the scan, or were touched while it ran.
    pub changed_count: u64,
    /// The first of them in the scan's order (pre-order: an ancestor before
    /// what lies under it).
    pub first_changed: Option<PathBuf>,
}

impl Sweep {
    /// Whether the tree changed since the scan: `structurally_stale`. A
    /// sweep that compared no directory cannot say the tree is unchanged,
    /// and is stale too.
    pub fn stale(&self) -> bool {
        self.changed_count > 0 || self.dir_count == 0
    }
}

/// A directory waiting in a chunk: its path and the later of the times the
/// scan recorded; none for one not to be read, which counts as changed.
struct Pending {
    path: PathBuf,
    latest_ns: Option<i64>,
}

/// Whether a directory listing can return `name`: not empty, `.` or `..`,
/// and without a slash. A name it cannot comes from a file no scan wrote,
/// and would lead the sweep outside the root.
fn listable(name: &[u8]) -> bool {
    !name.is_empty() && name != b"." && name != b".." && !name.contains(&b'/')
}

/// Sweeps every directory `index` holds, but those strictly under its root
/// and at or under one of `leave_out`, absolute paths with no symlinks (the
/// index directory, whose files a save adds to). The root is always read:
/// where it lies at or under one of `leave_out`, it is the only directory
/// read.
///
/// # Errors
///
/// [`CoreError::IndexCorrupt`] when a row of `dirs` or `files` breaks the
/// index's layout or an id below `n_entries` has no row, or the index's
/// directories or its scan options cannot be read.
pub fn sweep(index: &Index, leave_out: &[PathBuf]) -> Result<Sweep, CoreError> {
    let started = Instant::now();
    let corrupt = |e: rusqlite::Error| CoreError::IndexCorrupt {
        file: index.file().display().to_string(),
        message: e.to_string(),
    };
    rows_keep_the_layout(index).map_err(corrupt)?;
    let (options, _) = crate::rescan::options(index)?;
    let meta = index.meta();
    // Touched while the scan ran, with the margin on either side.
    let touched = meta.started_at.saturating_sub(TOUCHED_MARGIN_NS)
        ..=meta.finished_at.saturating_add(TOUCHED_MARGIN_NS);
    let root = index.root_path().to_owned();
    // A root inside a directory left out: everything under it is too.
    let root_left_out = leave_out.iter().any(|out| root.starts_with(out));
    let mut found = Sweep {
        dir_count: 0,
        changed_count: 0,
        first_changed: None,
    };
    let mut stmt = index
        .db()
        .prepare("SELECT id, last_id, name, mtime, ctime FROM dirs ORDER BY id")
        .map_err(corrupt)?;
    let mut rows = stmt.query([]).map_err(corrupt)?;
    // The directories above the current one, each with the last id of its
    // subtree: pre-order puts every directory after its parent.
    let mut ancestors: Vec<(i64, PathBuf)> = Vec::new();
    // The last id of a subtree left out, while the rows are inside it.
    let mut skip_to: Option<i64> = None;
    // The last id of a subtree under a name no listing returns, while the
    // rows are inside it: none of it is read.
    let mut unread_to: Option<i64> = None;
    let mut chunk: Vec<Pending> = Vec::with_capacity(CHUNK);
    while let Some(row) = rows.next().map_err(corrupt)? {
        let id: i64 = row.get(0).map_err(corrupt)?;
        let last_id: i64 = row.get(1).map_err(corrupt)?;
        if skip_to.is_some_and(|last| id <= last) {
            continue;
        }
        skip_to = None;
        while ancestors.last().is_some_and(|(last, _)| *last < id) {
            ancestors.pop();
        }
        if unread_to.is_some_and(|last| id > last) {
            unread_to = None;
        }
        let path = match ancestors.last() {
            Some((_, parent)) => {
                let name: Vec<u8> = row.get(2).map_err(corrupt)?;
                if unread_to.is_none() && !listable(&name) {
                    unread_to = Some(last_id);
                }
                parent.join(std::ffi::OsStr::from_bytes(&name))
            }
            // The root, and only it, has no ancestor in the scan; it is
            // never left out.
            None => root.clone(),
        };
        if !ancestors.is_empty() && (root_left_out || leave_out.iter().any(|out| out == &path)) {
            skip_to = Some(last_id);
            continue;
        }
        let mtime: i64 = row.get(3).map_err(corrupt)?;
        let ctime: i64 = row.get(4).map_err(corrupt)?;
        if last_id > id {
            ancestors.push((last_id, path.clone()));
        }
        let latest = mtime.max(ctime);
        let during = touched.contains(&latest);
        if during {
            log::debug!("{}: touched while the scan ran", path.display());
        }
        chunk.push(Pending {
            path,
            latest_ns: (unread_to.is_none() && !during).then_some(latest),
        });
        if chunk.len() == CHUNK {
            compare(&mut chunk, options.follow_symlinks, &mut found);
        }
    }
    compare(&mut chunk, options.follow_symlinks, &mut found);
    // The sweep's cost, the index read included, for -v.
    log::info!(
        "swept {} directories in {:.3} s: {} changed",
        found.dir_count,
        started.elapsed().as_secs_f64(),
        found.changed_count
    );
    Ok(found)
}

/// Holds every row of `dirs` and `files` to the layout, through the
/// functions the reader holds a row to it with, so the two cannot disagree:
/// a NULL or a value that does not fit fails here as it does there. `NOT
/// INDEXED`: the rows come from the tables a query reads, never from a
/// covering index, whose content `PRAGMA quick_check` does not compare with
/// its table and a crafted file can make say anything.
///
/// The two tables are read side by side in id order, which is pre-order,
/// with the directories whose subtrees hold the current id open: the ids
/// must run from 0 to `n_entries - 1` with none left out and none in both
/// tables, and each entry below the root must lie where its parent's ids
/// place it (a file up to the parent's `files_end`, a directory after it
/// with its subtree inside the parent's), so that a walk over the ids or
/// down the tree meets nothing the reader would refuse.
pub(crate) fn rows_keep_the_layout(index: &Index) -> rusqlite::Result<()> {
    let db = index.db();
    let len = index.len();
    let mut dir_rows =
        db.prepare("SELECT id, parent_id, files_end, last_id FROM dirs NOT INDEXED ORDER BY id")?;
    let mut dirs = dir_rows.query([])?;
    let mut next_dir = || -> rusqlite::Result<Option<Row>> {
        let Some(row) = dirs.next()? else {
            return Ok(None);
        };
        let (id, parent, files_end, last) = (row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?);
        dir_ids(id, parent, files_end, last, len)?;
        Ok(Some(Row {
            id,
            parent,
            dir: Some((files_end, last)),
        }))
    };
    let mut file_rows = db.prepare("SELECT id, parent_id FROM files NOT INDEXED ORDER BY id")?;
    let mut files = file_rows.query([])?;
    let mut next_file = || -> rusqlite::Result<Option<Row>> {
        let Some(row) = files.next()? else {
            return Ok(None);
        };
        let (id, parent) = (row.get(0)?, row.get(1)?);
        file_ids(id, parent)?;
        Ok(Some(Row {
            id,
            parent: Some(parent),
            dir: None,
        }))
    };
    let (mut dir, mut file) = (next_dir()?, next_file()?);
    // The directories whose subtrees hold the id reached, the innermost
    // last: each one's id, `files_end` and `last_id`.
    let mut open: Vec<(u32, u32, u32)> = Vec::new();
    // The id the rows have reached.
    let mut expected = 0u32;
    loop {
        let row = match (&dir, &file) {
            (None, None) => break,
            (Some(d), Some(f)) if d.id == f.id => {
                return Err(layout_error(format!(
                    "dirs and files both have a row with id {}",
                    d.id
                )));
            }
            (Some(d), Some(f)) if d.id < f.id => dir.take(),
            (Some(_), None) => dir.take(),
            _ => file.take(),
        };
        let Some(row) = row else { break };
        if row.id != expected {
            return Err(missing(expected, len));
        }
        if row.id >= len {
            return Err(layout_error(format!(
                "a row has id {}, which is not below n_entries {len}",
                row.id
            )));
        }
        nests(&row, &mut open)?;
        if row.dir.is_some() {
            dir = next_dir()?;
        } else {
            file = next_file()?;
        }
        expected = row.id.saturating_add(1);
    }
    if expected < len {
        return Err(missing(expected, len));
    }
    Ok(())
}

/// A row of `dirs` or `files` as the check reads it: its id, its parent,
/// and a directory's `files_end` and `last_id`.
struct Row {
    id: u32,
    parent: Option<u32>,
    dir: Option<(u32, u32)>,
}

/// Holds `row`, the next in id order, to where the directories `open`
/// (each one's id, `files_end` and `last_id`, the innermost last) place
/// it, and opens it when it is a directory.
fn nests(row: &Row, open: &mut Vec<(u32, u32, u32)>) -> rusqlite::Result<()> {
    let id = row.id;
    let table = if row.dir.is_some() { "dirs" } else { "files" };
    while open.last().is_some_and(|&(_, _, last)| last < id) {
        open.pop();
    }
    let what = match (open.last(), row.dir) {
        // The root.
        (None, _) if id == 0 => None,
        (None, _) => Some("it lies past the root's last_id".to_owned()),
        (Some(&(parent, ..)), _) if row.parent != Some(parent) => Some(format!(
            "its parent_id {} is not {parent}, the directory whose subtree its id lies in",
            row.parent
                .map_or_else(|| "NULL".to_owned(), |p| p.to_string())
        )),
        (Some(&(_, files_end, _)), None) if id > files_end => Some(format!(
            "it is a file past its parent's files_end, {files_end}"
        )),
        (Some(&(_, files_end, _)), Some(_)) if id <= files_end => Some(format!(
            "it is a directory that its parent's files_end, {files_end}, takes in as a file"
        )),
        (Some(&(.., end)), Some((_, last))) if last > end => {
            Some(format!("its last_id {last} is past its parent's, {end}"))
        }
        _ => None,
    };
    if let Some(what) = what {
        return Err(misplaced(table, id, &what));
    }
    if let Some((files_end, last)) = row.dir {
        open.push((id, files_end, last));
    }
    Ok(())
}

/// Reads the stamps of the directories in `chunk`, following links when
/// the scan did, compares each with the scan's, and empties it.
fn compare(chunk: &mut Vec<Pending>, follow_symlinks: bool, found: &mut Sweep) {
    let read = chunk.iter().filter(|p| p.latest_ns.is_some());
    let mut stamps =
        duscope_walk::stamp_dirs(read.map(|p| p.path.as_path()), follow_symlinks).into_iter();
    for pending in chunk.drain(..) {
        found.dir_count += 1;
        // The stamps are of the directories read, in order.
        let same = pending.latest_ns.is_some_and(|latest| match stamps.next() {
            Some(Ok(stamp)) => stamp.latest_ns() == latest,
            Some(Err(e)) => {
                log::debug!("{}: {e}", pending.path.display());
                false
            }
            None => false,
        });
        if !same {
            if found.changed_count == 0 {
                found.first_changed = Some(pending.path);
            }
            found.changed_count += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs::{self, Permissions};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    use duscope_testkit::TestEnv;
    use duscope_walk::{Attrs, WalkOptions, walk};

    use super::{TOUCHED_MARGIN_NS, sweep};
    use crate::arena::Arena;
    use crate::index::{Index, IndexWriter, ScanId, ScanMeta, index_path};
    use crate::rollup::roll_up;

    /// Scans `root` and saves the scan under `dir`, as one that started
    /// and finished at the epoch, long before the tree was made.
    fn save(root: &Path, dir: &Path) -> Index {
        save_timed(root, dir, None)
    }

    /// Now, Unix nanoseconds.
    fn now_ns() -> i64 {
        let since = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap();
        i64::try_from(since.as_nanos()).unwrap()
    }

    /// Scans `root` and saves the scan under `dir`; with `started`, as a
    /// scan that started then and finished when the walk did.
    fn save_timed(root: &Path, dir: &Path, started: Option<i64>) -> Index {
        let mut arena = Arena::new(Attrs::Basic);
        walk(root, &WalkOptions::default(), |event| {
            arena.push(event).unwrap();
        })
        .unwrap();
        let finished = started.map_or(0, |_| now_ns());
        let rollup = roll_up(&mut arena);
        let id = ScanId::parse("c4ec0000c4ec").unwrap();
        let root = root.as_os_str().as_bytes().to_vec();
        let meta = ScanMeta {
            scan_id: id.clone(),
            root: root.clone(),
            root_as_given: root,
            started_at: started.unwrap_or(0),
            finished_at: finished,
            options: "{}".to_owned(),
            platform: duscope_walk::system(),
            scanning_uid: 0,
            thread_count: 1,
            size_kind: "allocated",
            volume_total: 0,
            volume_free: 0,
            unaccounted_bytes: None,
            error_count: 0,
            vanished_count: 0,
            excluded_count: 0,
            clones_unavailable: false,
            incremental: false,
            parent_scan_id: None,
            source: "scan".to_owned(),
            tool_version: "0.2.0".to_owned(),
        };
        IndexWriter::write(&arena, &rollup, &meta, dir).unwrap();
        Index::open(&index_path(dir, &id)).unwrap()
    }

    /// Waits out a clock tick, so a change after the scan gets a later time
    /// than the tree's own: Linux stamps times from a clock that moves once
    /// a tick (a few milliseconds), and the scan of a small tree takes less.
    fn settle() {
        std::thread::sleep(std::time::Duration::from_millis(25));
    }

    /// A tree of eight directories and a file.
    fn tree(env: &TestEnv) -> PathBuf {
        let root = env.work_dir().join("tree");
        for dir in ["a/b/c", "a/d", "e/f", "g"] {
            fs::create_dir_all(root.join(dir)).unwrap();
        }
        fs::write(root.join("a/b/file"), b"x").unwrap();
        root
    }

    #[test]
    fn p5_core_02_an_untouched_tree_sweeps_fresh() {
        let env = TestEnv::new();
        let root = tree(&env);
        let index = save(&root, &env.tmp_dir());
        let found = sweep(&index, &[]).unwrap();
        assert_eq!(found.dir_count, 8);
        assert_eq!(found.changed_count, 0);
        assert_eq!(found.first_changed, None);
        assert!(!found.stale());
        // Writing a file in place changes no directory: the sweep is
        // structural.
        fs::write(root.join("a/b/file"), b"longer now").unwrap();
        assert!(!sweep(&index, &[]).unwrap().stale());
    }

    #[test]
    fn p5_core_02_each_structural_change_makes_the_scan_stale() {
        let env = TestEnv::new();
        let root = tree(&env);
        let index = save(&root, &env.tmp_dir());
        settle();
        // Each change in its own directory: an entry added, a mode
        // changed, a directory removed, one replaced by a file.
        fs::write(root.join("a/b/c/new"), b"").unwrap();
        let found = sweep(&index, &[]).unwrap();
        assert_eq!(found.changed_count, 1);
        assert_eq!(found.first_changed.as_deref(), Some(&*root.join("a/b/c")));
        fs::set_permissions(root.join("a/d"), Permissions::from_mode(0o700)).unwrap();
        assert_eq!(sweep(&index, &[]).unwrap().changed_count, 2);
        // Gone: it and its parent, whose names changed.
        fs::remove_dir(root.join("e/f")).unwrap();
        let found = sweep(&index, &[]).unwrap();
        assert_eq!(found.changed_count, 4);
        assert_eq!(found.dir_count, 8);
        // Pre-order: the first changed is the earliest in the scan.
        assert_eq!(found.first_changed.as_deref(), Some(&*root.join("a/b/c")));
        fs::remove_dir(root.join("g")).unwrap();
        fs::write(root.join("g"), b"").unwrap();
        assert_eq!(sweep(&index, &[]).unwrap().changed_count, 6);
    }

    #[test]
    fn p5_core_02_a_name_no_listing_returns_is_never_read() {
        let env = TestEnv::new();
        let root = tree(&env);
        let dir = env.tmp_dir();
        let index = save(&root, &dir);
        let file = index.file().to_owned();
        drop(index);
        // A crafted file: `e` renamed to lead outside the root.
        for name in [&b".."[..], b"../..", b"", b"."] {
            let db = rusqlite::Connection::open(&file).unwrap();
            db.execute(
                "UPDATE dirs SET name = ?1 WHERE parent_id = 0 AND name IN (X'65', X'2e2e', X'2e2e2f2e2e', X'', X'2e')",
                [name],
            )
            .unwrap();
            drop(db);
            let index = Index::open(&file).unwrap();
            let found = sweep(&index, &[]).unwrap();
            // It and the directory under it count as changed, unread; the
            // rest reads fresh.
            assert_eq!((found.dir_count, found.changed_count), (8, 2), "{name:?}");
        }
    }

    #[test]
    fn p5_core_02_the_index_directory_under_the_root_is_left_out() {
        let env = TestEnv::new();
        let root = tree(&env);
        // The index directory inside the tree, made before the scan: the
        // save then adds its file there.
        let dir = root.join("e/index");
        fs::create_dir(&dir).unwrap();
        let index = save(&root, &dir);
        settle();
        let found = sweep(&index, std::slice::from_ref(&dir)).unwrap();
        assert!(!found.stale(), "{found:?}");
        // The index directory and its scans/ are not read.
        assert_eq!(found.dir_count, 8);
        // Unless left in: its scans/ came after the scan.
        let found = sweep(&index, &[]).unwrap();
        assert_eq!(found.first_changed.as_deref(), Some(&*dir));
        // Above it, a change still counts.
        fs::create_dir(root.join("e/new")).unwrap();
        let found = sweep(&index, std::slice::from_ref(&dir)).unwrap();
        assert_eq!(found.changed_count, 1);
        assert_eq!(found.first_changed.as_deref(), Some(&*root.join("e")));
    }

    #[test]
    fn p5_core_02_a_directory_touched_while_the_scan_ran_counts_as_changed() {
        let env = TestEnv::new();
        let root = tree(&env);
        // The tree older than the margin when the scan starts.
        let margin = std::time::Duration::from_nanos(TOUCHED_MARGIN_NS.unsigned_abs());
        std::thread::sleep(margin + std::time::Duration::from_millis(100));
        let started = now_ns();
        // Changed once the scan started, before the walk read it: the walk
        // records the new times, and a second change in the same clock tick
        // after the read would leave them as recorded.
        fs::write(root.join("a/b/new"), b"").unwrap();
        let index = save_timed(&root, &env.tmp_dir(), Some(started));
        let found = sweep(&index, &[]).unwrap();
        assert_eq!(found.dir_count, 8);
        assert_eq!(found.changed_count, 1, "{found:?}");
        assert_eq!(found.first_changed.as_deref(), Some(&*root.join("a/b")));
        assert!(found.stale());
    }

    #[test]
    fn p5_core_02_the_root_is_read_even_inside_a_directory_left_out() {
        let env = TestEnv::new();
        let root = tree(&env);
        let index = save(&root, &env.tmp_dir());
        settle();
        // The root is the index directory, or lies under it: the root is
        // read, and nothing under it.
        for out in [root.clone(), env.work_dir()] {
            let found = sweep(&index, std::slice::from_ref(&out)).unwrap();
            assert_eq!((found.dir_count, found.changed_count), (1, 0), "{out:?}");
            assert!(!found.stale());
        }
        fs::create_dir(root.join("new")).unwrap();
        let found = sweep(&index, std::slice::from_ref(&root)).unwrap();
        assert_eq!(found.first_changed.as_deref(), Some(&*root));
        assert!(found.stale());
    }

    #[test]
    fn p5_core_02_a_sweep_that_compares_no_directory_is_stale() {
        let env = TestEnv::new();
        let root = tree(&env);
        // A scan of a single file, which holds no directory. (A saved scan
        // whose `dirs` rows are deleted leaves ids that no row holds, which
        // `Index::open` refuses.)
        let index = save(&root.join("a/b/file"), &env.tmp_dir());
        assert_eq!(index.rows("dirs").unwrap(), Some(0));
        let found = sweep(&index, &[]).unwrap();
        assert_eq!((found.dir_count, found.changed_count), (0, 0));
        assert!(found.stale());
    }
}
