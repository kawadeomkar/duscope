//! P5-SEC-04: a saved scan whose rows break the index's id layout (a parent
//! cycle, a directory its own parent, a subtree that ends before its
//! directory or past the scan) fails `top --files`, `tree` and `rescan
//! --incremental` with `index_corrupt`, exit 5, where `top --files` read for
//! ever before. Each index is a copy of a scan the test saved in its own
//! index directory, changed in a second index directory of the test's: an
//! index is never edited in place.
//!
//! A walk down the tree holds each directory's children to the layout as
//! it lists them, so `tree`, `render` and `diff` read each entry once and
//! fail where a row's ids do not nest, however its rows lie; and an
//! `n_entries` past the rows, or an id that no row holds, fails at once
//! rather than read each missing id.
//!
//! A verb checks the rows it reads, so one that never reaches a damaged row
//! may answer from the rows it did read; `scans --check` reads them all.
//!
//! `rescan --incremental` reads the saved rows only where the platform keeps
//! a change history (macOS's `FSEvents`); elsewhere it is `not_available`
//! before it reads them, and the incremental rescan's own read of the rows
//! is tested in duscope-core on every platform with a scripted history.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fs::{self, DirBuilder, File};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
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
    saved_with(env, true)
}

/// The tree [`saved`] describes, its root's `root.txt` only with `root_file`
/// (without it, `a` is 1, `b` 2, `f` 3 and `c` 4): its scan id.
fn saved_with(env: &TestEnv, root_file: bool) -> String {
    let root = tree_root(env);
    fs::create_dir_all(root.join("a/b")).unwrap();
    fs::create_dir_all(root.join("c")).unwrap();
    if root_file {
        fs::write(root.join("root.txt"), [7; 100]).unwrap();
    }
    fs::write(root.join("a/b/f"), [7; 8192]).unwrap();
    scan_saved(env, &root)
}

/// Scans `root` and saves the scan in the test's index directory: its id.
fn scan_saved(env: &TestEnv, root: &Path) -> String {
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

/// The root of the tree the tests save.
fn tree_root(env: &TestEnv) -> PathBuf {
    fs::canonicalize(env.tmp_dir()).unwrap().join("t")
}

/// A copy of saved scan `id` of [`saved`]'s tree in an index directory of
/// its own, `case`, changed by `sql`: the directory to pass as
/// `--index-dir`.
fn broken(env: &TestEnv, id: &str, case: &str, sql: &str) -> PathBuf {
    broken_ids(env, id, case, sql, &[(2, "a"), (3, "b"), (5, "c")])
}

/// [`broken`], for a tree whose directories below the root are `dirs`, the
/// ids `sql` names.
fn broken_ids(env: &TestEnv, id: &str, case: &str, sql: &str, dirs: &[(u32, &str)]) -> PathBuf {
    let dir = env.tmp_dir().join(case);
    let db = copy(env, id, &dir);
    // The ids the cases name.
    let names: Vec<(u32, Vec<u8>)> = db
        .prepare("SELECT id, name FROM dirs WHERE id > 0 ORDER BY id")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let expected: Vec<(u32, Vec<u8>)> = dirs
        .iter()
        .map(|(id, name)| (*id, name.as_bytes().to_vec()))
        .collect();
    assert_eq!(names, expected);
    db.execute_batch(sql).unwrap();
    drop(db);
    dir
}

/// A copy of saved scan `id` in index directory `dir`, made if need be,
/// open to be changed.
fn copy(env: &TestEnv, id: &str, dir: &Path) -> Connection {
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir.join("scans"))
        .unwrap();
    let name = format!("{id}.db");
    let file = dir.join("scans").join(&name);
    fs::copy(env.index_dir().join("scans").join(&name), &file).unwrap();
    Connection::open(&file).unwrap()
}

/// duscope's exit code and envelope for `args`, failing the test if it has
/// not answered within [`DEADLINE`].
fn answer(env: &TestEnv, index_dir: &Path, args: &[&str]) -> (Option<i32>, Value) {
    let mut json = args.to_vec();
    json.push("--json");
    let (code, text, _) = run(env, index_dir, &json);
    let envelope = serde_json::from_slice(&text)
        .unwrap_or_else(|e| panic!("{args:?}: {e}: {}", String::from_utf8_lossy(&text)));
    (code, envelope)
}

