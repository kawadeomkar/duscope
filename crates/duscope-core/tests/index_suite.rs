//! P2-TEST-01: the index as a whole. A reader never sees half a write;
//! reading an index changes nothing in it or beside it, and needs no write
//! access; a newer minor opens and another major is refused; and the
//! `query --sql` sandbox answers or refuses whatever it is given and
//! leaves the file as it was.
//!
//! The sandbox sets the process's `SQLite` heap limit (256 MiB), which
//! binds this test binary alone.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fs::{self, Permissions};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, SystemTime};

use duscope_core::CoreError;
use duscope_core::arena::Arena;
use duscope_core::index::{Index, IndexWriter, ScanId, ScanMeta, index_path};
use duscope_core::query::{
    self, ByTypeQuery, Filters, SizeKind, SortKey, SqlQuery, StaleQuery, TopQuery, TreeQuery,
    TreeView, index_files, sql,
};
use duscope_core::rollup::roll_up;
use duscope_core::rules::Classes;
use duscope_testkit::TestEnv;
use duscope_walk::{Attrs, MemProvider, WalkOptions, walk_with};
use proptest::prelude::*;
use rusqlite::Connection;

fn meta(root: &Path, started_at: i64) -> ScanMeta {
    ScanMeta {
        scan_id: ScanId::new(root.as_os_str().as_encoded_bytes(), started_at, [3; 8]),
        root: root.as_os_str().as_encoded_bytes().to_vec(),
        root_as_given: root.as_os_str().as_encoded_bytes().to_vec(),
        started_at,
        finished_at: started_at + 1_000,
        options: "{}".to_owned(),
        platform: "test".to_owned(),
        scanning_uid: 501,
        thread_count: 1,
        size_kind: "allocated",
        volume_total: 1 << 40,
        volume_free: 1 << 39,
        unaccounted_bytes: None,
        error_count: 1,
        vanished_count: 0,
        excluded_count: 0,
        clones_unavailable: false,
        incremental: false,
        parent_scan_id: None,
        source: "scan".to_owned(),
        tool_version: "0.0.0".to_owned(),
    }
}

fn tree() -> MemProvider {
    let mut tree = MemProvider::new("/r");
    tree.file("a/x.txt", 10)
        .file("a/y.bin", 4096)
        .file("b/c/w.log", 1 << 20)
        .file("b/node_modules/m.js", 300)
        .symlink("b/s", 3)
        .dir("locked")
        .unreadable("locked", 13)
        .dir("empty");
    tree
}

/// The tree's scan, started at `started_at`, written under `dir`.
fn write(dir: &Path, started_at: i64) -> PathBuf {
    let tree = tree();
    let mut arena = Arena::new(Attrs::Basic);
    walk_with(&tree, tree.root_path(), &WalkOptions::default(), |e| {
        arena.push(e).unwrap();
    })
    .unwrap();
    let rollup = roll_up(&mut arena);
    let meta = meta(tree.root_path(), started_at);
    IndexWriter::write(&arena, &rollup, &meta, dir).unwrap();
    index_path(dir, &meta.scan_id)
}

