//! P5-CLI-04: a hint names a command the user can run after the verb that
//! gave it. `partial_read` offers `--strict` only after a list verb, the
//! verbs that take it; `scan_age` names what renews the saved scan `stale`
//! read (a rescan of it, or for an import a new scan of its root), then
//! `stale` again; a failed save of `rescan` and of `import` names that
//! verb, and `rescan` names its saved scan by file, since `--index-dir`
//! also says where it looks for the scan.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fs::{self, Permissions};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Output;

use duscope_core::query::timegrammar;
use duscope_testkit::TestEnv;
use serde_json::Value;

/// `partial_read`'s hint after a verb that takes no `--strict`.
const UNREADABLE: &str = "Rerun duscope as a user who can read them";

/// `partial_read`'s hint after a list verb.
const UNREADABLE_STRICT: &str =
    "Rerun duscope as a user who can read them, or with --strict to fail instead";

/// The list verbs: the verbs that take `--strict`.
const LIST_VERBS: [&str; 5] = ["top", "tree", "by-type", "stale", "caches"];

fn run(env: &TestEnv, args: &[&str]) -> Output {
    env.cmd().args(args).output().unwrap()
}

/// `args` run `days` after the instant the environment's clock reads.
fn run_later(env: &TestEnv, days: i64, args: &[&str]) -> Output {
    let now = timegrammar::parse_date(env.test_now()).unwrap().ns;
    let later = timegrammar::iso8601(now + days * 24 * 3_600_000_000_000);
    env.cmd()
        .env("DUSCOPE_TEST_NOW", later)
        .args(args)
        .output()
        .unwrap()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn ok(out: &Output) -> Value {
    assert!(
        out.status.success(),
        "exit {:?}: {}",
        out.status.code(),
        stderr(out)
    );
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stdout)))
}

/// The hint of the warning `code` in `envelope`; `None` when there is no
/// such warning.
fn hint(envelope: &Value, code: &str) -> Option<String> {
    envelope["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["code"] == code)
        .map(|w| w["hint"].as_str().unwrap().to_owned())
}

/// A small tree under the work directory, its path resolved; a path a
/// shell takes as one word, unquoted.
fn tree(env: &TestEnv) -> PathBuf {
    let root = env.work_dir().join("tree");
    fs::create_dir_all(root.join("a")).unwrap();
    fs::write(root.join("a/file"), vec![b'x'; 50_000]).unwrap();
    fs::write(root.join("top-file"), vec![b'x'; 9_000]).unwrap();
    let root = fs::canonicalize(root).unwrap();
    let plain = root
        .to_str()
        .unwrap()
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "@%+=:,./_-".contains(c));
    assert!(plain, "{}", root.display());
    root
}

/// Saves a scan of `root` at the environment's instant: its id.
fn save(env: &TestEnv, root: &str) -> String {
    let saved = ok(&run(env, &["scan", root, "--save", "--json"]));
    saved["scan"]["id"].as_str().unwrap().to_owned()
}

/// The words of the command a hint names, from `Run duscope ` to the
/// first `,` or ` with ` after it.
fn command_of(hint: &str) -> Vec<String> {
    let rest = hint.strip_prefix("Run duscope ").unwrap();
    let end = [rest.find(','), rest.find(" with ")]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(rest.len());
    rest[..end].split(' ').map(str::to_owned).collect()
}

/// An index directory where no scan can be saved: its `scans` is a file,
/// which stops root too.
fn unwritable(env: &TestEnv) -> PathBuf {
    let dir = env.tmp_dir().join("unwritable");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("scans"), b"").unwrap();
    dir
}