/// duscope's exit code, stdout and stderr for `args` with `--index-dir
/// index_dir`, failing the test if it has not answered within
/// [`DEADLINE`].
fn run(env: &TestEnv, index_dir: &Path, args: &[&str]) -> (Option<i32>, Vec<u8>, Vec<u8>) {
    let stdout = index_dir.with_extension(format!("{}.out", args[0]));
    let stderr = index_dir.with_extension(format!("{}.err", args[0]));
    let mut child = env
        .cmd()
        .args(args)
        .arg("--index-dir")
        .arg(index_dir)
        .stdout(File::create(&stdout).unwrap())
        .stderr(File::create(&stderr).unwrap())
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
    (
        status.code(),
        fs::read(&stdout).unwrap(),
        fs::read(&stderr).unwrap(),
    )
}

/// Checks that `render` of the saved scan `id` with `args` fails with
/// `index_corrupt`, exit 5, which it says on stderr with `reported`, in SVG
/// and in PNG, and writes no picture.
fn render_is_index_corrupt(env: &TestEnv, dir: &Path, case: &str, args: &[&str], reported: &str) {
    for format in ["svg", "png"] {
        let picture = dir.with_extension(format);
        let mut render = vec![
            "render",
            "--format",
            format,
            "-o",
            picture.to_str().unwrap(),
        ];
        render.extend_from_slice(args);
        let (code, _, stderr) = run(env, dir, &render);
        let stderr = String::from_utf8_lossy(&stderr);
        assert_eq!(code, Some(5), "{case} {render:?}: {stderr}");
        let said = format!("cannot read the index {}", dir.display());
        assert!(
            stderr.contains(&said) && stderr.contains(reported),
            "{case} {render:?}: {stderr}"
        );
        assert!(!picture.exists(), "{case} {render:?}");
    }
}

/// Checks that `args` answers `index_corrupt`, exit 5, its message ending
/// in `reported`.
fn is_index_corrupt(env: &TestEnv, dir: &Path, case: &str, args: &[&str], reported: &str) {
    let (code, envelope) = answer(env, dir, args);
    assert_eq!(code, Some(5), "{case} {args:?}: {envelope}");
    assert_eq!(
        envelope["error"]["code"], "index_corrupt",
        "{case} {args:?}"
    );
    let message = envelope["error"]["message"].as_str().unwrap();
    assert!(message.ends_with(reported), "{case} {args:?}: {message}");
}

