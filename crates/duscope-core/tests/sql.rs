//! P2-QRY-07: the `query --sql` sandbox on an index written from a tree in
//! memory. Every test here runs it, so the process's `SQLite` heap limit
//! (256 MiB, set by the sandbox) binds this test binary alone.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use duscope_core::CoreError;
use duscope_core::arena::Arena;
use duscope_core::envelope::SqlValue;
use duscope_core::index::{Index, IndexWriter, ScanId, ScanMeta, index_path};
use duscope_core::query::sql::{DEFAULT_TIMEOUT, MAX_ROWS};
use duscope_core::query::{SqlQuery, TreeView, sql};
use duscope_core::rollup::roll_up;
use duscope_testkit::TestEnv;
use duscope_walk::{Attrs, MemProvider, WalkOptions, walk_with};

fn meta(root: &Path) -> ScanMeta {
    ScanMeta {
        scan_id: ScanId::new(b"sql", 1, [0; 8]),
        root: root.as_os_str().as_encoded_bytes().to_vec(),
        root_as_given: root.as_os_str().as_encoded_bytes().to_vec(),
        started_at: 1,
        finished_at: 2,
        options: "{}".to_owned(),
        platform: "test".to_owned(),
        scanning_uid: 501,
        thread_count: 4,
        size_kind: "allocated",
        volume_total: 1 << 40,
        volume_free: 1 << 39,
        unaccounted_bytes: None,
        error_count: 0,
        vanished_count: 0,
        excluded_count: 0,
        clones_unavailable: false,
        incremental: false,
        parent_scan_id: None,
        source: "scan".to_owned(),
        tool_version: "0.0.0".to_owned(),
    }
}

/// An index of /r: a/ with 40 files of 1..=40 KiB, b/c/w.txt, and a file
/// whose name is not UTF-8.
fn index(env: &TestEnv) -> PathBuf {
    let mut tree = MemProvider::new("/r");
    for n in 1..=40u64 {
        tree.file(format!("a/f{n:02}.bin"), n << 10);
    }
    tree.file("b/c/w.txt", 10);
    tree.file(Path::new(OsStr::from_bytes(b"b/\xff.bin")), 7);
    let mut arena = Arena::new(Attrs::Basic);
    walk_with(&tree, tree.root_path(), &WalkOptions::default(), |e| {
        arena.push(e).unwrap();
    })
    .unwrap();
    let rollup = roll_up(&mut arena);
    let meta = meta(tree.root_path());
    let dir = env.tmp_dir();
    IndexWriter::write(&arena, &rollup, &meta, &dir).unwrap();
    index_path(&dir, &meta.scan_id)
}

fn query(sql: &str) -> SqlQuery<'_> {
    SqlQuery {
        sql,
        max_rows: MAX_ROWS,
        timeout: DEFAULT_TIMEOUT,
        root_id: 0,
    }
}

fn ints(rows: &[Vec<SqlValue>]) -> Vec<i64> {
    rows.iter()
        .map(|row| match row[0] {
            SqlValue::Integer(n) => n,
            ref other => panic!("{other:?}"),
        })
        .collect()
}

