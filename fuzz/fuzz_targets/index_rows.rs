//! P5-SEC-04: a saved index whose rows say anything about where an entry
//! sits. The index of a small tree is written once per process; each input
//! is a list of changes to a copy of it, `UPDATE dirs SET {parent_id,
//! last_id, files_end, depth} = v WHERE id = k` and `UPDATE files SET
//! parent_id = v WHERE id = k`. The copy is opened as any saved scan is,
//! and every entry's path, every directory's subdirectories, `top` (files,
//! with ties walked in path order, and directories), `tree` and the
//! incremental rescan's load of the saved directories must answer, or report
//! the damage, without panicking or reading for ever.

#![no_main]

use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use duscope_core::CoreError;
use duscope_core::arena::Arena;
use duscope_core::index::{Index, IndexWriter, ScanId, ScanMeta, index_path};
use duscope_core::query::{self, TopQuery, TreeQuery, TreeView};
use duscope_core::rescan::{self, Checks, History, MAX_AGE_NS, Stop};
use duscope_core::rollup::roll_up;
use duscope_walk::fsevents::Replay;
use duscope_walk::{
    Attrs, FsEventsError, FsEventsMark, MemProvider, ScanEvent, WalkOptions, walk_with,
};
use libfuzzer_sys::fuzz_target;
use rusqlite::Connection;

/// The columns an input changes, by table: where an entry sits.
const COLUMNS: [(&str, &str); 5] = [
    ("dirs", "parent_id"),
    ("dirs", "last_id"),
    ("dirs", "files_end"),
    ("dirs", "depth"),
    ("files", "parent_id"),
];

/// The version the fixture's `scan_meta` names, which the rescan's checks
/// compare with their own.
const TOOL_VERSION: &str = "0.0.0";

/// The fixture's index, the ids of its two tables, and the file each input
/// is copied to.
struct Fixture {
    file: PathBuf,
    copy: PathBuf,
    dirs: Vec<i64>,
    files: Vec<i64>,
    meta: ScanMeta,
}

/// The mark the fixture's volume carries, so the rescan's checks reach the
/// saved directories.
fn mark() -> FsEventsMark {
    FsEventsMark {
        uuid: "5A1E0000-0000-0000-0000-000000000000".to_owned(),
        event_id: 1,
        os_build: "25G332".to_owned(),
    }
}

