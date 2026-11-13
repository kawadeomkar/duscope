//! P5-CLI-03: a save that fails before anything was printed (`query --sql`
//! and `export`, which read the index of a scan run for them; `import`
//! with no index directory) is the verb's failure as any other: the error
//! envelope on stdout under `--json` and `--format ndjson`, two lines on
//! stderr otherwise. One that fails after the output (`scan --save`, the
//! list verbs, `rescan`, `import`) leaves the output standing, with the
//! failure on stderr alone.
//!
//! The index directory's `scans` is a regular file, so no scan can be
//! saved, as root too.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use duscope_testkit::TestEnv;
use serde_json::Value;

/// The hint of a scan run for a verb that cannot be saved.
const HINT: &str = "Rerun with --no-save, or with --index-dir naming a writable directory";

/// A small tree.
fn tree(env: &TestEnv) -> PathBuf {
    let root = env.work_dir().join("tree");
    fs::create_dir_all(root.join("a")).unwrap();
    fs::write(root.join("a/file"), vec![b'x'; 50_000]).unwrap();
    fs::write(root.join("top-file"), vec![b'x'; 9_000]).unwrap();
    fs::canonicalize(root).unwrap()
}

/// An index directory where no scan can be saved: its `scans` is a file.
fn unwritable(env: &TestEnv) -> PathBuf {
    let dir = env.tmp_dir().join("unwritable");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("scans"), b"").unwrap();
    dir
}

fn run(env: &TestEnv, dir: &Path, args: &[&str]) -> Output {
    env.cmd()
        .args(args)
        .env("DUSCOPE_INDEX_DIR", dir)
        .output()
        .unwrap()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// The one JSON value on stdout.
fn envelope(out: &Output) -> Value {
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(text.lines().count(), 1, "{text}");
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}: {text}"))
}

/// `out` failed `index_write_failed` (exit 1) with the error envelope on
/// stdout, the failure nowhere else, and `hint`; its message.
fn failed_with_envelope(out: &Output, hint: &str) -> String {
    assert_eq!(out.status.code(), Some(1), "{}", stderr(out));
    let envelope = envelope(out);
    assert_eq!(envelope["ok"], false, "{envelope}");
    let error = &envelope["error"];
    assert_eq!(error["code"], "index_write_failed", "{envelope}");
    assert_eq!(error["hint"], hint, "{envelope}");
    assert_eq!(error["retryable"], true, "{envelope}");
    let message = error["message"].as_str().unwrap().to_owned();
    let err = stderr(out);
    assert!(!err.contains(&message), "the failure on stderr too: {err}");
    assert!(!err.contains(hint), "the hint on stderr too: {err}");
    message
}

