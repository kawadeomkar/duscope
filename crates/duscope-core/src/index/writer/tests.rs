use std::cell::{Cell, RefCell};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use duscope_walk::{Attrs, MemProvider, WalkOptions, walk_with};
use rusqlite::types::Value;
use rusqlite::{Connection, ErrorCode};

use super::{IndexWriter, ScanMeta, index_path, write_file_with, write_in};
use crate::CoreError;
use crate::arena::Arena;
use crate::index::{SCAN_META_KEYS, ScanId};
use crate::query::{ArenaView, TreeView};
use crate::rollup::{RollUp, roll_up};

/// A small tree: files and a symbolic link at the root, a directory with
/// files and a subdirectory, an unreadable directory, and an entry its
/// directory lists but cannot read (EACCES: ENOENT would be a vanished entry).
fn scanned() -> (Arena, RollUp) {
    let mut tree = MemProvider::new("/t");
    tree.file("z.txt", 3)
        .file("a.tar.gz", 5000)
        .symlink("link", 7)
        .dir("src")
        .file("src/main.rs", 1200)
        .file("src/lib.rs", 800)
        .dir("src/deep")
        .file("src/deep/data.bin", 70_000)
        .dir("closed")
        .unreadable("closed", 13)
        .file("gone", 1)
        .unstatable("gone", 13);
    let mut arena = Arena::new(Attrs::Basic);
    walk_with(&tree, Path::new("/t"), &WalkOptions::default(), |event| {
        arena.push(event).unwrap();
    })
    .unwrap();
    let rollup = roll_up(&mut arena);
    (arena, rollup)
}

fn meta(id: &str) -> ScanMeta {
    ScanMeta {
        scan_id: ScanId::new(id.as_bytes(), 1, [0; 8]),
        root: b"/t".to_vec(),
        root_as_given: b"t".to_vec(),
        started_at: 1_000,
        finished_at: 2_000,
        options: "{}".to_owned(),
        platform: "test".to_owned(),
        scanning_uid: 501,
        thread_count: 4,
        size_kind: "allocated",
        volume_total: 1 << 40,
        volume_free: 1 << 39,
        unaccounted_bytes: None,
        error_count: 2,
        vanished_count: 0,
        excluded_count: 0,
        clones_unavailable: false,
        incremental: false,
        parent_scan_id: None,
        source: "scan".to_owned(),
        tool_version: "0.0.0".to_owned(),
    }
}

fn written(dir: &Path) -> (Arena, RollUp, PathBuf) {
    let (arena, rollup) = scanned();
    let meta = meta("a");
    let id = IndexWriter::write(&arena, &rollup, &meta, dir).unwrap();
    assert_eq!(id, meta.scan_id);
    let path = index_path(dir, &id);
    (arena, rollup, path)
}

fn count(db: &Connection, sql: &str) -> i64 {
    db.query_row(sql, [], |r| r.get(0)).unwrap()
}

