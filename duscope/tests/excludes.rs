//! P1-WALK-09: excludes at scan time, end to end: a golden of the scan with
//! cache, glob and ignore-file excludes, and negations in an ignore file.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use duscope_testkit::{TestEnv, golden, normalize};

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

#[test]
fn p1_walk_09_exclude_from_skips_comments_blank_lines_and_crlf() {
    let env = TestEnv::new();
    let root = fs::canonicalize(env.fixtures().path("home/Projects")).unwrap();
    let full = scan_json(&env, &root, &[]);
    // A comment, a blank line, a CRLF line, a pattern with blanks around
    // it and an anchored one: the same as the two patterns on the command
    // line.
    let list = env.tmp_dir().join("excludes");
    fs::write(
        &list,
        "# build output\r\n\r\n*.lock\r\n   /npm-app/node_modules   \n#/rust-app\n",
    )
    .unwrap();
    let from_file = scan_json(&env, &root, &["--exclude-from".as_ref(), list.as_os_str()]);
    let on_line = scan_json(
        &env,
        &root,
        &[
            "--exclude".as_ref(),
            "*.lock".as_ref(),
            "--exclude".as_ref(),
            "/npm-app/node_modules".as_ref(),
        ],
    );
    for key in ["total_allocated_bytes", "file_count", "excluded_count"] {
        assert_eq!(from_file["scan"][key], on_line["scan"][key], "{key}");
    }
    assert!(from_file["scan"]["excluded_count"].as_u64().unwrap() >= 2);
    assert!(
        from_file["scan"]["total_allocated_bytes"].as_u64().unwrap()
            < full["scan"]["total_allocated_bytes"].as_u64().unwrap()
    );
    // The commented-out pattern excluded nothing.
    let records = from_file["scan"]["excluded"].as_array().unwrap();
    assert!(
        records
            .iter()
            .all(|r| !r["path"].as_str().unwrap().ends_with("rust-app"))
    );
}

#[test]
fn p1_walk_09_a_trailing_slash_leaves_out_directories_only() {
    let env = TestEnv::new();
    let tree = env.tmp_dir().join("tree");
    // A directory and a file of the same name, at two depths.
    for file in ["build/a.o", "src/build", "src/main.rs"] {
        let path = tree.join(file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "x").unwrap();
    }
    let with = |pattern: &str| scan_json(&env, &tree, &["--exclude".as_ref(), pattern.as_ref()]);
    // build/: the directory is left out, the file named build is not.
    let dirs = with("build/");
    assert_eq!(dirs["scan"]["file_count"], 2);
    assert_eq!(dirs["scan"]["excluded_count"], 1);
    // build: both are.
    let both = with("build");
    assert_eq!(both["scan"]["file_count"], 1);
    assert_eq!(both["scan"]["excluded_count"], 2);
}

/// The JSON envelope of `scan` over `root` with `args`.
fn scan_json(env: &TestEnv, root: &Path, args: &[&std::ffi::OsStr]) -> serde_json::Value {
    let out = duscope(env)
        .args(["scan", "--json"])
        .args(args)
        .arg(root)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}