/// `out` failed `index_write_failed` (exit 1) with stdout empty and the
/// failure's two lines on stderr.
fn failed_on_stderr(out: &Output, hint: &str) {
    assert_eq!(out.status.code(), Some(1), "{}", stderr(out));
    assert!(
        out.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let err = stderr(out);
    assert!(err.contains("duscope: cannot write the index"), "{err}");
    assert!(err.contains(&format!("hint: {hint}")), "{err}");
}

/// `out` printed its envelope, `ok: true` with the id the scan was to
/// have, then failed `index_write_failed` (exit 1) on stderr alone.
fn printed_then_failed(out: &Output) {
    assert_eq!(out.status.code(), Some(1), "{}", stderr(out));
    let envelope = envelope(out);
    assert_eq!(envelope["ok"], true, "{envelope}");
    assert!(envelope["scan"]["id"].is_string(), "{envelope}");
    let err = stderr(out);
    assert!(err.contains("duscope: cannot write the index"), "{err}");
}

#[test]
fn p5_cli_03_query_answers_the_error_envelope_when_its_save_fails() {
    let env = TestEnv::new();
    let root = tree(&env);
    let root = root.to_str().unwrap();
    let dir = unwritable(&env);
    let query = |format: &[&str]| {
        let mut args = vec!["query", root, "--sql", "SELECT 1"];
        args.extend(format);
        run(&env, &dir, &args)
    };
    for format in [&["--json"][..], &["--format", "ndjson"]] {
        let message = failed_with_envelope(&query(format), HINT);
        assert!(message.starts_with("cannot write the index"), "{message}");
    }
    failed_on_stderr(&query(&["--plain"]), HINT);
    failed_on_stderr(&query(&[]), HINT);
    // Nothing was saved, and the same statement answers under --no-save.
    assert!(dir.join("scans").is_file());
    let out = query(&["--json", "--no-save"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(envelope(&out)["ok"], true);
}

#[test]
fn p5_cli_03_export_answers_the_error_envelope_when_its_save_fails() {
    let env = TestEnv::new();
    let root = tree(&env);
    let root = root.to_str().unwrap();
    let dir = unwritable(&env);
    let message = failed_with_envelope(
        &run(&env, &dir, &["export", root, "--format", "ndjson"]),
        HINT,
    );
    assert!(message.starts_with("cannot write the index"), "{message}");
    for format in ["ncdu", "csv"] {
        failed_on_stderr(
            &run(&env, &dir, &["export", root, "--format", format]),
            HINT,
        );
    }
    // An -o file is left as it was.
    let file = env.tmp_dir().join("export.json");
    fs::write(&file, b"before").unwrap();
    let out = run(
        &env,
        &dir,
        &[
            "export",
            root,
            "--format",
            "ncdu",
            "-o",
            file.to_str().unwrap(),
        ],
    );
    failed_on_stderr(&out, HINT);
    assert_eq!(fs::read(&file).unwrap(), b"before");
}

#[test]
fn p5_cli_03_import_with_no_index_directory_answers_the_error_envelope() {
    let env = TestEnv::new();
    let file = env.tmp_dir().join("tree.json");
    fs::write(
        &file,
        br#"[1,2,{"progname":"ncdu","progver":"1.19","timestamp":1800000000},
[{"name":"/imported"},{"name":"a","asize":10,"dsize":4096}]]"#,
    )
    .unwrap();
    let import = |format: &str| {
        env.cmd()
            .args(["import", file.to_str().unwrap(), format])
            .env_remove("DUSCOPE_INDEX_DIR")
            .env_remove("XDG_CACHE_HOME")
            .env_remove("HOME")
            .output()
            .unwrap()
    };
    let hint = "Run duscope import with --index-dir naming a writable directory";
    let message = failed_with_envelope(&import("--json"), hint);
    assert!(message.starts_with("no index directory"), "{message}");
    let out = import("--plain");
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    assert!(
        stderr(&out).contains("duscope: no index directory"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn p5_cli_03_a_save_after_the_output_leaves_the_output_standing() {
    let env = TestEnv::new();
    let root = tree(&env);
    let root = root.to_str().unwrap();
    let dir = unwritable(&env);
    printed_then_failed(&run(&env, &dir, &["scan", root, "--save", "--json"]));
    printed_then_failed(&run(&env, &dir, &["top", root, "--json"]));
    printed_then_failed(&run(&env, &dir, &["summary", root, "--json"]));
    // A rescan of a scan saved elsewhere, and an import of its export,
    // saved where nothing can be.
    let saved = env
        .cmd()
        .args(["scan", root, "--save", "--json"])
        .output()
        .unwrap();
    assert!(saved.status.success(), "{}", stderr(&saved));
    let id = envelope(&saved)["scan"]["id"].as_str().unwrap().to_owned();
    let index = env.index_dir().join("scans").join(format!("{id}.db"));
    let index = index.to_str().unwrap();
    printed_then_failed(&run(
        &env,
        &dir,
        &["rescan", "--scan", index, "--trust", index, "--json"],
    ));
    let export = env.tmp_dir().join("tree.ncdu.json");
    let out = env
        .cmd()
        .args(["export", "--scan", index, "--format", "ncdu", "-o"])
        .arg(&export)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    printed_then_failed(&run(
        &env,
        &dir,
        &["import", export.to_str().unwrap(), "--json"],
    ));
}