#[test]
fn p2_idx_02_the_index_holds_every_entry_in_preorder() {
    let env = duscope_testkit::TestEnv::new();
    let (arena, rollup, path) = written(&env.index_dir());
    let db = Connection::open(&path).unwrap();
    let view = ArenaView::new(&arena, &rollup, PathBuf::from("/t"));
    let n = i64::from(view.len());
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM dirs") + count(&db, "SELECT COUNT(*) FROM files"),
        n
    );
    // One id space, 0 .. n - 1, split between the two tables.
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM (SELECT id FROM dirs UNION SELECT id FROM files)"
        ),
        n
    );
    assert_eq!(
        count(
            &db,
            "SELECT MAX(id) FROM (SELECT id FROM dirs UNION SELECT id FROM files)"
        ),
        n - 1
    );
    // The root: id 0, no parent, the root's path as its name, the scan's totals.
    let (parent, name, cum, last): (Option<i64>, Vec<u8>, i64, i64) = db
        .query_row(
            "SELECT parent_id, name, cum_allocated, last_id FROM dirs WHERE id = 0",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    let root = view.node(0);
    assert_eq!(parent, None);
    assert_eq!(name, b"/t");
    assert_eq!(cum, i64::try_from(root.allocated).unwrap());
    assert_eq!(last, n - 1);
    // Every row against the view: parents, names, sizes, ranges.
    for id in 0..view.len() {
        let node = view.node(id);
        let table = if node.kind == duscope_walk::Kind::Dir {
            "dirs"
        } else {
            "files"
        };
        let (row_parent, row_name): (Option<i64>, Vec<u8>) = db
            .query_row(
                &format!("SELECT parent_id, name FROM {table} WHERE id = ?1"),
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        if id != 0 {
            assert_eq!(row_parent, Some(i64::from(node.parent)), "{id}");
            assert_eq!(row_name, &*view.name(id), "{id}");
        }
        if table == "dirs" {
            let (files_end, last_id, cum): (u32, u32, i64) = db
                .query_row(
                    "SELECT files_end, last_id, cum_apparent FROM dirs WHERE id = ?1",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .unwrap();
            assert_eq!((files_end, last_id), (node.files_end, node.last_id), "{id}");
            assert_eq!(cum, i64::try_from(node.apparent).unwrap(), "{id}");
        }
    }
    // The unreadable directory and the entry that could not be read.
    assert_eq!(
        count(
            &db,
            "SELECT errors FROM dirs WHERE name = CAST('closed' AS BLOB)"
        ),
        13
    );
    assert_eq!(
        count(
            &db,
            "SELECT errno FROM scan_errors WHERE name = CAST('gone' AS BLOB)"
        ),
        13
    );
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM files WHERE kind = 'symlink'"),
        1
    );
    // Extensions as by-type reads them.
    assert_eq!(
        count(&db, "SELECT n FROM type_stats WHERE ext = 'tar.gz'"),
        1
    );
    assert_eq!(count(&db, "SELECT n FROM type_stats WHERE ext = 'rs'"), 2);
    assert_eq!(
        count(&db, "SELECT SUM(n) FROM type_stats"),
        count(&db, "SELECT COUNT(*) FROM files")
    );
}

#[test]
fn p2_idx_02_scan_meta_has_every_key() {
    let env = duscope_testkit::TestEnv::new();
    let (_, rollup, path) = written(&env.index_dir());
    let db = Connection::open(&path).unwrap();
    let mut keys: Vec<String> = db
        .prepare("SELECT key FROM scan_meta")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    keys.sort();
    let mut expected: Vec<String> = SCAN_META_KEYS.iter().map(|k| (*k).to_owned()).collect();
    expected.sort();
    assert_eq!(keys, expected);
    let value = |key: &str| -> Value {
        db.query_row("SELECT value FROM scan_meta WHERE key = ?1", [key], |r| {
            r.get(0)
        })
        .unwrap()
    };
    assert_eq!(value("index_version"), Value::Text("2.0".to_owned()));
    assert_eq!(value("root"), Value::Blob(b"/t".to_vec()));
    assert_eq!(
        value("n_entries"),
        Value::Integer(i64::try_from(rollup.len()).unwrap())
    );
    assert_eq!(value("parent_scan_id"), Value::Null);
    assert_eq!(value("attrs"), Value::Text("basic".to_owned()));
    assert_eq!(value("sibling_order"), Value::Text("bytewise".to_owned()));
}

#[test]
fn p2_idx_02_the_file_is_0600_in_a_0700_directory_and_nothing_else_is_left() {
    let env = duscope_testkit::TestEnv::new();
    let dir = env.index_dir().join("fresh");
    let (_, _, path) = written(&dir);
    let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&path), 0o600);
    assert_eq!(mode(&dir.join("scans")), 0o700);
    let names: Vec<String> = fs::read_dir(dir.join("scans"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(names, [path.file_name().unwrap().to_str().unwrap()]);
}

#[test]
fn p2_idx_02_a_failed_write_leaves_no_file() {
    let env = duscope_testkit::TestEnv::new();
    let dir = env.index_dir();
    let scans = dir.join("scans");
    fs::create_dir_all(&scans).unwrap();
    let (arena, rollup) = scanned();
    let meta = meta("b");
    // The final name is taken by a directory: the rename fails.
    fs::create_dir(index_path(&dir, &meta.scan_id)).unwrap();
    let error = IndexWriter::write(&arena, &rollup, &meta, &dir).unwrap_err();
    assert!(
        error.to_string().starts_with("cannot write the index "),
        "{error}"
    );
    let left: Vec<String> = fs::read_dir(&scans)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(
        left,
        [format!("{}.db", meta.scan_id)],
        "the temporary file is gone"
    );
}

/// What a build fails with when `SQLite` cannot write its temporary file:
/// `SQLITE_FULL`.
fn disk_full() -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_FULL),
        Some("database or disk is full".to_owned()),
    )
}

