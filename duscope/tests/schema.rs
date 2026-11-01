//! P2-QRY-08: `schema` through the binary: a golden of the layout with a
//! saved scan's rows, the DDL for people, null rows without a saved scan,
//! and `--envelope`.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

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

/// A tree of four files under two directories, canonical.
fn tree(env: &TestEnv) -> PathBuf {
    let root = env.tmp_dir().join("tree");
    for (path, size) in [
        ("a.bin", 3000),
        ("b.txt", 100),
        ("sub/c.log", 2000),
        ("sub/d", 0),
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
        &[
            "scan",
            root.to_str().unwrap(),
            "--save",
            "--json",
            "--threads",
            "4",
        ],
    ));
}

/// The rows `schema` reports for `table`.
fn rows(answer: &Value, table: &str) -> Value {
    answer["result"]["tables"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == table)
        .unwrap()["rows"]
        .clone()
}

#[test]
fn p2_qry_08_golden_of_the_layout_with_a_saved_scans_rows() {
    let env = TestEnv::new();
    let root = tree(&env);
    save(&env, &root);
    let out = run(&env, &["schema", root.to_str().unwrap(), "--json"]);
    let answer = ok(&out);
    let os = std::env::consts::OS;
    golden(
        &format!("schema_json_tree@{os}.snap"),
        &duscope_testkit::normalize(&String::from_utf8(out.stdout).unwrap(), &root),
    );
    assert_eq!(answer["result"]["index_version"], "2.0");
    // The root, sub/ and four files.
    assert_eq!(rows(&answer, "dirs"), 2);
    assert_eq!(rows(&answer, "files"), 4);
    assert_eq!(rows(&answer, "scan_meta"), 25);
    let tables: Vec<&str> = answer["result"]["tables"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        tables,
        [
            "scan_meta",
            "scan_volumes",
            "dirs",
            "files",
            "hardlinks",
            "clones",
            "shared_members",
            "scan_errors",
            "type_stats"
        ]
    );
    assert!(answer["scan"]["id"].is_string());
}

#[test]
fn p2_qry_08_people_read_the_ddl_with_each_tables_rows() {
    let env = TestEnv::new();
    let root = tree(&env);
    save(&env, &root);
    let out = run(
        &env,
        &["schema", root.to_str().unwrap(), "--format", "table"],
    );
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    // After the files table, before the next: its rows.
    let files = text.find("CREATE TABLE files (").unwrap() + "CREATE TABLE".len();
    let rows = text[files..].find("-- 4 rows in scan ").unwrap();
    assert!(
        !text[files..files + rows].contains("CREATE TABLE"),
        "{text}"
    );
    assert!(text.contains("CREATE INDEX files_allocated"));
    // --plain: a record per column.
    let out = run(&env, &["schema", root.to_str().unwrap(), "--plain"]);
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(
        text.lines()
            .any(|l| l == "files\tid\tINTEGER\tfalse\tmacos,linux\t4"),
        "{text}"
    );
    assert!(
        text.lines()
            .any(|l| l.starts_with("scan_volumes\tfsevents_uuid\tTEXT\ttrue\tmacos\t")),
        "{text}"
    );
}

#[test]
fn p2_qry_08_without_a_saved_scan_the_rows_are_null() {
    let env = TestEnv::new();
    let root = tree(&env);
    let answer = ok(&run(&env, &["schema", root.to_str().unwrap(), "--json"]));
    assert!(answer["scan"].is_null());
    assert!(rows(&answer, "files").is_null());
    // schema never scans.
    let scans = ok(&run(&env, &["scans", "--json"]));
    assert_eq!(scans["entries"], serde_json::json!([]));
}

#[test]
fn p2_qry_08_envelope_prints_a_verbs_schema() {
    let env = TestEnv::new();
    let file = Path::new(env!("CARGO_MANIFEST_DIR")).join("../schemas/envelope/top.schema.json");
    let expected: Value = serde_json::from_str(&fs::read_to_string(file).unwrap()).unwrap();
    let out = run(&env, &["schema", "--envelope", "top", "--json"]);
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    // Compact, on one line, for machines.
    assert_eq!(text.lines().count(), 1);
    assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), expected);
    // With no verb, the list.
    let out = run(&env, &["schema", "--envelope", "--plain"]);
    let names = String::from_utf8(out.stdout).unwrap();
    assert!(names.lines().any(|l| l == "query"), "{names}");
    assert!(names.lines().any(|l| l == "error"), "{names}");
    let answer = ok(&run(&env, &["schema", "--envelope", "--json"]));
    assert_eq!(answer["query"]["command"], "schema --envelope");
    assert!(
        answer["result"]["envelopes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "diff")
    );
    // A name with no envelope.
    let out = run(&env, &["schema", "--envelope", "nope", "--json"]);
    assert_eq!(out.status.code(), Some(2));
    let error: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(error["error"]["code"], "usage");
}