#[test]
fn p2_qry_07_a_statement_without_a_limit_returns_25_rows() {
    let env = TestEnv::new();
    let index = Index::open(&index(&env)).unwrap();
    let all = "SELECT apparent FROM files WHERE apparent >= 1024 ORDER BY apparent";
    let got = sql(&index, &query(all)).unwrap();
    assert_eq!(got.columns, ["apparent"]);
    assert_eq!(got.row_count, 25);
    assert!(got.truncated);
    assert_eq!(
        ints(&got.rows),
        (1..=25).map(|n| n << 10).collect::<Vec<_>>()
    );
    // A LIMIT of its own stands, up to --max-rows.
    let limited = format!("{all} LIMIT 30");
    let got = sql(&index, &query(&limited)).unwrap();
    assert_eq!((got.row_count, got.truncated), (30, false));
    let q = SqlQuery {
        max_rows: 10,
        ..query(&limited)
    };
    let got = sql(&index, &q).unwrap();
    assert_eq!((got.row_count, got.truncated), (10, true));
    // Below 25, --max-rows bounds a statement without a LIMIT too.
    let q = SqlQuery {
        max_rows: 10,
        ..query(all)
    };
    assert_eq!(sql(&index, &q).unwrap().row_count, 10);
    // Every row there is, and no more: not truncated.
    let got = sql(&index, &query(&format!("{all} LIMIT 40"))).unwrap();
    assert_eq!((got.row_count, got.truncated), (40, false));
    // A LIMIT inside parentheses is the subquery's, not the statement's.
    let inner = "SELECT apparent FROM (SELECT apparent FROM files LIMIT 100)";
    assert_eq!(sql(&index, &query(inner)).unwrap().row_count, 25);
    // A trailing semicolon and comment.
    let tail = format!("{all} LIMIT 3; -- three");
    assert_eq!(sql(&index, &query(&tail)).unwrap().row_count, 3);
}

#[test]
fn p2_qry_07_values_keep_their_types_and_blobs_are_bytes() {
    let env = TestEnv::new();
    let index = Index::open(&index(&env)).unwrap();
    let got = sql(
        &index,
        &query(
            "SELECT name, CAST(name AS TEXT), 1.5, NULL FROM files \
             WHERE CAST(name AS TEXT) = 'w.txt'",
        ),
    )
    .unwrap();
    assert_eq!(
        got.rows,
        [vec![
            SqlValue::Bytes(b"w.txt".to_vec()),
            SqlValue::Text("w.txt".to_owned()),
            SqlValue::Real(1.5),
            SqlValue::Null,
        ]]
    );
    let json = serde_json::to_value(&got).unwrap();
    assert_eq!(
        json["rows"][0][0],
        serde_json::json!({"$bytes": "dy50eHQ="})
    );
    assert_eq!(json["rows"][0][1], "w.txt");
    assert_eq!(json["row_count"], 1);
    // Text that is not UTF-8 is bytes too.
    let got = sql(
        &index,
        &query("SELECT CAST(name AS TEXT) FROM files WHERE apparent = 7"),
    )
    .unwrap();
    assert_eq!(got.rows, [vec![SqlValue::Bytes(b"\xff.bin".to_vec())]]);
}

#[test]
fn p2_qry_07_path_under_and_human_size() {
    let env = TestEnv::new();
    let index = Index::open(&index(&env)).unwrap();
    let w = index.lookup([&b"b"[..], b"c", b"w.txt"]).unwrap();
    let c = index.lookup([&b"b"[..], b"c"]).unwrap();
    let a = index.lookup([&b"a"[..]]).unwrap();
    let got = sql(
        &index,
        &query(&format!(
            "SELECT path({w}), path(0), under({w}, {c}), under({w}, {a}), \
             human_size(1536), human_size(-1536), human_size(999), path(99999), under({w}, 99999)"
        )),
    )
    .unwrap();
    assert_eq!(
        got.rows,
        [vec![
            SqlValue::Text("/r/b/c/w.txt".to_owned()),
            SqlValue::Text("/r".to_owned()),
            SqlValue::Integer(1),
            SqlValue::Integer(0),
            SqlValue::Text("1.5 KiB".to_owned()),
            SqlValue::Text("-1.5 KiB".to_owned()),
            SqlValue::Text("999 B".to_owned()),
            SqlValue::Null,
            SqlValue::Null,
        ]]
    );
    // A path that is not UTF-8 is bytes.
    let got = sql(
        &index,
        &query("SELECT path(id) FROM files WHERE apparent = 7"),
    )
    .unwrap();
    assert_eq!(got.rows, [vec![SqlValue::Bytes(b"/r/b/\xff.bin".to_vec())]]);
    // :root_id and :root_last name the entry asked about.
    let q = SqlQuery {
        root_id: c,
        ..query(
            "SELECT :root_id, :root_last, count(*) FROM files WHERE id BETWEEN :root_id AND :root_last",
        )
    };
    let got = sql(&index, &q).unwrap();
    let last = i64::from(index.node(c).last_id);
    assert_eq!(
        got.rows,
        [vec![
            SqlValue::Integer(i64::from(c)),
            SqlValue::Integer(last),
            SqlValue::Integer(1),
        ]]
    );
}

