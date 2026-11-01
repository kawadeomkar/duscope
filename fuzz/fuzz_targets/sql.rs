//! P2-QRY-07: arbitrary statements through the `query --sql` sandbox, on
//! the index of a small tree written once per process. Whatever the input,
//! the sandbox answers within its row limit or refuses it (`sql_denied`,
//! `sql_timeout`, or a statement SQLite cannot prepare), never panics, and
//! never changes the file.

#![no_main]

use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

use duscope_core::CoreError;
use duscope_core::arena::Arena;
use duscope_core::index::{Index, IndexWriter, ScanId, ScanMeta, index_path};
use duscope_core::query::{SqlQuery, sql};
use duscope_core::rollup::roll_up;
use duscope_walk::{Attrs, MemProvider, WalkOptions, walk_with};
use libfuzzer_sys::fuzz_target;

/// The rows an answer may hold.
const MAX_ROWS: usize = 100;

/// The index file, with its length and modification time as written.
struct Fixture {
    file: PathBuf,
    len: u64,
    modified: SystemTime,
}

fn fixture() -> &'static Fixture {
    static FIXTURE: OnceLock<Fixture> = OnceLock::new();
    FIXTURE.get_or_init(|| {
        let mut tree = MemProvider::new("/r");
        tree.file("a/x.txt", 10)
            .file("a/y.bin", 4096)
            .file("b/c/w.txt", 1 << 20)
            .file(Path::new(OsStr::from_bytes(b"b/\xff.bin")), 7)
            .link("a/x-link.txt", "a/x.txt")
            .symlink("b/s", 3)
            .dir("empty");
        let mut arena = Arena::new(Attrs::Basic);
        walk_with(&tree, tree.root_path(), &WalkOptions::default(), |e| {
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
            tool_version: "0.0.0".to_owned(),
        };
        let dir = std::env::temp_dir().join(format!("duscope-fuzz-sql-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("the temporary directory is writable");
        IndexWriter::write(&arena, &rollup, &meta, &dir).expect("the index writes");
        let file = index_path(&dir, &meta.scan_id);
        let written = fs::metadata(&file).expect("the index is there");
        Fixture {
            len: written.len(),
            modified: written.modified().expect("the platform has mtimes"),
            file,
        }
    })
}

fuzz_target!(|data: &[u8]| {
    let fixture = fixture();
    let statement = String::from_utf8_lossy(data);
    let index = Index::open(&fixture.file).expect("the index opens");
    let q = SqlQuery {
        sql: &statement,
        max_rows: MAX_ROWS,
        timeout: Duration::from_millis(500),
        root_id: 0,
    };
    match sql(&index, &q) {
        Ok(answer) => {
            assert!(answer.rows.len() <= MAX_ROWS);
            assert_eq!(answer.row_count, answer.rows.len() as u64);
            assert!(
                answer
                    .rows
                    .iter()
                    .all(|row| row.len() == answer.columns.len())
            );
        }
        Err(CoreError::SqlDenied(_) | CoreError::SqlTimeout(_) | CoreError::SqlInvalid(_)) => {}
        Err(other) => panic!("{statement:?}: {other}"),
    }
    let after = fs::metadata(&fixture.file).expect("the index is still there");
    assert_eq!(after.len(), fixture.len);
    assert_eq!(after.modified().ok(), Some(fixture.modified));
});
