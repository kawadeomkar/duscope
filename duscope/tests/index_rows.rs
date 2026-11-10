//! P5-SEC-04: a saved scan whose rows break the index's id layout (a parent
//! cycle, a directory its own parent, a subtree that ends before its
//! directory or past the scan) fails `top --files`, `tree` and `rescan
//! --incremental` with `index_corrupt`, exit 5, where `top --files` read for
//! ever before. Each index is a copy of a scan the test saved in its own
//! index directory, changed in a second index directory of the test's: an
//! index is never edited in place.
//!
//! `rescan --incremental` reads the saved rows only where the platform keeps
//! a change history (macOS's `FSEvents`); elsewhere it is `not_available`
//! before it reads them, and the incremental rescan's own read of the rows
//! is tested in duscope-core on every platform with a scripted history.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fs::{self, DirBuilder, File};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use duscope_testkit::TestEnv;
use rusqlite::Connection;
use serde_json::Value;

/// How long a verb may take on a six-entry scan before the test calls it a
/// hang.
const DEADLINE: Duration = Duration::from_secs(60);

/// `t` holding `root.txt` (id 1), `a` (2) holding `b` (3) holding `f` (4),
/// and `c` (5), saved in the test's index directory: its scan id. The root
/// holds a file beside its subdirectories, as most do: `tree` reads it
/// beside the root, and so the block of rows that holds `a`, which a cycle
/// takes out of the root's subdirectories (its `parent_id` points down).
fn saved(env: &TestEnv) -> String {
    let root = fs::canonicalize(env.tmp_dir()).unwrap().join("t");
    fs::create_dir_all(root.join("a/b")).unwrap();
    fs::create_dir_all(root.join("c")).unwrap();
    fs::write(root.join("root.txt"), [7; 100]).unwrap();
    fs::write(root.join("a/b/f"), [7; 8192]).unwrap();
    let out = env
        .cmd()
        .args(["scan", root.to_str().unwrap(), "--save", "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let scan: Value = serde_json::from_slice(&out.stdout).unwrap();
    scan["scan"]["id"].as_str().unwrap().to_owned()
}

/// A copy of saved scan `id` in an index directory of its own, `case`,
/// changed by `sql`: the directory to pass as `--index-dir`.
fn broken(env: &TestEnv, id: &str, case: &str, sql: &str) -> PathBuf {
    let dir = env.tmp_dir().join(case);
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir.join("scans"))
        .unwrap();
    let name = format!("{id}.db");
    let file = dir.join("scans").join(&name);
    fs::copy(env.index_dir().join("scans").join(&name), &file).unwrap();
    let db = Connection::open(&file).unwrap();
    // The ids the cases name.
    let names: Vec<(u32, Vec<u8>)> = db
        .prepare("SELECT id, name FROM dirs WHERE id > 0 ORDER BY id")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(
        names,
        [(2, b"a".to_vec()), (3, b"b".to_vec()), (5, b"c".to_vec())]
    );
    db.execute_batch(sql).unwrap();
    drop(db);
    dir
}

/// duscope's exit code and envelope for `args`, failing the test if it has
/// not answered within [`DEADLINE`].
fn answer(env: &TestEnv, index_dir: &Path, args: &[&str]) -> (Option<i32>, Value) {
    let stdout = index_dir.with_extension(format!("{}.json", args[0]));
    let mut child = env
        .cmd()
        .args(args)
        .arg("--index-dir")
        .arg(index_dir)
        .arg("--json")
        .stdout(File::create(&stdout).unwrap())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let began = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if began.elapsed() > DEADLINE {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("{args:?} did not answer within {DEADLINE:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let text = fs::read(&stdout).unwrap();
    let envelope = serde_json::from_slice(&text)
        .unwrap_or_else(|e| panic!("{args:?}: {e}: {}", String::from_utf8_lossy(&text)));
    (status.code(), envelope)
}

/// Saves the scan, breaks a copy of it with `sql`, and checks that each
/// verb that reads the broken row answers `index_corrupt` naming `reported`.
fn each_verb_is_index_corrupt(case: &str, sql: &str, reported: &str) {
    let env = TestEnv::new();
    let id = saved(&env);
    let dir = broken(&env, &id, case, sql);
    let corrupt = |args: &[&str]| {
        let (code, envelope) = answer(&env, &dir, args);
        assert_eq!(code, Some(5), "{case} {args:?}: {envelope}");
        assert_eq!(
            envelope["error"]["code"], "index_corrupt",
            "{case} {args:?}"
        );
        let message = envelope["error"]["message"].as_str().unwrap();
        assert!(message.ends_with(reported), "{case} {args:?}: {message}");
    };
    corrupt(&["top", "--files", "--scan", &id]);
    corrupt(&["tree", "--scan", &id]);
    corrupt(&["tree", "--scan", &id, "--depth", "5"]);
    if duscope_walk::fsevents::current_event_id().is_some() {
        corrupt(&["rescan", "--incremental", "--scan", &id]);
    } else {
        // No change history (Linux): refused before the rows are read.
        let (code, envelope) = answer(&env, &dir, &["rescan", "--incremental", "--scan", &id]);
        assert_eq!(code, Some(2), "{case}: {envelope}");
        assert_eq!(envelope["error"]["code"], "not_available", "{case}");
    }
}

#[test]
fn p5_sec_04_a_two_directory_cycle_is_index_corrupt() {
    // `a` and `b` each name the other their parent.
    each_verb_is_index_corrupt(
        "cycle",
        "UPDATE dirs SET parent_id = 3 WHERE id = 2",
        "dirs row 2: its parent_id 3 is not below its id",
    );
}

#[test]
fn p5_sec_04_a_directory_its_own_parent_is_index_corrupt() {
    each_verb_is_index_corrupt(
        "self-parent",
        "UPDATE dirs SET parent_id = 2 WHERE id = 2",
        "dirs row 2: its parent_id 2 is not below its id",
    );
}

#[test]
fn p5_sec_04_a_subtree_that_ends_before_its_directory_is_index_corrupt() {
    each_verb_is_index_corrupt(
        "last-before-id",
        "UPDATE dirs SET last_id = 1 WHERE id = 2",
        "dirs row 2: id <= files_end <= last_id < n_entries fails (files_end 2, last_id 1, \
         n_entries 6)",
    );
}

#[test]
fn p5_sec_04_a_subtree_that_ends_past_the_scan_is_index_corrupt() {
    each_verb_is_index_corrupt(
        "last-past-the-scan",
        "UPDATE dirs SET last_id = 6 WHERE id = 2",
        "dirs row 2: id <= files_end <= last_id < n_entries fails (files_end 2, last_id 6, \
         n_entries 6)",
    );
}
