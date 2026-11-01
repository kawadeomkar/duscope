//! P2-QRY-07: `query --sql` through the binary: a golden of `--plain`, the
//! envelope, the refusals' codes and exit codes, `--timeout 1` stopping a
//! cross join, a path binding `:root_id`, and a scan run for the verb.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::{Duration, Instant};

use duscope_testkit::{TestEnv, golden};
use serde_json::Value;

fn run(env: &TestEnv, args: &[&str]) -> Output {
    env.cmd().args(args).output().unwrap()
}

fn ok(out: &Output) -> Value {
    assert!(
        out.status.success(),
        "exit {:?}: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

/// The error code and exit code of a failed run.
fn refused(out: &Output) -> (String, i32) {
    let error: Value = serde_json::from_slice(&out.stdout).unwrap();
    (
        error["error"]["code"].as_str().unwrap().to_owned(),
        out.status.code().unwrap(),
    )
}

/// A tree of files of known lengths, canonical.
fn tree(env: &TestEnv, name: &str) -> PathBuf {
    let root = env.tmp_dir().join(name);
    for (path, size) in [
        ("a.bin", 3000),
        ("b.txt", 100),
        ("sub/c.log", 2000),
        ("sub/deeper/d", 0),
    ] {
        let file = root.join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, vec![b'x'; size]).unwrap();
    }
    fs::canonicalize(root).unwrap()
}

fn save(env: &TestEnv, root: &Path) {
    ok(&run(
        env,
        &["scan", root.to_str().unwrap(), "--save", "--json"],
    ));
}

const LARGEST: &str =
    "SELECT CAST(name AS TEXT) AS name, apparent, kind FROM files ORDER BY apparent DESC, name";

#[test]
fn p2_qry_07_golden_of_plain_on_a_tree() {
    let env = TestEnv::new();
    let root = tree(&env, "tree");
    save(&env, &root);
    let out = run(
        &env,
        &["query", root.to_str().unwrap(), "--sql", LARGEST, "--plain"],
    );
    assert!(out.status.success());
    // A heading line, then a record per row.
    golden(
        "query_plain_largest.snap",
        &String::from_utf8(out.stdout).unwrap(),
    );
}

#[test]
fn p2_qry_07_the_envelope_carries_the_query_and_the_answer() {
    let env = TestEnv::new();
    let root = tree(&env, "tree");
    save(&env, &root);
    let path = root.to_str().unwrap();
    let sql = "SELECT name, apparent FROM files ORDER BY apparent DESC LIMIT 2";
    let answer = ok(&run(&env, &["query", path, "--sql", sql, "--json"]));
    assert_eq!(answer["ok"], true);
    let query = &answer["query"];
    assert_eq!(query["command"], "query");
    assert_eq!(query["path"], path);
    assert_eq!(query["sql"], sql);
    assert_eq!(query["limit"], 10_000);
    assert_eq!(query["max_rows"], 10_000);
    assert_eq!(query["timeout_ms"], 5_000);
    let result = &answer["result"];
    assert_eq!(result["columns"], serde_json::json!(["name", "apparent"]));
    // Names are BLOBs: base64 of "a.bin".
    assert_eq!(
        result["rows"],
        serde_json::json!([[{"$bytes": "YS5iaW4="}, 3000], [{"$bytes": "Yy5sb2c="}, 2000]])
    );
    assert_eq!(result["row_count"], 2);
    assert_eq!(result["truncated"], false);
    assert!(answer["scan"]["id"].is_string());
    // Without a LIMIT, 25 at most; --max-rows lowers it.
    let answer = ok(&run(
        &env,
        &[
            "query",
            path,
            "--sql",
            "SELECT id FROM files",
            "--max-rows",
            "2",
            "--json",
        ],
    ));
    assert_eq!(answer["query"]["limit"], 2);
    assert_eq!(answer["result"]["row_count"], 2);
    assert_eq!(answer["result"]["truncated"], true);
}

#[test]
fn p2_qry_07_refusals_have_their_codes_and_exit_codes() {
    let env = TestEnv::new();
    let root = tree(&env, "tree");
    save(&env, &root);
    let path = root.to_str().unwrap();
    for (sql, code, exit) in [
        ("PRAGMA table_info(files)", "sql_denied", 1),
        ("ATTACH 'other.db' AS other", "sql_denied", 1),
        ("SELECT load_extension('x')", "sql_denied", 1),
        ("DELETE FROM files", "sql_denied", 1),
        ("SELECT * FROM sqlite_master", "sql_denied", 1),
        ("SELECT 1; SELECT 2", "sql_denied", 1),
        ("SELEC 1", "usage", 2),
        ("SELECT * FROM nowhere", "usage", 2),
    ] {
        let out = run(&env, &["query", path, "--sql", sql, "--json"]);
        assert_eq!(refused(&out), (code.to_owned(), exit), "{sql}");
    }
    for flags in [
        ["--max-rows", "10001"],
        ["--max-rows", "0"],
        ["--timeout", "61"],
        ["--timeout", "0"],
    ] {
        let mut args = vec!["query", path, "--sql", "SELECT 1", "--json"];
        args.extend(flags);
        assert_eq!(
            refused(&run(&env, &args)),
            ("usage".to_owned(), 2),
            "{flags:?}"
        );
    }
}

#[test]
fn p2_qry_07_timeout_1_interrupts_a_cross_join() {
    let env = TestEnv::new();
    let home = fs::canonicalize(env.fixtures().path("home")).unwrap();
    save(&env, &home);
    let started = Instant::now();
    let out = run(
        &env,
        &[
            "query",
            home.to_str().unwrap(),
            "--sql",
            "SELECT count(*) FROM files a, files b, files c, files d, files e",
            "--timeout",
            "1",
            "--json",
        ],
    );
    let elapsed = started.elapsed();
    assert_eq!(refused(&out), ("sql_timeout".to_owned(), 1));
    let error: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(error["error"]["retryable"], true);
    assert!(elapsed < Duration::from_secs(4), "{elapsed:?}");
}

#[test]
fn p2_qry_07_a_path_inside_a_saved_scan_binds_root_id() {
    let env = TestEnv::new();
    let root = tree(&env, "tree");
    save(&env, &root);
    let sub = root.join("sub");
    let answer = ok(&run(
        &env,
        &[
            "query",
            sub.to_str().unwrap(),
            "--sql",
            "SELECT path(:root_id), count(*) FROM files WHERE under(id, :root_id)",
            "--json",
        ],
    ));
    assert_eq!(answer["query"]["path"], sub.to_str().unwrap());
    assert_eq!(
        answer["result"]["rows"],
        serde_json::json!([[sub.to_str().unwrap(), 2]])
    );
}

#[test]
fn p2_qry_07_without_a_saved_scan_it_scans_first() {
    let env = TestEnv::new();
    let kept = tree(&env, "kept");
    let count = "SELECT count(*) FROM files";
    let answer = ok(&run(
        &env,
        &["query", kept.to_str().unwrap(), "--sql", count, "--json"],
    ));
    assert_eq!(answer["result"]["rows"], serde_json::json!([[4]]));
    // Saved, and read the next time.
    let scans = ok(&run(&env, &["scans", "--json"]));
    assert_eq!(scans["entries"].as_array().unwrap().len(), 1);
    let again = ok(&run(
        &env,
        &[
            "query",
            kept.to_str().unwrap(),
            "--sql",
            count,
            "--no-scan",
            "--json",
        ],
    ));
    assert_eq!(again["scan"]["id"], answer["scan"]["id"]);
    // --no-save: the index is written aside and removed.
    let passing = tree(&env, "passing");
    let answer = ok(&run(
        &env,
        &[
            "query",
            passing.to_str().unwrap(),
            "--sql",
            count,
            "--no-save",
            "--json",
        ],
    ));
    assert_eq!(answer["result"]["rows"], serde_json::json!([[4]]));
    let scans = ok(&run(&env, &["scans", "--json"]));
    assert_eq!(scans["entries"].as_array().unwrap().len(), 1);
    let left: Vec<_> = fs::read_dir(env.tmp_dir())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("duscope-query-"))
        .collect();
    assert!(left.is_empty(), "{left:?}");
}

#[test]
fn p2_qry_07_people_get_a_table_and_a_footer() {
    let env = TestEnv::new();
    let root = tree(&env, "tree");
    save(&env, &root);
    let out = run(
        &env,
        &[
            "query",
            root.to_str().unwrap(),
            "--sql",
            LARGEST,
            "--format",
            "table",
        ],
    );
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines
            .first()
            .map(|l| l.split_whitespace().collect::<Vec<_>>()),
        Some(vec!["name", "apparent", "kind"])
    );
    assert!(
        lines[1].contains("a.bin") && lines[1].contains("3000"),
        "{text}"
    );
    assert!(
        lines.last().unwrap().starts_with("4 rows · saved scan "),
        "{text}"
    );
}