#[test]
fn p5_cli_04_partial_read_offers_strict_only_to_the_list_verbs() {
    let env = TestEnv::new();
    let fixtures = env.fixtures();
    if !fixtures.unreadable_effective() {
        eprintln!("skipped: running as root, the mode-000 directory is readable");
        return;
    }
    let root = fixtures.case_root("unreadable");
    let root = root.to_str().unwrap();
    // scan, and a verb that reads a scan but lists nothing: neither takes
    // --strict, so the hint does not name it.
    let scanned = ok(&run(&env, &["scan", root, "--json"]));
    assert_eq!(hint(&scanned, "partial_read").unwrap(), UNREADABLE);
    let summary = ok(&run(&env, &["summary", root, "--json", "--no-save"]));
    assert_eq!(hint(&summary, "partial_read").unwrap(), UNREADABLE);
    for verb in ["scan", "summary"] {
        let out = run(&env, &[verb, root, "--strict", "--json"]);
        assert_eq!(out.status.code(), Some(2), "{verb}: {}", stderr(&out));
    }
    // The list verbs, on a scan run for them and on a saved one: the hint
    // offers --strict, which fails them with exit 4.
    for verb in LIST_VERBS {
        let fresh = ok(&run(&env, &[verb, root, "--json", "--no-save"]));
        assert_eq!(
            hint(&fresh, "partial_read").unwrap(),
            UNREADABLE_STRICT,
            "{verb}"
        );
    }
    let id = save(&env, root);
    for verb in LIST_VERBS {
        let saved = ok(&run(&env, &[verb, root, "--json"]));
        assert_eq!(saved["scan"]["id"], id.as_str(), "{verb}");
        assert_eq!(
            hint(&saved, "partial_read").unwrap(),
            UNREADABLE_STRICT,
            "{verb}"
        );
        let out = run(&env, &[verb, root, "--strict", "--json"]);
        assert_eq!(out.status.code(), Some(4), "{verb}: {}", stderr(&out));
        let error: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(error["error"]["code"], "permission_denied", "{verb}");
    }
    // A rescan of the saved scan takes no --strict either.
    let rescanned = ok(&run_later(&env, 1, &["rescan", "--scan", &id, "--json"]));
    assert_eq!(hint(&rescanned, "partial_read").unwrap(), UNREADABLE);
    let out = run_later(&env, 1, &["rescan", "--scan", &id, "--strict", "--json"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
}

#[test]
fn p5_cli_04_scan_age_names_the_rescan_that_renews_the_saved_scan() {
    let env = TestEnv::new();
    let root = tree(&env);
    let root = root.to_str().unwrap();
    let id = save(&env, root);
    // A month on, past a tenth of stale's 180 days: the hint names the
    // rescan of the saved scan stale read.
    let aged = ok(&run_later(&env, 30, &["stale", root, "--json"]));
    assert_eq!(aged["scan"]["id"], id.as_str());
    assert_eq!(
        hint(&aged, "scan_age").unwrap(),
        format!(
            "Run duscope rescan --scan {id}, then duscope stale again, to measure from a fresh scan"
        )
    );
    // Run as it says: the rescan, then stale again, which reads the
    // rescan and is no longer old.
    let words = command_of(&hint(&aged, "scan_age").unwrap());
    assert_eq!(words[0], "rescan");
    let mut args: Vec<&str> = words.iter().map(String::as_str).collect();
    args.push("--json");
    let rescanned = ok(&run_later(&env, 30, &args));
    assert_eq!(rescanned["scan"]["parent_scan_id"], id.as_str());
    let again = ok(&run_later(&env, 30, &["stale", root, "--json"]));
    assert_eq!(again["scan"]["id"], rescanned["scan"]["id"]);
    assert_eq!(hint(&again, "scan_age"), None, "{again}");
}

#[test]
fn p5_cli_04_scan_age_of_an_import_names_a_new_scan_of_its_root() {
    let env = TestEnv::new();
    let root = tree(&env);
    let root = root.to_str().unwrap();
    let id = save(&env, root);
    let file = env.tmp_dir().join("tree.ncdu.json");
    let out = env
        .cmd()
        .args(["export", "--scan", &id, "--format", "ncdu", "-o"])
        .arg(&file)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let imported = ok(&run(&env, &["import", file.to_str().unwrap(), "--json"]));
    let import_id = imported["scan"]["id"].as_str().unwrap().to_owned();
    // An import has no scan options to rescan with: the hint names a new
    // scan of its root.
    let aged = ok(&run_later(
        &env,
        30,
        &["stale", root, "--scan", &import_id, "--json"],
    ));
    assert_eq!(aged["scan"]["id"], import_id.as_str());
    assert_eq!(
        hint(&aged, "scan_age").unwrap(),
        format!(
            "Run duscope scan --save {root}, then duscope stale again, to measure from a fresh scan"
        )
    );
    let words = command_of(&hint(&aged, "scan_age").unwrap());
    let mut args: Vec<&str> = words.iter().map(String::as_str).collect();
    args.push("--json");
    let scanned = ok(&run_later(&env, 30, &args));
    let again = ok(&run_later(&env, 30, &["stale", root, "--json"]));
    assert_eq!(again["scan"]["id"], scanned["scan"]["id"]);
    assert_eq!(hint(&again, "scan_age"), None, "{again}");
}

#[test]
fn p5_cli_04_a_failed_save_names_the_verb_that_saves() {
    let env = TestEnv::new();
    let root = tree(&env);
    let root = root.to_str().unwrap();
    let dir = unwritable(&env);
    let writable = env.tmp_dir().join("writable");
    let in_dir = |dir: &Path, args: &[&str]| {
        env.cmd()
            .args(args)
            .env("DUSCOPE_INDEX_DIR", dir)
            .output()
            .unwrap()
    };
    // The output stands, and the failure's hint is on stderr.
    let failed = |out: &Output, hint: &str| {
        assert_eq!(out.status.code(), Some(1), "{}", stderr(out));
        let printed: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(printed["ok"], true, "{printed}");
        let err = stderr(out);
        assert!(err.contains("duscope: cannot write the index"), "{err}");
        assert!(err.contains(&format!("\nhint: {hint}\n")), "{err}");
    };
    // scan --save: its own command again.
    failed(
        &in_dir(&dir, &["scan", root, "--save", "--json"]),
        "Run duscope scan --save again with --index-dir naming a writable directory",
    );
    // rescan: the saved scan named by its file, which --index-dir does not
    // change.
    let id = save(&env, root);
    let file = env.index_dir().join("scans").join(format!("{id}.db"));
    let file = fs::canonicalize(file).unwrap();
    let file = file.to_str().unwrap();
    let rescan_hint = format!(
        "Run duscope rescan --trust {file} --scan {file} with --index-dir naming a writable \
         directory"
    );
    failed(
        &in_dir(&dir, &["rescan", "--scan", file, "--trust", file, "--json"]),
        &rescan_hint,
    );
    let mut args: Vec<String> = command_of(&rescan_hint);
    assert_eq!(args[0], "rescan");
    args.extend([
        "--index-dir".to_owned(),
        writable.to_str().unwrap().to_owned(),
    ]);
    args.push("--json".to_owned());
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let renewed = ok(&in_dir(&dir, &args));
    assert_eq!(renewed["scan"]["parent_scan_id"], id.as_str());
    let renewed_id = renewed["scan"]["id"].as_str().unwrap();
    assert!(writable.join(format!("scans/{renewed_id}.db")).is_file());
    // The same rescan by the path alone, its saved scan found in an index
    // directory that can no longer be written (permissions, which do not
    // stop root): the hint holds, where a rescan by the path in another
    // directory would find no saved scan.
    let scans = env.index_dir().join("scans");
    fs::set_permissions(&scans, Permissions::from_mode(0o555)).unwrap();
    let probe = scans.join("probe");
    if fs::write(&probe, b"").is_err() {
        failed(
            &run_later(&env, 1, &["rescan", root, "--json"]),
            &rescan_hint,
        );
        let empty = env.tmp_dir().join("empty");
        let out = run_later(
            &env,
            1,
            &["rescan", root, "--index-dir", empty.to_str().unwrap()],
        );
        assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    } else {
        fs::remove_file(&probe).unwrap();
        eprintln!("skipped the read-only index directory: permissions do not stop this user");
    }
    fs::set_permissions(&scans, Permissions::from_mode(0o755)).unwrap();
    // import: its own verb again.
    let export = env.tmp_dir().join("tree.ncdu.json");
    let out = env
        .cmd()
        .args(["export", "--scan", &id, "--format", "ncdu", "-o"])
        .arg(&export)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let export = export.to_str().unwrap();
    let import_hint = "Run duscope import again with --index-dir naming a writable directory";
    failed(&in_dir(&dir, &["import", export, "--json"]), import_hint);
    let imported = ok(&in_dir(
        &dir,
        &[
            "import",
            export,
            "--index-dir",
            writable.to_str().unwrap(),
            "--json",
        ],
    ));
    let import_id = imported["scan"]["id"].as_str().unwrap();
    assert!(writable.join(format!("scans/{import_id}.db")).is_file());
}