/// Which error a statement gets.
#[derive(Debug, PartialEq)]
enum Refusal {
    Denied,
    Timeout,
    Invalid,
}

fn refusal(index: &Index, statement: &str, timeout: Duration) -> Refusal {
    let q = SqlQuery {
        timeout,
        ..query(statement)
    };
    match sql(index, &q) {
        Err(CoreError::SqlDenied(_)) => Refusal::Denied,
        Err(CoreError::SqlTimeout(_)) => Refusal::Timeout,
        Err(CoreError::SqlInvalid(_)) => Refusal::Invalid,
        other => panic!("{statement}: {other:?}"),
    }
}

#[test]
fn p2_qry_07_adversarial_statements_are_refused_with_their_codes() {
    let env = TestEnv::new();
    let file = index(&env);
    let before = fs::read(&file).unwrap();
    let index = Index::open(&file).unwrap();
    let deep = format!("SELECT {}1{}", "abs(".repeat(150), ")".repeat(150));
    let wide = format!("SELECT {}", vec!["1"; 150].join(", "));
    let compound = vec!["SELECT 1"; 30].join(" UNION ALL ");
    let nested = format!(
        "SELECT * FROM {}files{}",
        "(SELECT * FROM ".repeat(500),
        ")".repeat(500)
    );
    let cases: &[(&str, Refusal)] = &[
        ("PRAGMA table_info(files)", Refusal::Denied),
        ("PRAGMA writable_schema = 1", Refusal::Denied),
        ("PRAGMA query_only = 0", Refusal::Denied),
        ("SELECT * FROM pragma_table_info('files')", Refusal::Denied),
        ("ATTACH '/tmp/duscope-attach.db' AS other", Refusal::Denied),
        ("DETACH main", Refusal::Denied),
        ("SELECT load_extension('/tmp/x')", Refusal::Denied),
        ("SELECT readfile('/etc/passwd')", Refusal::Denied),
        ("SELECT writefile('/tmp/x', 'y')", Refusal::Denied),
        ("SELECT random()", Refusal::Denied),
        ("SELECT randomblob(8)", Refusal::Denied),
        ("SELECT zeroblob(8)", Refusal::Denied),
        ("SELECT row_number() OVER () FROM files", Refusal::Denied),
        ("INSERT INTO files (id) VALUES (999999)", Refusal::Denied),
        ("UPDATE dirs SET name = x'00'", Refusal::Denied),
        ("DELETE FROM files", Refusal::Denied),
        ("DROP TABLE files", Refusal::Denied),
        ("CREATE TABLE t (x)", Refusal::Denied),
        ("CREATE TEMP TABLE t (x)", Refusal::Denied),
        ("CREATE TEMP VIEW v AS SELECT 1", Refusal::Denied),
        ("ALTER TABLE files RENAME TO f", Refusal::Denied),
        ("VACUUM", Refusal::Denied),
        ("VACUUM INTO '/tmp/duscope-vacuum.db'", Refusal::Denied),
        ("ANALYZE", Refusal::Denied),
        ("REINDEX", Refusal::Denied),
        ("BEGIN", Refusal::Denied),
        ("SAVEPOINT s", Refusal::Denied),
        ("SELECT * FROM sqlite_master", Refusal::Denied),
        ("SELECT count(*) FROM sqlite_schema", Refusal::Denied),
        ("SELECT name FROM temp.sqlite_master", Refusal::Denied),
        (
            "WITH t AS (SELECT sql FROM sqlite_master) SELECT * FROM t",
            Refusal::Denied,
        ),
        ("SELECT 1; SELECT 2", Refusal::Denied),
        ("SELECT 1; DROP TABLE files", Refusal::Denied),
        ("SELECT printf('%.*c', 2000000, 'x')", Refusal::Denied),
        (&deep, Refusal::Denied),
        (&wide, Refusal::Denied),
        (&compound, Refusal::Denied),
        (&nested, Refusal::Denied),
        ("SELEC 1", Refusal::Invalid),
        ("SELECT * FROM nowhere", Refusal::Invalid),
        ("SELECT nothing FROM files", Refusal::Invalid),
        ("", Refusal::Invalid),
        ("-- a comment alone", Refusal::Invalid),
    ];
    for (statement, expected) in cases {
        assert_eq!(
            &refusal(&index, statement, DEFAULT_TIMEOUT),
            expected,
            "{statement}"
        );
    }
    // A CTE bomb and a cross join run to the deadline.
    for statement in [
        "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c) SELECT count(*) FROM c",
        "SELECT count(*) FROM files a, files b, files c, files d, files e, files f",
    ] {
        let started = Instant::now();
        assert_eq!(
            refusal(&index, statement, Duration::from_secs(1)),
            Refusal::Timeout,
            "{statement}"
        );
        assert!(started.elapsed() < Duration::from_secs(3), "{statement}");
    }
    // One that outgrows the heap limit before the deadline.
    let distinct = "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c) \
                    SELECT DISTINCT printf('%.400c%d', 'x', x) FROM c LIMIT 1 OFFSET 100000000";
    assert_eq!(
        refusal(&index, distinct, Duration::from_secs(60)),
        Refusal::Denied
    );
    // Nothing was written, and no file appeared beside the index.
    assert_eq!(fs::read(&file).unwrap(), before);
    let beside: Vec<_> = fs::read_dir(file.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(beside.len(), 1, "{beside:?}");
}

#[test]
fn p2_qry_07_common_table_expressions_joins_and_counts_read_the_tables() {
    let env = TestEnv::new();
    let index = Index::open(&index(&env)).unwrap();
    for (statement, expected) in [
        ("SELECT count(*) FROM files", 42),
        ("SELECT count(*) FROM FILES", 42),
        (
            "WITH t AS (SELECT id FROM files) SELECT count(*) FROM t",
            42,
        ),
        // A common table expression may take a table's name.
        (
            "WITH dirs AS (SELECT id FROM files) SELECT count(*) FROM dirs",
            42,
        ),
        (
            "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM n WHERE x < 5) \
             SELECT count(*) FROM n",
            5,
        ),
        (
            "SELECT count(*) FROM files f JOIN dirs d ON f.parent_id = d.id \
             WHERE CAST(d.name AS TEXT) = 'a'",
            40,
        ),
        (
            "SELECT count(*) FROM files a, files b WHERE a.id = b.id",
            42,
        ),
    ] {
        let got = sql(&index, &query(statement)).unwrap();
        assert_eq!(ints(&got.rows), [expected], "{statement}");
    }
}

#[test]
fn p2_qry_07_a_hostile_index_cannot_reach_past_its_tables() {
    let env = TestEnv::new();
    let file = index(&env);
    let hostile = file.with_file_name("hostile.db");
    fs::copy(&file, &hostile).unwrap();
    let db = rusqlite::Connection::open(&hostile).unwrap();
    db.execute_batch(
        "CREATE TABLE secrets (x); INSERT INTO secrets VALUES ('s');
         CREATE VIEW peek AS SELECT sql FROM sqlite_master;
         CREATE VIEW files_too AS SELECT * FROM files;",
    )
    .unwrap();
    drop(db);
    let index = Index::open(&hostile).unwrap();
    for statement in [
        "SELECT * FROM secrets",
        "SELECT count(*) FROM secrets",
        "SELECT * FROM peek",
        "SELECT count(*) FROM files_too",
    ] {
        assert!(sql(&index, &query(statement)).is_err(), "{statement}");
    }
}