/// Saves the scan, breaks a copy of it with `sql`, and checks that each
/// verb that reads the broken row answers `index_corrupt` naming `reported`.
fn each_verb_is_index_corrupt(case: &str, sql: &str, reported: &str) {
    let env = TestEnv::new();
    let id = saved(&env);
    let dir = broken(&env, &id, case, sql);
    let corrupt = |args: &[&str]| is_index_corrupt(&env, &dir, case, args, reported);
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

#[test]
fn p5_sec_04_scans_check_reports_rows_out_of_layout() {
    let env = TestEnv::new();
    let id = saved(&env);
    let check = ["scans", "--check", id.as_str()];
    // A copy with nothing changed passes.
    let clean = broken(&env, &id, "clean", "");
    let (code, envelope) = answer(&env, &clean, &check);
    assert_eq!(code, Some(0), "{envelope}");
    assert_eq!(envelope["entries"][0]["quick_check"], "ok");
    assert_eq!(envelope["entries"][0]["structurally_stale"], false);
    for (case, sql, reported) in [
        (
            "cycle",
            "UPDATE dirs SET parent_id = 3 WHERE id = 2",
            "dirs row 2: its parent_id 3 is not below its id",
        ),
        (
            "self-parent",
            "UPDATE dirs SET parent_id = 2 WHERE id = 2",
            "dirs row 2: its parent_id 2 is not below its id",
        ),
        (
            "last-before-id",
            "UPDATE dirs SET last_id = 1 WHERE id = 2",
            "dirs row 2: id <= files_end <= last_id < n_entries fails (files_end 2, last_id 1, \
             n_entries 6)",
        ),
        (
            "last-past-the-scan",
            "UPDATE dirs SET last_id = 6 WHERE id = 2",
            "dirs row 2: id <= files_end <= last_id < n_entries fails (files_end 2, last_id 6, \
             n_entries 6)",
        ),
        // A file's row, and what the reader's reading of a row refuses
        // beside the comparisons: the sweep holds rows to the same terms.
        (
            "file-self-parent",
            "UPDATE files SET parent_id = 4 WHERE id = 4",
            "files row 4: its parent_id 4 is not below its id",
        ),
        (
            "no-parent",
            "UPDATE dirs SET parent_id = NULL WHERE id = 2",
            "dirs row 2: its parent_id is NULL, which only the root's may be",
        ),
        // rusqlite names the value that does not fit: any message will do.
        (
            "negative-parent",
            "UPDATE dirs SET parent_id = -1 WHERE id = 2",
            "",
        ),
        // `n_entries` as the reader takes it: gone reads as 0.
        (
            "no-entry-count",
            "DELETE FROM scan_meta WHERE key = 'n_entries'",
            "dirs row 0: id <= files_end <= last_id < n_entries fails (files_end 1, last_id 5, \
             n_entries 0)",
        ),
    ] {
        let dir = broken(&env, &id, case, sql);
        is_index_corrupt(&env, &dir, case, &check, reported);
    }
}

#[test]
fn p5_sec_04_scans_check_reads_the_rows_and_not_an_index_over_them() {
    let env = TestEnv::new();
    let id = saved(&env);
    // `f` is made its own parent behind an index that still holds its old
    // parent, which `PRAGMA quick_check` does not compare with the table
    // and `SQLite` would answer from where the index covers a statement.
    let dir = broken(
        &env,
        &id,
        "hidden",
        "CREATE INDEX hide ON files (parent_id);
         PRAGMA writable_schema = ON;
         UPDATE sqlite_schema SET sql = 'CREATE INDEX hide ON files (name)' WHERE name = 'hide';
         PRAGMA writable_schema = RESET;
         UPDATE files SET parent_id = 4 WHERE id = 4;
         PRAGMA writable_schema = ON;
         UPDATE sqlite_schema SET sql = 'CREATE INDEX hide ON files (parent_id)'
             WHERE name = 'hide';
         PRAGMA writable_schema = RESET;",
    );
    is_index_corrupt(
        &env,
        &dir,
        "hidden",
        &["scans", "--check", &id],
        "files row 4: its parent_id 4 is not below its id",
    );
}

/// The promise of `SECURITY.md` ("Index files"): a verb that never reads a
/// damaged row answers from the rows it did read, and says nothing of the
/// damage. `a` and `b` name each other as parent, so no row names the root
/// as `a`'s parent, and the walk down the root's subdirectories (a root with
/// no file of its own) that `top` takes reads neither.
#[test]
fn p5_sec_04_a_verb_that_does_not_read_the_damaged_rows_answers_from_those_it_read() {
    let env = TestEnv::new();
    let id = saved_with(&env, false);
    let dir = broken_ids(
        &env,
        &id,
        "cut-off",
        "UPDATE dirs SET parent_id = 2 WHERE id = 1; UPDATE dirs SET parent_id = 1 WHERE id = 2",
        &[(1, "a"), (2, "b"), (4, "c")],
    );
    let (code, envelope) = answer(&env, &dir, &["top", "--scan", &id]);
    assert_eq!(code, Some(0), "{envelope}");
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["warnings"], Value::Array(Vec::new()));
    let root = tree_root(&env);
    let paths: Vec<&str> = envelope["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths, [root.join("c").to_str().unwrap()]);
    // A verb that reads the rows reports them; `scans --check` reads them
    // all.
    let reported = "dirs row 1: its parent_id 2 is not below its id";
    is_index_corrupt(
        &env,
        &dir,
        "cut-off",
        &["top", "--files", "--scan", &id],
        reported,
    );
    is_index_corrupt(&env, &dir, "cut-off", &["scans", "--check", &id], reported);
    // `tree` lists the root's children whole, and finds that its
    // subdirectories leave out ids its subtree holds.
    is_index_corrupt(
        &env,
        &dir,
        "cut-off",
        &["tree", "--scan", &id, "--depth", "9"],
        "dirs row 0: the subtrees of its subdirectories do not fill its ids from files_end + 1 \
         to last_id (files_end 0, last_id 4)",
    );
}

/// A chain of directories, `chain/d01/d02/…/d19` with `f` in the last, so
/// that its ids are 0 for `chain`, 1 to 19 for `d01` to `d19` and 20 for
/// `f`, saved twice, `f` grown in between: the two scans' ids.
fn chain(env: &TestEnv) -> (String, String) {
    let root = fs::canonicalize(env.tmp_dir()).unwrap().join("chain");
    let last = (1..20).fold(root.clone(), |dir, n| dir.join(format!("d{n:02}")));
    fs::create_dir_all(&last).unwrap();
    fs::write(last.join("f"), [7; 100]).unwrap();
    let old = scan_saved(env, &root);
    fs::write(last.join("f"), vec![7; 1 << 20]).unwrap();
    let new = scan_saved(env, &root);
    (old, new)
}

/// Every directory of a chain takes in every id after it as a file of its
/// own: rows that each keep `id <= files_end <= last_id < n_entries`, so
/// the reader takes each one, but that do not nest. A walk that took them
/// as written listed each directory again under every directory above it,
/// 2^19 times for `f`: `tree` and `diff` (whose rows were `f`, 2^19 times)
/// took time exponential in the rows, and `scans --check` passed the file.
#[test]
fn p5_sec_04_directories_whose_files_take_in_their_subdirectories_are_index_corrupt() {
    let env = TestEnv::new();
    let (old, new) = chain(&env);
    let dir = env.tmp_dir().join("overlap");
    for id in [&old, &new] {
        let db = copy(&env, id, &dir);
        let last: u32 = db
            .query_row("SELECT max(id) FROM files", [], |row| row.get(0))
            .unwrap();
        assert_eq!(last, 20, "the ids the case names");
        db.execute("UPDATE dirs SET files_end = 20, last_id = 20", [])
            .unwrap();
    }
    let reported = "dirs row 0: its files_end takes in id 1 as a file, which is not a file whose \
                    parent_id is 0";
    let corrupt = |args: &[&str], reported: &str| {
        is_index_corrupt(&env, &dir, "overlap", args, reported);
    };
    corrupt(&["tree", "--scan", &old, "--depth", "30"], reported);
    corrupt(&["diff", &old, &new, "--limit", "5"], reported);
    render_is_index_corrupt(&env, &dir, "overlap", &["--scan", &old], reported);
    corrupt(
        &["scans", "--check", &old],
        "dirs row 1: it is a directory that its parent's files_end, 20, takes in as a file",
    );
}

/// `b` takes in `c`, the directory beside its parent `a`, as a file of its
/// own, and its subtree ends past `a`'s: rows that each keep `id <=
/// files_end <= last_id < n_entries`. A view of `a` read past its last id,
/// so `tree t/a` listed `c` as `b`'s file, and `render t/a` looked up a
/// class past the end of its list: a panic, an abort in a release build.
#[test]
fn p5_sec_04_a_subtree_that_ends_past_its_parents_is_index_corrupt() {
    let env = TestEnv::new();
    let id = saved(&env);
    let case = "past-the-parent";
    let dir = broken(
        &env,
        &id,
        case,
        "UPDATE dirs SET files_end = 5, last_id = 5 WHERE id = 3",
    );
    let a = tree_root(&env).join("a");
    let a = a.to_str().unwrap();
    render_is_index_corrupt(
        &env,
        &dir,
        case,
        &[a, "--scan", &id],
        "dirs row 3: its parent_id 2 and last_id 5 put it outside the subtree of the entry 2, \
         which ends at 4",
    );
    is_index_corrupt(
        &env,
        &dir,
        case,
        &["tree", a, "--scan", &id, "--depth", "5"],
        "dirs row 2: the subtrees of its subdirectories do not fill its ids from files_end + 1 \
         to last_id (files_end 2, last_id 4)",
    );
    is_index_corrupt(
        &env,
        &dir,
        case,
        &["scans", "--check", &id],
        "dirs row 3: its last_id 5 is past its parent's, 4",
    );
}

/// `n_entries` past the rows. A walk over the ids read each missing one
/// with a query of its own, about 160 hours for `top --files --sort mtime`
/// and as long for `render`, and `scans --check` passed the file. A count
/// past the highest id + 1 is refused at open; a count that a row at its
/// last id keeps leaves ids out below it, and a walk stops at the first.
#[test]
fn p5_sec_04_an_entry_count_past_the_rows_is_index_corrupt() {
    let env = TestEnv::new();
    let id = saved(&env);
    let count = "UPDATE scan_meta SET value = 4294967295 WHERE key = 'n_entries'; \
                 UPDATE dirs SET last_id = 4294967294 WHERE id = 0;";
    // A copy of `c`'s row at the last id the count leaves, which nests
    // where the root's ids place it.
    let row_at_the_end = "CREATE TEMP TABLE moved AS SELECT * FROM dirs WHERE id = 5; \
                          UPDATE moved SET id = 4294967294, files_end = 4294967294, \
                          last_id = 4294967294; \
                          INSERT INTO dirs SELECT * FROM moved;";
    for (case, sql, reported) in [
        (
            "count",
            count.to_owned(),
            "scan_meta.n_entries 4294967295 is more than the highest id + 1 (6)",
        ),
        (
            "gap",
            format!("{count} {row_at_the_end}"),
            "no row of dirs or files has id 6, below n_entries 4294967295",
        ),
    ] {
        let dir = broken(&env, &id, case, &sql);
        for args in [
            &["top", "--files", "--sort", "mtime", "--scan", &id][..],
            &["by-type", "--scan", &id],
            &["scans", "--check", &id],
        ] {
            is_index_corrupt(&env, &dir, case, args, reported);
        }
        render_is_index_corrupt(&env, &dir, case, &["--scan", &id], reported);
    }
}