/// `temp_store` as it reads inside a build: 1 is `FILE`, 2 is `MEMORY`.
fn temp_store(db: &Connection) -> i64 {
    db.pragma_query_value(None, "temp_store", |row| row.get(0))
        .unwrap()
}

#[test]
fn p2_idx_07_the_sort_spills_to_a_file_under_sqlites_default_cache() {
    let env = duscope_testkit::TestEnv::new();
    let tmp = env.tmp_dir().join("pragmas.db");
    let (arena, rollup) = scanned();
    let seen = RefCell::new(None);
    let create = |db: &Connection, ddl: &str| {
        let cache: i64 = db
            .pragma_query_value(None, "cache_size", |row| row.get(0))
            .unwrap();
        *seen.borrow_mut() = Some((temp_store(db), cache));
        db.execute_batch(ddl)
    };
    write_file_with(&arena, &rollup, &meta("pragmas"), &tmp, &create).unwrap();
    assert_eq!(
        seen.into_inner(),
        Some((1, -2000)),
        "temp_store = FILE and SQLite's own default cache of about 2 MB"
    );
}

#[test]
fn p2_idx_07_a_sort_file_that_fails_builds_the_file_again_in_memory() {
    let env = duscope_testkit::TestEnv::new();
    let tmp = env.tmp_dir().join("retry.db");
    let (arena, rollup) = scanned();
    let tries = RefCell::new(Vec::new());
    let create = |db: &Connection, ddl: &str| {
        tries.borrow_mut().push(temp_store(db));
        if tries.borrow().len() == 1 {
            return Err(disk_full());
        }
        db.execute_batch(ddl)
    };
    write_file_with(&arena, &rollup, &meta("retry"), &tmp, &create).unwrap();
    assert_eq!(*tries.borrow(), [1, 2], "once on a file, once in memory");
    // The second build started over: every row is there once, and the
    // indexes, whatever the first one left behind.
    let db = Connection::open(&tmp).unwrap();
    let dirs = count(&db, "SELECT count(*) FROM dirs");
    let files = count(&db, "SELECT count(*) FROM files");
    assert_eq!(usize::try_from(dirs + files).unwrap(), rollup.len());
    let indexes = "SELECT count(*) FROM sqlite_schema WHERE type = 'index' AND sql IS NOT NULL";
    assert_eq!(count(&db, indexes), 5);
    let ok: String = db
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(ok, "ok");
}

#[test]
fn p2_idx_07_a_second_failure_is_reported_after_one_retry() {
    let env = duscope_testkit::TestEnv::new();
    let tmp = env.tmp_dir().join("twice.db");
    let (arena, rollup) = scanned();
    let tries = Cell::new(0);
    let create = |_: &Connection, _: &str| {
        tries.set(tries.get() + 1);
        Err(disk_full())
    };
    let error = write_file_with(&arena, &rollup, &meta("twice"), &tmp, &create).unwrap_err();
    assert_eq!(tries.get(), 2, "one retry");
    assert!(error.contains("full"), "{error}");
}

#[test]
fn p2_idx_07_a_failure_the_sort_file_cannot_cause_is_not_retried() {
    let env = duscope_testkit::TestEnv::new();
    let tmp = env.tmp_dir().join("other.db");
    let (arena, rollup) = scanned();
    let tries = Cell::new(0);
    let create = |_: &Connection, _: &str| {
        tries.set(tries.get() + 1);
        Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_NOMEM),
            None,
        ))
    };
    write_file_with(&arena, &rollup, &meta("other"), &tmp, &create).unwrap_err();
    assert_eq!(tries.get(), 1, "only the temporary file's failures retry");
}

