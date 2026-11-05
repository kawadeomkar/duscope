//! P5-CORE-02: `scans --check` runs `PRAGMA quick_check` and sweeps every
//! directory the saved scan holds: exit 0 when the tree is as the scan
//! found it, 1 (`index_stale`, the row printed with `structurally_stale`)
//! when a directory changed, 5 (`index_corrupt`) when the file is damaged.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fs::{self, Permissions};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::Duration;

use duscope_testkit::TestEnv;
use serde_json::Value;

fn run(env: &TestEnv, args: &[&str]) -> Output {
    env.cmd().args(args).output().unwrap()
}

fn envelope(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "{e}: stdout {} stderr {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// A small tree under the environment's work directory.
fn tree(env: &TestEnv) -> PathBuf {
    let root = env.work_dir().join("tree");
    for dir in ["src/core", "src/cli", "docs", "build/out"] {
        fs::create_dir_all(root.join(dir)).unwrap();
    }
    fs::write(root.join("src/core/lib.rs"), b"pub fn f() {}\n").unwrap();
    fs::write(root.join("docs/readme.md"), b"# tree\n").unwrap();
    root
}

/// Scans `root` and saves it, with `flags`; the saved scan's id.
fn save(env: &TestEnv, root: &Path, flags: &[&str]) -> String {
    let mut args = vec!["scan", root.to_str().unwrap(), "--save", "--json"];
    args.extend(flags);
    let out = run(env, &args);
    assert!(out.status.success(), "{}", stderr(&out));
    let id = envelope(&out)["scan"]["id"].as_str().unwrap().to_owned();
    // A change after the save must get a later time than the tree's own,
    // and Linux stamps times from a clock that moves once a tick.
    std::thread::sleep(Duration::from_millis(25));
    id
}

#[test]
fn p5_core_02_an_unchanged_tree_exits_0() {
    let env = TestEnv::new();
    let root = tree(&env);
    let id = save(&env, &root, &[]);
    // A file written in place changes no directory.
    fs::write(root.join("src/core/lib.rs"), b"pub fn g() {}\n").unwrap();
    let out = run(&env, &["scans", "--check", &id, "--json"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let checked = envelope(&out);
    assert_eq!(checked["ok"], true);
    assert_eq!(checked["query"]["command"], "scans --check");
    let row = &checked["entries"][0];
    assert_eq!(row["id"], id.as_str());
    assert_eq!(row["quick_check"], "ok");
    assert_eq!(row["structurally_stale"], false);
    assert!(
        stderr(&out).contains("its 7 directories are unchanged"),
        "{}",
        stderr(&out)
    );
    let next = checked["next"].as_array().unwrap();
    assert!(next[0].as_str().unwrap().starts_with("duscope summary "));
}

#[test]
fn p5_core_02_a_changed_directory_exits_1_with_index_stale() {
    let env = TestEnv::new();
    let root = tree(&env);
    let id = save(&env, &root, &[]);
    fs::create_dir(root.join("src/cli/new")).unwrap();
    let out = run(&env, &["scans", "--check", &id, "--json"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    // The row says so, and the failure goes to stderr beside it: one
    // envelope on stdout.
    let checked = envelope(&out);
    assert_eq!(checked["ok"], false);
    assert_eq!(checked["entries"][0]["quick_check"], "ok");
    assert_eq!(checked["entries"][0]["structurally_stale"], true);
    let next = checked["next"].as_array().unwrap();
    assert_eq!(next.len(), 1);
    assert!(next[0].as_str().unwrap().starts_with("duscope rescan "));
    let said = stderr(&out);
    assert!(
        said.contains(&format!(
            "1 of 7 directories changed since the scan, {} first",
            root.join("src/cli").display()
        )),
        "{said}"
    );
    assert!(
        said.contains(&format!("the saved scan {id} is stale")),
        "{said}"
    );
    assert!(
        said.contains(&format!("hint: Run duscope rescan --scan {id}")),
        "{said}"
    );
    // A mode changed and a directory gone count as well; --plain says so
    // in its third field.
    fs::set_permissions(root.join("docs"), Permissions::from_mode(0o700)).unwrap();
    fs::remove_dir_all(root.join("build")).unwrap();
    let out = run(&env, &["scans", "--check", &id, "--plain"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let plain = String::from_utf8_lossy(&out.stdout);
    let fields: Vec<&str> = plain.trim_end().split('\t').collect();
    assert_eq!(fields[..3], [id.as_str(), "ok", "true"], "{plain}");
    assert!(
        stderr(&out).contains("5 of 7 directories changed"),
        "{}",
        stderr(&out)
    );
    // The rescan the hint names renews it: the new scan checks fresh.
    let renewed = run(&env, &["rescan", "--scan", &id, "--json"]);
    assert!(renewed.status.success(), "{}", stderr(&renewed));
    let child = envelope(&renewed)["scan"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let out = run(&env, &["scans", "--check", &child, "--json"]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
}

#[test]
fn p5_core_02_a_damaged_file_exits_5_before_any_sweep() {
    let env = TestEnv::new();
    let root = tree(&env);
    let id = save(&env, &root, &[]);
    let file = env.index_dir().join("scans").join(format!("{id}.db"));
    let mut bytes = fs::read(&file).unwrap();
    // The second page onwards overwritten: the header still says SQLite.
    for b in &mut bytes[4096..] {
        *b = 0xa5;
    }
    fs::write(&file, &bytes).unwrap();
    // The tree changed too: damage wins, since nothing in the file can be
    // trusted to compare with.
    fs::create_dir(root.join("docs/new")).unwrap();
    let out = run(&env, &["scans", "--check", &id, "--json"]);
    assert_eq!(out.status.code(), Some(5), "{}", stderr(&out));
    assert_eq!(envelope(&out)["error"]["code"], "index_corrupt");
}

#[test]
fn p5_core_02_the_index_directory_inside_the_tree_is_left_out() {
    let env = TestEnv::new();
    let root = tree(&env);
    // The index directory inside the scanned tree, as ~/.cache/duscope is
    // inside a scan of ~: the save writes its file there after the walk.
    let dir = root.join("cache");
    fs::create_dir(&dir).unwrap();
    fs::set_permissions(&dir, Permissions::from_mode(0o700)).unwrap();
    let flag = dir.to_str().unwrap();
    let id = save(&env, &root, &["--index-dir", flag]);
    // Named with a detour, which the sweep resolves.
    let detour = root.join("src/../cache");
    let detour = detour.to_str().unwrap();
    let out = run(
        &env,
        &["scans", "--check", &id, "--index-dir", detour, "--json"],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(envelope(&out)["entries"][0]["structurally_stale"], false);
    // The rest of the tree is still swept.
    fs::write(root.join("new-file"), b"").unwrap();
    let out = run(
        &env,
        &["scans", "--check", &id, "--index-dir", flag, "--json"],
    );
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(
        stderr(&out).contains(&format!("{} first", root.display())),
        "{}",
        stderr(&out)
    );
}