/// The names in `dir`, sorted.
fn listing(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

#[test]
fn p2_test_01_a_reader_never_sees_half_a_write() {
    let env = TestEnv::new();
    let dir = env.tmp_dir();
    let done = AtomicBool::new(false);
    let opened = thread::scope(|scope| {
        scope.spawn(|| {
            for n in 0..40 {
                write(&dir, 1_700_000_000_000_000_000 + n);
            }
            done.store(true, Ordering::Release);
        });
        // Every file the listing shows opens whole, however far the writer
        // is through the next one.
        let mut opened = 0;
        loop {
            let finished = done.load(Ordering::Acquire);
            for (id, file) in index_files(&dir) {
                let index =
                    Index::open(&file).unwrap_or_else(|e| panic!("{id} opened half written: {e}"));
                assert_eq!(index.meta().scan_id, id);
                assert!(index.len() > 0);
                opened += 1;
            }
            if finished {
                break opened;
            }
        }
    });
    assert!(opened >= 40);
    assert_eq!(index_files(&dir).len(), 40);
    // Nothing but the 40 is left: no temporary file, no journal.
    assert_eq!(listing(&dir.join("scans")).len(), 40);
}

/// Every read a verb makes of a saved scan.
fn read_everything(index: &Index) {
    let classes = Classes::vendored();
    let _ = query::top(index, &TopQuery::default());
    let _ = query::top(
        index,
        &TopQuery {
            files: true,
            ..TopQuery::default()
        },
    );
    let _ = query::tree(index, &TreeQuery::default());
    let _ = query::by_type(
        index,
        &ByTypeQuery {
            classes: &classes,
            examples: 3,
            filters: Filters::default(),
            limit: None,
            size: SizeKind::Allocated,
            sort: SortKey::Size,
            reverse: false,
        },
    );
    let _ = query::stale(
        index,
        &StaleQuery {
            now: 4_102_444_800_000_000_000,
            older_than: 1,
            ..StaleQuery::default()
        },
    );
    let _ = index.volumes();
    let _ = index.excluded();
    let _ = index.unreadable_dirs(0, index.node(0).last_id);
    for table in ["dirs", "files", "scan_meta", "no_such_table"] {
        let _ = index.rows(table);
    }
    let answer = sql(
        index,
        &SqlQuery {
            sql: "SELECT path(id), human_size(cum_allocated) FROM dirs",
            max_rows: 100,
            timeout: Duration::from_secs(5),
            root_id: 0,
        },
    )
    .unwrap();
    assert!(answer.row_count > 0);
    assert!(index.failure().is_none(), "{:?}", index.failure());
}

#[test]
fn p2_test_01_reading_an_index_changes_nothing_and_needs_no_write_access() {
    let env = TestEnv::new();
    let dir = env.tmp_dir();
    let file = write(&dir, 1_700_000_000_000_000_000);
    let scans = dir.join("scans");
    let bytes = fs::read(&file).unwrap();
    let modified: SystemTime = fs::metadata(&file).unwrap().modified().unwrap();
    let before = listing(&scans);
    {
        let index = Index::open(&file).unwrap();
        read_everything(&index);
        // Opened twice at once, as a verb and the sandbox do.
        let again = Index::open(&file).unwrap();
        read_everything(&again);
    }
    assert_eq!(fs::read(&file).unwrap(), bytes, "the file changed");
    assert_eq!(fs::metadata(&file).unwrap().modified().unwrap(), modified);
    assert_eq!(listing(&scans), before, "a file appeared beside the index");
    // Read-only, in a directory that is read-only too.
    fs::set_permissions(&file, Permissions::from_mode(0o400)).unwrap();
    fs::set_permissions(&scans, Permissions::from_mode(0o500)).unwrap();
    let read = std::panic::catch_unwind(|| read_everything(&Index::open(&file).unwrap()));
    fs::set_permissions(&scans, Permissions::from_mode(0o700)).unwrap();
    fs::set_permissions(&file, Permissions::from_mode(0o600)).unwrap();
    assert!(read.is_ok(), "a read-only index could not be read");
    assert_eq!(listing(&scans), before);
}

/// A copy of the index at `file`, named `name`, changed by `sql`.
fn changed(file: &Path, name: &str, sql: &str) -> PathBuf {
    let copy = file.with_file_name(name);
    fs::copy(file, &copy).unwrap();
    let db = Connection::open(&copy).unwrap();
    db.execute_batch(sql).unwrap();
    copy
}

#[test]
fn p2_test_01_a_newer_minor_opens_and_another_major_is_refused() {
    let env = TestEnv::new();
    let file = write(&env.tmp_dir(), 1_700_000_000_000_000_000);
    let top = |index: &Index| query::top(index, &TopQuery::default()).entries.len();
    let current = top(&Index::open(&file).unwrap());
    // A later minor adds a table, a column and a key, which this reader
    // passes over.
    let newer = changed(
        &file,
        "newer.db",
        "UPDATE scan_meta SET value = '2.9' WHERE key = 'index_version'; \
         INSERT INTO scan_meta VALUES ('later_key', 'x'); \
         ALTER TABLE dirs ADD COLUMN later_column INTEGER; \
         CREATE TABLE later_table (x);",
    );
    let index = Index::open(&newer).unwrap();
    assert_eq!(index.version(), "2.9");
    assert_eq!(top(&index), current);
    assert_eq!(index.rows("later_table").unwrap(), Some(0));
    assert!(index.failure().is_none());
    // Another major, or no major at all.
    for version in ["1.0", "3.0", "10.2", "2", "two"] {
        let other = changed(
            &file,
            &format!("v{version}.db"),
            &format!("UPDATE scan_meta SET value = '{version}' WHERE key = 'index_version';"),
        );
        match Index::open(&other) {
            Err(CoreError::IndexVersion { found, .. }) => assert_eq!(found, version),
            other => panic!("{version}: {other:?}"),
        }
    }
    // A file that does not say.
    let unsaid = changed(
        &file,
        "unsaid.db",
        "DELETE FROM scan_meta WHERE key = 'index_version';",
    );
    assert!(matches!(
        Index::open(&unsaid),
        Err(CoreError::IndexCorrupt { .. })
    ));
}

/// A statement built from parts the sandbox allows, weighted so that
/// about half the statements are answered, and parts it must not allow.
fn statement() -> impl Strategy<Value = String> {
    let verb = prop_oneof![
        12 => Just("SELECT"),
        2 => Just("SELECT DISTINCT"),
        1 => Just("PRAGMA"),
        1 => Just("INSERT INTO dirs SELECT"),
        1 => Just("UPDATE dirs SET name = x'00' WHERE 1 OR"),
        1 => Just("DELETE FROM dirs WHERE"),
        1 => Just("ATTACH DATABASE ':memory:' AS"),
        1 => Just("CREATE TABLE t AS SELECT"),
        1 => Just("CREATE TEMP VIEW v AS SELECT"),
        1 => Just("DROP TABLE"),
        1 => Just("VACUUM INTO '/tmp/x.db' --"),
        1 => Just("BEGIN; SELECT"),
        2 => Just("WITH RECURSIVE r(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM r) SELECT"),
    ];
    let expr = prop_oneof![
        4 => Just("*"),
        4 => Just("count(*)"),
        4 => Just("id, path(id)"),
        3 => Just("human_size(cum_allocated)"),
        3 => Just("under(id, 0)"),
        1 => Just("load_extension('x')"),
        1 => Just("readfile('/etc/passwd')"),
        1 => Just("randomblob(100000000)"),
        1 => Just("zeroblob(2000000000)"),
        1 => Just("printf('%.*c', 2000000000, 'x')"),
        1 => Just("sqlite_version()"),
        1 => Just("(SELECT group_concat(sql) FROM sqlite_schema)"),
        1 => Just("n"),
    ];
    let object = prop_oneof![
        8 => Just("dirs"),
        3 => Just("files"),
        1 => Just("scan_meta"),
        1 => Just("sqlite_schema"),
        1 => Just("sqlite_master"),
        2 => Just("main.dirs"),
        1 => Just("temp.sqlite_temp_schema"),
        1 => Just("pragma_table_info('dirs')"),
        1 => Just("json_each('[1,2]')"),
        1 => Just("r"),
        1 => Just("dirs a, dirs b, dirs c, dirs d"),
    ];
    let tail = prop_oneof![
        4 => Just(""),
        3 => Just("LIMIT 5"),
        2 => Just("WHERE id = :root_id"),
        1 => Just("; DROP TABLE dirs"),
        1 => Just("UNION ALL SELECT sql FROM sqlite_schema"),
        1 => Just("-- a comment"),
        1 => Just("/* unclosed"),
        2 => Just("ORDER BY 1 DESC LIMIT 100000"),
    ];
    (verb, expr, object, tail).prop_map(|(v, e, o, t)| format!("{v} {e} FROM {o} {t}"))
}

#[test]
fn p2_test_01_the_sandbox_answers_or_refuses_and_changes_nothing() {
    let env = TestEnv::new();
    let file = write(&env.tmp_dir(), 1_700_000_000_000_000_000);
    let bytes = fs::read(&file).unwrap();
    let before = listing(file.parent().unwrap());
    let index = Index::open(&file).unwrap();
    let (answered, refused) = (AtomicUsize::new(0), AtomicUsize::new(0));
    let mut runner = proptest::test_runner::TestRunner::new(ProptestConfig::with_cases(400));
    runner
        .run(&prop_oneof![4 => statement(), 1 => ".{0,80}"], |text| {
            let q = SqlQuery {
                sql: &text,
                max_rows: 50,
                timeout: Duration::from_secs(1),
                root_id: 0,
            };
            match sql(&index, &q) {
                Ok(answer) => {
                    prop_assert!(answer.rows.len() <= 50, "{text}");
                    prop_assert_eq!(answer.row_count, answer.rows.len() as u64);
                    answered.fetch_add(1, Ordering::Relaxed);
                }
                Err(
                    CoreError::SqlDenied(_) | CoreError::SqlTimeout(_) | CoreError::SqlInvalid(_),
                ) => {
                    refused.fetch_add(1, Ordering::Relaxed);
                }
                Err(other) => prop_assert!(false, "{text}: {other:?}"),
            }
            Ok(())
        })
        .unwrap();
    // The parts make both: statements it answers, and ones it refuses.
    let (answered, refused) = (answered.into_inner(), refused.into_inner());
    assert!(
        answered >= 20 && refused >= 20,
        "{answered} answered, {refused} refused"
    );
    drop(index);
    assert_eq!(
        fs::read(&file).unwrap(),
        bytes,
        "the sandbox changed the file"
    );
    assert_eq!(listing(file.parent().unwrap()), before);
}