#[test]
fn p2_idx_02_scan_ids_are_twelve_hex_digits() {
    let a = ScanId::new(b"/home/me", 1_700_000_000_000_000_000, [1; 8]);
    let b = ScanId::new(b"/home/me", 1_700_000_000_000_000_000, [2; 8]);
    let c = ScanId::new(b"/home/me", 1_700_000_000_000_000_001, [1; 8]);
    for id in [&a, &b, &c] {
        assert_eq!(id.as_str().len(), 12);
        assert!(
            id.as_str()
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        );
    }
    assert_ne!(a, b, "the random bytes salt the id");
    assert_ne!(a, c);
    assert_eq!(
        a,
        ScanId::new(b"/home/me", 1_700_000_000_000_000_000, [1; 8])
    );
    assert_ne!(
        crate::index::random_bytes().unwrap(),
        crate::index::random_bytes().unwrap()
    );
}

#[test]
fn p5_walk_01_scan_volumes_keep_each_volumes_fsevents_mark() {
    let env = duscope_testkit::TestEnv::new();
    let mut tree = MemProvider::new("/t");
    tree.file("a", 1);
    let mark = duscope_walk::FsEventsMark {
        uuid: "8332A5A0-3024-4BB2-AB99-D8AB8FC0AECC".to_owned(),
        event_id: 2_694_539_488,
        os_build: "25G332".to_owned(),
    };
    let marked = (
        Some(mark.uuid.clone()),
        Some(2_694_539_488),
        Some(mark.os_build.clone()),
    );
    for (name, fsevents, columns) in [
        ("marked", Some(mark), marked),
        ("unmarked", None, (None, None, None)),
    ] {
        let mut arena = Arena::new(Attrs::Basic);
        walk_with(&tree, Path::new("/t"), &WalkOptions::default(), |event| {
            let event = match event {
                duscope_walk::ScanEvent::Volume(mut volume) => {
                    volume.fsevents = fsevents.clone();
                    duscope_walk::ScanEvent::Volume(volume)
                }
                event => event,
            };
            arena.push(event).unwrap();
        })
        .unwrap();
        let rollup = roll_up(&mut arena);
        let id = IndexWriter::write(&arena, &rollup, &meta(name), &env.index_dir()).unwrap();
        let db = Connection::open(index_path(&env.index_dir(), &id)).unwrap();
        let row: (Option<String>, Option<i64>, Option<String>) = db
            .query_row(
                "SELECT fsevents_uuid, fsevents_event_id, os_build FROM scan_volumes",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(row, columns, "{name}");
    }
}

/// A tree of `files` files in one directory: rows enough that each of
/// `files`' statements, the inserts and the index over it, runs past
/// [`super::PROGRESS_OPS`] steps.
fn wide(files: usize) -> (Arena, RollUp) {
    let mut tree = MemProvider::new("/t");
    tree.dir("d");
    for n in 0..files {
        tree.file(format!("d/f{n}"), 4096);
    }
    let mut arena = Arena::new(Attrs::Basic);
    walk_with(&tree, Path::new("/t"), &WalkOptions::default(), |event| {
        arena.push(event).unwrap();
    })
    .unwrap();
    let rollup = roll_up(&mut arena);
    (arena, rollup)
}

/// The names under `dir/scans`, none when it does not exist.
fn left(dir: &Path) -> Vec<String> {
    fs::read_dir(dir.join("scans"))
        .map(|entries| {
            entries
                .map(|e| e.unwrap().file_name().into_string().unwrap())
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn p5_core_07_a_write_stopped_before_it_starts_leaves_nothing() {
    let env = duscope_testkit::TestEnv::new();
    let dir = env.index_dir();
    let (arena, rollup) = scanned();
    let stop = Arc::new(AtomicBool::new(true));
    let error =
        IndexWriter::write_until(&arena, &rollup, &meta("before"), &dir, &stop).unwrap_err();
    assert!(matches!(error, CoreError::IndexWriteStopped), "{error:?}");
    assert_eq!(left(&dir), Vec::<String>::new());
}

/// Stopped while the indexes are built, after the rows: the next statement
/// past [`super::PROGRESS_OPS`] steps is interrupted by the progress
/// handler, not run to its end, and the temporary file goes.
#[test]
fn p5_core_07_a_write_stopped_under_way_interrupts_its_statement_and_leaves_nothing() {
    let env = duscope_testkit::TestEnv::new();
    let dir = env.index_dir();
    let (arena, rollup) = wide(2_000);
    let stop = Arc::new(AtomicBool::new(false));
    let seen = RefCell::new(None);
    let create = |db: &Connection, ddl: &str| {
        // The rows are in, under the name the write will rename.
        *seen.borrow_mut() = Some(left(&dir));
        stop.store(true, Ordering::Relaxed);
        let built = db.execute_batch(ddl);
        let code = built
            .as_ref()
            .err()
            .and_then(rusqlite::Error::sqlite_error_code);
        assert_eq!(code, Some(ErrorCode::OperationInterrupted), "{built:?}");
        built
    };
    let meta = meta("under-way");
    let error = write_in(&arena, &rollup, &meta, &dir, &stop, &create).unwrap_err();
    assert!(matches!(error, CoreError::IndexWriteStopped), "{error:?}");
    let pid = std::process::id();
    assert_eq!(
        seen.into_inner(),
        Some(vec![format!("{}.db.tmp-{pid}", meta.scan_id)])
    );
    assert_eq!(left(&dir), Vec::<String>::new(), "no .db, no .db.tmp-*");
}

/// Stopped once the last statement has run: the look before the rename
/// finds it, and the finished file is not given its name.
#[test]
fn p5_core_07_a_write_stopped_after_its_last_statement_is_not_renamed() {
    let env = duscope_testkit::TestEnv::new();
    let dir = env.index_dir();
    let (arena, rollup) = scanned();
    let stop = Arc::new(AtomicBool::new(false));
    let create = |db: &Connection, ddl: &str| {
        db.execute_batch(ddl)?;
        stop.store(true, Ordering::Relaxed);
        Ok(())
    };
    let error = write_in(&arena, &rollup, &meta("last"), &dir, &stop, &create).unwrap_err();
    assert!(matches!(error, CoreError::IndexWriteStopped), "{error:?}");
    assert_eq!(left(&dir), Vec::<String>::new());
}

/// A sort file that fails while the write is stopped does not start the
/// build again in memory.
#[test]
fn p5_core_07_a_stopped_write_is_not_built_again() {
    let env = duscope_testkit::TestEnv::new();
    let dir = env.index_dir();
    let (arena, rollup) = scanned();
    let stop = Arc::new(AtomicBool::new(false));
    let tries = Cell::new(0);
    let create = |_: &Connection, _: &str| {
        tries.set(tries.get() + 1);
        stop.store(true, Ordering::Relaxed);
        Err(disk_full())
    };
    let error = write_in(&arena, &rollup, &meta("again"), &dir, &stop, &create).unwrap_err();
    assert!(matches!(error, CoreError::IndexWriteStopped), "{error:?}");
    assert_eq!(tries.get(), 1, "no second build");
    assert_eq!(left(&dir), Vec::<String>::new());
}

/// A flag never set changes nothing: the file is written whole, under its
/// name, as `write` writes it.
#[test]
fn p5_core_07_a_write_never_stopped_is_whole() {
    let env = duscope_testkit::TestEnv::new();
    let dir = env.index_dir();
    let (arena, rollup) = wide(2_000);
    let stop = Arc::new(AtomicBool::new(false));
    let meta = meta("whole");
    let id = IndexWriter::write_until(&arena, &rollup, &meta, &dir, &stop).unwrap();
    assert_eq!(left(&dir), [format!("{id}.db")]);
    let db = Connection::open(index_path(&dir, &id)).unwrap();
    assert_eq!(count(&db, "SELECT COUNT(*) FROM files"), 2_000);
    let ok: String = db
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .unwrap();
    assert_eq!(ok, "ok");
}
