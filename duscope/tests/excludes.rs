//! P1-WALK-09: excludes at scan time, end to end: a golden of the scan with
//! cache, glob and ignore-file excludes, and negations in an ignore file.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use duscope_testkit::TestEnv;

/// The golden file `name` under `duscope/tests/golden/`, compared with
/// `actual`; with `DUSCOPE_TEST_UPDATE_GOLDEN=1` it is rewritten instead.
fn golden(name: &str, actual: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name);
    if std::env::var_os("DUSCOPE_TEST_UPDATE_GOLDEN").is_some_and(|v| v == "1") {
        fs::write(&path, actual).unwrap();
        return;
    }
    let expected = fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert_eq!(
        actual, expected,
        "golden {name} differs; rerun with DUSCOPE_TEST_UPDATE_GOLDEN=1 to regenerate"
    );
}

/// `text` without what differs between runs, machines and releases: the
/// root's path, the scan's times, the volumes and the version.
fn normalize(text: &str, root: &Path) -> String {
    let mut text = text.replace(root.to_str().unwrap(), "<root>");
    for (key, placeholder) in [
        ("\"started_at\":\"", "<time>"),
        ("\"finished_at\":\"", "<time>"),
        ("\"name\":\"duscope\",\"version\":\"", "<version>"),
    ] {
        let mut from = 0;
        while let Some(at) = text[from..].find(key) {
            let start = from + at + key.len();
            let end = start + text[start..].find('"').unwrap();
            text.replace_range(start..end, placeholder);
            from = start;
        }
    }
    if let Some(at) = text.find("\"volumes\":[") {
        let start = at + "\"volumes\":".len();
        let end = start + text[start..].find("}]").unwrap() + 2;
        text.replace_range(start..end, "[\"<volumes>\"]");
    }
    text
}

fn duscope(env: &TestEnv) -> Command {
    Command::from_std(env.cmd())
}

#[test]
fn p1_walk_09_excluded_subtrees_leave_the_totals_and_are_recorded() {
    let env = TestEnv::new();
    let root = fs::canonicalize(env.fixtures().path("home/Projects")).unwrap();
    let ignore = env.tmp_dir().join("ignore");
    fs::write(&ignore, "*.json\n!package.json\n").unwrap();
    let out = duscope(&env)
        .args([
            "scan",
            "--json",
            "--threads",
            "4",
            "--exclude-caches",
            "--exclude",
            "*.lock",
        ])
        .arg("--ignore-from")
        .arg(&ignore)
        .arg(&root)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    let os = std::env::consts::OS;
    golden(
        &format!("scan_json_excludes@{os}.snap"),
        &normalize(&stdout, &root),
    );
    // The excluded subtrees are gone from the totals.
    let excluded: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let full: serde_json::Value = serde_json::from_slice(
        &duscope(&env)
            .args(["scan", "--json", "--threads", "4"])
            .arg(&root)
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let total = |e: &serde_json::Value| e["scan"]["total_allocated_bytes"].as_u64().unwrap();
    assert!(total(&excluded) * 10 < total(&full));
    let records = excluded["scan"]["excluded"].as_array().unwrap();
    assert!(records.iter().any(|r| r["path"]
        == format!("{}/npm-app/node_modules", root.display())
        && r["reason"] == "excluded_cache"));
}

#[test]
fn p1_walk_09_ignore_from_honours_negations() {
    let env = TestEnv::new();
    let tree = env.tmp_dir().join("tree");
    for file in [
        "a.log",
        "keep.log",
        "sub/b.log",
        "sub/keep.log",
        "sub/notes.txt",
    ] {
        let path = tree.join(file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "x").unwrap();
    }
    let ignore = env.tmp_dir().join("ignore");
    fs::write(&ignore, "*.log\n!keep.log\n").unwrap();
    let out = duscope(&env)
        .args(["scan", "--json", "--ignore-from"])
        .arg(&ignore)
        .arg(&tree)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let envelope: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    // keep.log twice and notes.txt are counted; a.log and b.log are not.
    assert_eq!(envelope["scan"]["file_count"], 3);
    assert_eq!(envelope["scan"]["excluded_count"], 2);
    // Files left out leave no directory record.
    assert_eq!(envelope["scan"]["excluded"], serde_json::json!([]));
    // A missing ignore file is a usage error.
    duscope(&env)
        .args(["scan", "--ignore-from", "/nonexistent/ignore"])
        .arg(&tree)
        .assert()
        .code(2);
}

#[test]
fn p1_walk_09_a_pattern_naming_nothing_is_refused_and_dot_slash_anchors() {
    let env = TestEnv::new();
    let tree = env.tmp_dir().join("tree");
    for file in ["build/a.o", "src/build/b.o", "src/main.rs"] {
        let path = tree.join(file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "x").unwrap();
    }
    for pattern in ["", "/", "//", ".", "./"] {
        duscope(&env)
            .args(["scan", "--json", "--exclude", pattern])
            .arg(&tree)
            .assert()
            .code(2);
    }
    // Anchored at the root: build/ is left out, src/build/ is not.
    for pattern in ["/build", "./build", "//build", "./build/"] {
        let out = duscope(&env)
            .args(["scan", "--json", "--exclude", pattern])
            .arg(&tree)
            .output()
            .unwrap();
        assert!(out.status.success(), "{pattern}");
        let envelope: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(envelope["scan"]["file_count"], 2, "{pattern}");
    }
}
