//! P2-TEST-01: the phase-2 suite's goldens. `scans` in both formats, and
//! `--plain` of the phase-2 verbs whose `--json` goldens live beside their
//! packages' tests (summary, diff, schema, explain, import), on a small
//! tree of fixed sizes and times.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::{Duration, SystemTime};

use duscope_testkit::{TestEnv, golden, replace_values};
use serde_json::Value;

const MIB: usize = 1 << 20;

fn run(env: &TestEnv, args: &[&str]) -> Output {
    env.cmd().args(args).output().unwrap()
}

fn ok(out: &Output) -> String {
    assert!(
        out.status.success(),
        "exit {:?}: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout.clone()).unwrap()
}

/// 2026-01-01T00:00:00Z.
fn fixed_time() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_767_225_600)
}

/// Gives `path` and everything under it the fixed modification time, the
/// deepest first, so that setting a child's leaves its parent's alone.
fn touch(path: &Path) {
    if path.is_dir() {
        for entry in fs::read_dir(path).unwrap() {
            touch(&entry.unwrap().path());
        }
    }
    File::open(path)
        .unwrap()
        .set_modified(fixed_time())
        .unwrap();
}

/// A tree of files of known sizes, resolved.
fn tree(env: &TestEnv) -> PathBuf {
    let root = env.tmp_dir().join("tree");
    for (path, size) in [
        ("docs/a.bin", 3 * MIB),
        ("docs/b.bin", 2 * MIB),
        ("docs/small.txt", 1000),
        ("old/x/1.bin", MIB),
        ("photos/p.jpg", 4 * MIB),
    ] {
        let file = root.join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, vec![1u8; size]).unwrap();
    }
    touch(&root);
    fs::canonicalize(root).unwrap()
}

/// `text` without what differs between runs: the shared normalisation,
/// the test's temporary and index directories, and a summary's duration.
fn mask(env: &TestEnv, text: &str, root: &Path) -> String {
    let mut text = duscope_testkit::normalize(text, root)
        .replace(env.index_dir().to_str().unwrap(), "<index dir>")
        .replace(env.tmp_dir().to_str().unwrap(), "<tmp>");
    replace_values(&mut text, "duration_ms\t", '\n', "<ms>");
    text
}

/// Saves a scan of `path` taken at `now`, its id. Scans taken at one
/// instant would list in the order of their ids, which are random.
fn save(env: &TestEnv, path: &Path, now: &str) -> String {
    let out = env
        .cmd()
        .args(["scan", path.to_str().unwrap(), "--save", "--threads", "4"])
        .arg("--json")
        .env("DUSCOPE_TEST_NOW", now)
        .output()
        .unwrap();
    let scan: Value = serde_json::from_str(&ok(&out)).unwrap();
    scan["scan"]["id"].as_str().unwrap().to_owned()
}

#[test]
fn p2_test_01_scans_lists_the_saved_scans_in_both_formats() {
    let env = TestEnv::new();
    let root = tree(&env);
    let os = std::env::consts::OS;
    let ids = [
        save(&env, &root, "2026-06-01T00:00:00Z"),
        save(&env, &root.join("docs"), "2026-06-02T00:00:00Z"),
    ];
    // The ids, in the index files' names too.
    let unnamed = |text: String| {
        ids.iter()
            .fold(text, |text, id| text.replace(id.as_str(), "<id>"))
    };
    let mut json = unnamed(mask(&env, &ok(&run(&env, &["scans", "--json"])), &root));
    replace_values(&mut json, "\"index_bytes\":", ',', "<bytes>");
    golden(&format!("scans_json_two@{os}.snap"), &json);
    let plain = unnamed(mask(&env, &ok(&run(&env, &["scans", "--plain"])), &root));
    golden(&format!("scans_plain_two@{os}.snap"), &plain);
}

#[test]
fn p2_test_01_plain_goldens_of_the_phase_2_verbs() {
    let env = TestEnv::new();
    let root = tree(&env);
    let r = root.to_str().unwrap();
    let os = std::env::consts::OS;
    let before = save(&env, &root, "2026-06-01T00:00:00Z");
    let plain = |args: &[&str]| mask(&env, &ok(&run(&env, args)), &root);
    golden(
        &format!("summary_plain_tree@{os}.snap"),
        &plain(&["summary", r, "--plain"]),
    );
    golden(
        &format!("schema_plain_tree@{os}.snap"),
        &plain(&["schema", r, "--plain"]),
    );
    let file = root.join("docs/a.bin");
    golden(
        &format!("explain_plain_file@{os}.snap"),
        &plain(&["explain", file.to_str().unwrap(), "--plain"]),
    );
    // The scan exported as ncdu's JSON, and imported back.
    let export = env.tmp_dir().join("tree.json");
    ok(&run(
        &env,
        &[
            "export",
            r,
            "--format",
            "ncdu",
            "-o",
            export.to_str().unwrap(),
        ],
    ));
    golden(
        &format!("import_plain_ncdu@{os}.snap"),
        &plain(&["import", export.to_str().unwrap(), "--plain"]),
    );
    // A change set: a file grown, one added, a directory removed.
    fs::write(root.join("docs/a.bin"), vec![2u8; 5 * MIB]).unwrap();
    fs::write(root.join("docs/new.bin"), vec![2u8; 6 * MIB]).unwrap();
    fs::remove_dir_all(root.join("old")).unwrap();
    touch(&root);
    let after = save(&env, &root, "2026-06-02T00:00:00Z");
    let diff = plain(&["diff", &before, &after, "--plain"])
        .replace(&before, "<old>")
        .replace(&after, "<new>");
    golden(&format!("diff_plain_changes@{os}.snap"), &diff);
}
