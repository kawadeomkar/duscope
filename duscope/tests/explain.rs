//! P2-QRY-04: `explain` on the fixture tree: goldens for a hard-linked
//! file, a clone member, a `node_modules` with and without its
//! `package.json`, an ignored path, a precious path and a path no saved
//! scan holds; `tracked_inside` in a repository with an index; `--errors`.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use duscope_testkit::{TestEnv, golden, replace_values};

fn run(env: &TestEnv, args: &[&str]) -> Output {
    env.cmd().args(args).output().unwrap()
}

fn ok(out: &Output) -> serde_json::Value {
    assert!(
        out.status.success(),
        "exit {:?}: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

/// The checkout, the repository the fixture tree lies in (under `target/`,
/// which it ignores).
fn checkout() -> PathBuf {
    fs::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("..")).unwrap()
}

/// `text` without what differs between runs and machines: the shared
/// normalisation, the scan id, the entries' times and owners, and the
/// checkout's path.
fn normalize(text: &str, root: &Path, id: &str) -> String {
    let mut text = duscope_testkit::normalize(text, root);
    text = text.replace(id, "<id>");
    text = text.replace(checkout().to_str().unwrap(), "<checkout>");
    for key in ["mtime", "ctime", "atime", "btime"] {
        replace_values(&mut text, &format!("\"{key}\":\""), '"', "<time>");
    }
    for key in ["uid", "gid"] {
        replace_values(&mut text, &format!("\"{key}\":"), ',', "<id>");
    }
    text
}

#[test]
fn p2_qry_04_goldens_for_each_kind_of_fact() {
    let env = TestEnv::new();
    let root = fs::canonicalize(env.fixtures().root()).unwrap();
    let saved = ok(&run(
        &env,
        &[
            "scan",
            root.to_str().unwrap(),
            "--save",
            "--json",
            "--threads",
            "4",
        ],
    ));
    let id = saved["scan"]["id"].as_str().unwrap().to_owned();
    let loose = env.tmp_dir().join("loose.txt");
    fs::write(&loose, b"not in any scan").unwrap();
    let os = std::env::consts::OS;
    let cases: [(&str, PathBuf); 7] = [
        ("hard_link", root.join("cases/hard-links/a/original.bin")),
        ("clone_member", root.join("cases/clones/clone-1.img")),
        (
            "node_modules",
            root.join("home/Projects/npm-app/node_modules"),
        ),
        (
            "node_modules_no_marker",
            root.join("cases/no-marker/node_modules"),
        ),
        ("ignored", root.join("home/Projects/notes-repo/debug.log")),
        (
            "precious",
            root.join("home/Projects/notes-repo/important.cache"),
        ),
        ("unknown", loose.clone()),
    ];
    for (name, path) in &cases {
        let out = run(&env, &["explain", path.to_str().unwrap(), "--json"]);
        ok(&out);
        let text = String::from_utf8(out.stdout).unwrap();
        let text = normalize(&text, &root, &id).replace(env.tmp_dir().to_str().unwrap(), "<tmp>");
        golden(&format!("explain_json_{name}@{os}.snap"), &text);
    }
    // The facts the goldens show, said plainly.
    let facts = |path: &Path| ok(&run(&env, &["explain", path.to_str().unwrap(), "--json"]));
    let ignored = facts(&cases[4].1);
    assert_eq!(ignored["result"]["git"]["ignored"], true);
    assert_eq!(ignored["result"]["git"]["precious"], false);
    let precious = facts(&cases[5].1);
    assert_eq!(precious["result"]["git"]["precious"], true);
    let unknown = facts(&loose);
    assert!(unknown["scan"].is_null());
    assert_eq!(unknown["result"]["confidence"], "low");
}

/// Runs git in `dir`, or says why the test is left out where git is missing.
fn git(env: &TestEnv, dir: &Path, args: &[&str]) -> bool {
    match env.command("git").args(args).current_dir(dir).output() {
        Ok(out) if out.status.success() => true,
        Ok(out) => panic!("git {args:?}: {}", String::from_utf8_lossy(&out.stderr)),
        Err(e) => {
            duscope_testkit::skip(&format!("no git here ({e})"));
            false
        }
    }
}

#[test]
fn p2_qry_04_tracked_inside_reads_the_repository_index() {
    let env = TestEnv::new();
    let repo = env.tmp_dir().join("repo");
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::create_dir_all(repo.join("scratch")).unwrap();
    fs::write(repo.join("src/main.rs"), b"fn main() {}\n").unwrap();
    fs::write(repo.join("scratch/notes.txt"), b"later\n").unwrap();
    if !git(&env, &repo, &["init", "-q"]) {
        return;
    }
    git(&env, &repo, &["add", "src/main.rs"]);
    ok(&run(
        &env,
        &["scan", repo.to_str().unwrap(), "--save", "--json"],
    ));
    let facts = |rel: &str| {
        ok(&run(
            &env,
            &["explain", repo.join(rel).to_str().unwrap(), "--json"],
        ))["result"]["git"]
            .clone()
    };
    assert_eq!(facts("src")["tracked_inside"], true);
    assert_eq!(facts("src/main.rs")["tracked_inside"], true);
    assert_eq!(facts("scratch")["tracked_inside"], false);
    assert_eq!(facts("scratch")["ignored"], false);
    assert_eq!(
        facts("src")["repo_root"],
        fs::canonicalize(&repo).unwrap().to_str().unwrap()
    );
}

#[test]
fn p2_qry_04_errors_lists_what_a_scan_could_not_read() {
    let env = TestEnv::new();
    let fixtures = env.fixtures();
    if !fixtures.unreadable_effective() {
        return duscope_testkit::skip("root reads the unreadable directories");
    }
    let root = fs::canonicalize(fixtures.root()).unwrap();
    let saved = ok(&run(
        &env,
        &["scan", root.to_str().unwrap(), "--save", "--json"],
    ));
    let id = saved["scan"]["id"].as_str().unwrap();
    let listed = ok(&run(&env, &["explain", "--errors", id, "--json"]));
    let rows = listed["entries"].as_array().unwrap();
    assert!(!rows.is_empty());
    assert!(rows.iter().all(|r| r["errno"] == 13), "{rows:?}");
    assert!(
        rows.iter().any(|r| r["kind"] == "directory"
            && r["path"].as_str().unwrap().contains("cases/unreadable")),
        "{rows:?}"
    );
    assert_eq!(listed["query"]["command"], "explain --errors");
    assert_eq!(listed["scan"]["id"], id);
    // A path and --errors do not go together.
    let out = run(&env, &["explain", root.to_str().unwrap(), "--errors", id]);
    assert_eq!(out.status.code(), Some(2));
}