fn fixture() -> &'static Fixture {
    static FIXTURE: OnceLock<Fixture> = OnceLock::new();
    FIXTURE.get_or_init(|| {
        let mut tree = MemProvider::new("/r");
        tree.file("top.txt", 5)
            .file("a/x.txt", 10)
            .file("a/y.bin", 4096)
            .file("a/d/z.txt", 10)
            .file("b/c/w.txt", 1 << 20)
            .file(Path::new(OsStr::from_bytes(b"b/\xff.bin")), 7)
            .link("a/x-link.txt", "a/x.txt")
            .symlink("b/s", 3)
            .dir("empty");
        let mut arena = Arena::new(Attrs::Basic);
        walk_with(&tree, tree.root_path(), &WalkOptions::default(), |mut e| {
            if let ScanEvent::Volume(volume) = &mut e {
                volume.fsevents = volume.entered().then(mark);
            }
            arena.push(e).expect("the tree fits the arena");
        })
        .expect("the tree in memory reads");
        let rollup = roll_up(&mut arena);
        let root = tree.root_path().as_os_str().as_bytes().to_vec();
        let meta = ScanMeta {
            scan_id: ScanId::new(b"fuzz", 1, [0; 8]),
            root: root.clone(),
            root_as_given: root,
            started_at: 1,
            finished_at: 2,
            options: "{}".to_owned(),
            platform: "fuzz".to_owned(),
            scanning_uid: 0,
            thread_count: 1,
            size_kind: "allocated",
            volume_total: 1 << 30,
            volume_free: 1 << 29,
            unaccounted_bytes: None,
            error_count: 0,
            vanished_count: 0,
            excluded_count: 0,
            clones_unavailable: false,
            incremental: false,
            parent_scan_id: None,
            source: "scan".to_owned(),
            tool_version: TOOL_VERSION.to_owned(),
        };
        let dir =
            std::env::temp_dir().join(format!("duscope-fuzz-index-rows-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("the temporary directory is writable");
        IndexWriter::write(&arena, &rollup, &meta, &dir).expect("the index writes");
        let file = index_path(&dir, &meta.scan_id);
        let db = Connection::open(&file).expect("the index opens");
        let ids = |table: &str| -> Vec<i64> {
            db.prepare(&format!("SELECT id FROM {table} ORDER BY id"))
                .and_then(|mut stmt| stmt.query_map([], |row| row.get(0))?.collect())
                .expect("the ids read")
        };
        let (dirs, files) = (ids("dirs"), ids("files"));
        drop(db);
        Fixture {
            copy: dir.join("input.db"),
            file,
            dirs,
            files,
            meta,
        }
    })
}

/// A value for a column from an input's two bytes: near the fixture's ids
/// (a few below zero, a few past its entries) most often, near `u32::MAX`,
/// past what a `u32` holds, or NULL.
fn value(kind: u8, v: u8) -> Option<i64> {
    match kind >> 5 {
        0..=4 => Some(i64::from(v) - 4),
        5 => Some(i64::from(u32::MAX) - i64::from(v)),
        6 => Some(i64::from(u32::MAX) + 1 + i64::from(v)),
        _ => None,
    }
}

/// A history that reports one directory changed and one subtree to read
/// whole, as `FSEvents` would.
struct Script;

impl History for Script {
    fn now(&self) -> Option<u64> {
        Some(2)
    }

    fn replay(
        &self,
        _dev: u64,
        _mark: &FsEventsMark,
        under: &Path,
    ) -> Result<Replay, FsEventsError> {
        Ok(Replay {
            root: under.to_owned(),
            dirs: vec![under.join("a")],
            must_scan_subdirs: vec![under.join("b")],
            history_done: true,
            ..Replay::default()
        })
    }

    fn mark(&self, _dev: u64, now: u64) -> Option<FsEventsMark> {
        Some(FsEventsMark {
            event_id: now,
            ..mark()
        })
    }

    fn reusable(&self, _fs_type: &str) -> bool {
        true
    }
}

fuzz_target!(|data: &[u8]| {
    let fixture = fixture();
    fs::copy(&fixture.file, &fixture.copy).expect("the index copies");
    let db = Connection::open(&fixture.copy).expect("the copy opens");
    for change in data.chunks_exact(3) {
        let (table, column) = COLUMNS[usize::from(change[0]) % COLUMNS.len()];
        let ids = if table == "dirs" {
            &fixture.dirs
        } else {
            &fixture.files
        };
        let id = ids[usize::from(change[1]) % ids.len()];
        // A change a column's constraint refuses (NULL where it may not be)
        // is not made.
        let _ = db.execute(
            &format!("UPDATE {table} SET {column} = ?1 WHERE id = ?2"),
            rusqlite::params![value(change[0], change[2]), id],
        );
    }
    drop(db);
    let index = Index::open(&fixture.copy).expect("rows do not change how an index opens");
    for id in 0..index.len() {
        index.path(id);
        index.child_dirs(id);
    }
    query::top(
        &index,
        &TopQuery {
            files: true,
            limit: Some(2),
            ..TopQuery::default()
        },
    );
    query::top(&index, &TopQuery::default());
    query::tree(
        &index,
        &TreeQuery {
            depth: Some(u32::MAX),
            ..TreeQuery::default()
        },
    );
    let checks = Checks {
        now_ns: fixture.meta.started_at,
        max_age_ns: MAX_AGE_NS,
        tool_version: TOOL_VERSION,
    };
    match rescan::plan(&index, &checks, &Script) {
        Ok(_) | Err(Stop::Fallback(..) | Stop::Index(CoreError::IndexCorrupt { .. })) => {}
        Err(other) => panic!("{other:?}"),
    }
});
