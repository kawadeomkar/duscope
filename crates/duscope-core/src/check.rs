//! The staleness sweep of `scans --check`: every directory a saved scan
//! holds, its modification and status-change times read again through
//! [`duscope_walk::stamp_dirs`], the later of the two compared with the
//! scan's for exact equality. A directory whose names changed (an entry
//! added, removed or renamed) or whose inode did (its mode, owner, links)
//! differs; one that is gone, is no longer a directory, or cannot be read
//! differs too, since nothing then says it is as the scan found it. A file
//! written in place under an unchanged directory does not: the sweep is
//! structural, and costs one call per directory, never a rescan. A change
//! that lands within the filesystem's timestamp granularity of the
//! directory's previous one (a clock tick on a Linux kernel without
//! fine-grained timestamps) reads as none.
//!
//! A directory under a name no listing returns (empty, `.`, `..`, or with a
//! slash: an index file no scan wrote) is not read, and counts as changed,
//! so a crafted file cannot lead the sweep outside its root.
//!
//! The answer is only ever reported: nothing reads it to skip work.

use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::time::Instant;

use crate::CoreError;
use crate::index::Index;
use crate::query::TreeView;

/// Directories read per call to [`duscope_walk::stamp_dirs`]: the paths of
/// a chunk are held at once, never a whole scan's.
const CHUNK: usize = 4096;

/// What the sweep found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sweep {
    /// The directories read again.
    pub dir_count: u64,
    /// Those that differ from the scan.
    pub changed_count: u64,
    /// The first of them in the scan's order (pre-order: an ancestor before
    /// what lies under it).
    pub first_changed: Option<PathBuf>,
}

impl Sweep {
    /// Whether the tree changed since the scan: `structurally_stale`.
    pub fn stale(&self) -> bool {
        self.changed_count > 0
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

/// Sweeps every directory `index` holds, but those at or under one of
/// `leave_out`, absolute paths with no symlinks (the index directory, whose
/// files a save adds to).
///
/// # Errors
///
/// [`CoreError::IndexCorrupt`] when the index's directories cannot be read.
pub fn sweep(index: &Index, leave_out: &[PathBuf]) -> Result<Sweep, CoreError> {
    let started = Instant::now();
    let corrupt = |e: rusqlite::Error| CoreError::IndexCorrupt {
        file: index.file().display().to_string(),
        message: e.to_string(),
    };
    let root = index.root_path().to_owned();
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
            // The root, and only it, has no ancestor in the scan.
            None => root.clone(),
        };
        if leave_out.iter().any(|out| out == &path) {
            skip_to = Some(last_id);
            continue;
        }
        let mtime: i64 = row.get(3).map_err(corrupt)?;
        let ctime: i64 = row.get(4).map_err(corrupt)?;
        if last_id > id {
            ancestors.push((last_id, path.clone()));
        }
        chunk.push(Pending {
            path,
            latest_ns: unread_to.is_none().then_some(mtime.max(ctime)),
        });
        if chunk.len() == CHUNK {
            compare(&mut chunk, &mut found);
        }
    }
    compare(&mut chunk, &mut found);
    // The sweep's cost, the index read included, for -v.
    log::info!(
        "swept {} directories in {:.3} s: {} changed",
        found.dir_count,
        started.elapsed().as_secs_f64(),
        found.changed_count
    );
    Ok(found)
}

/// Reads the stamps of the directories in `chunk`, compares each with the
/// scan's, and empties it.
fn compare(chunk: &mut Vec<Pending>, found: &mut Sweep) {
    let read = chunk.iter().filter(|p| p.latest_ns.is_some());
    let mut stamps = duscope_walk::stamp_dirs(read.map(|p| p.path.as_path())).into_iter();
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

    use super::sweep;
    use crate::arena::Arena;
    use crate::index::{Index, IndexWriter, ScanId, ScanMeta, index_path};
    use crate::rollup::roll_up;

    /// Scans `root` and saves the scan under `dir`.
    fn save(root: &Path, dir: &Path) -> Index {
        let mut arena = Arena::new(Attrs::Basic);
        walk(root, &WalkOptions::default(), |event| {
            arena.push(event).unwrap();
        })
        .unwrap();
        let rollup = roll_up(&mut arena);
        let id = ScanId::parse("c4ec0000c4ec").unwrap();
        let root = root.as_os_str().as_bytes().to_vec();
        let meta = ScanMeta {
            scan_id: id.clone(),
            root: root.clone(),
            root_as_given: root,
            started_at: 0,
            finished_at: 0,
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
}
