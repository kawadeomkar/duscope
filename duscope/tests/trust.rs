//! P2-SEC-01: an index file outside the index directory is read only when
//! `--trust` names it, and saved scans open to other users are read with a
//! warning.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fs::{self, Permissions};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Output;

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

fn ok(out: &Output) -> Value {
    assert!(
        out.status.success(),
        "exit {:?}: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    envelope(out)
}

/// A refusal of `file`, whose path the hint gives as the shell takes it.
fn untrusted(out: &Output, file: &str) {
    assert_eq!(out.status.code(), Some(1));
    let error = &envelope(out)["error"];
    assert_eq!(error["code"], "untrusted_index");
    assert_eq!(
        error["hint"],
        format!("Rerun the command with --trust {file} if you trust the file")
    );
}

/// A tree under the environment's temporary directory, scanned and saved:
/// its root, and its index file.
fn saved(env: &TestEnv) -> (PathBuf, PathBuf) {
    let root = env.tmp_dir().join("tree");
    fs::create_dir_all(root.join("sub")).unwrap();
    fs::write(root.join("sub/big.bin"), vec![b'x'; 50_000]).unwrap();
    let root = fs::canonicalize(root).unwrap();
    let scan = ok(&run(
        env,
        &["scan", root.to_str().unwrap(), "--save", "--json"],
    ));
    let id = scan["scan"]["id"].as_str().unwrap();
    let file = env.index_dir().join("scans").join(format!("{id}.db"));
    (root, file)
}

fn s(path: &Path) -> String {
    path.to_str().unwrap().to_owned()
}

#[test]
fn p2_sec_01_an_index_outside_the_index_directory_needs_trust() {
    let env = TestEnv::new();
    let (root, inside) = saved(&env);
    let root = root.to_str().unwrap();
    let top = |scan: &Path, trust: &[&Path]| {
        let mut args = vec!["top".to_owned(), root.to_owned(), "--json".to_owned()];
        for file in trust {
            args.extend(["--trust".to_owned(), s(file)]);
        }
        args.extend(["--scan".to_owned(), s(scan)]);
        env.cmd().args(&args).output().unwrap()
    };
    // In the index directory, a path needs no --trust.
    ok(&top(&inside, &[]));
    // A copy elsewhere is refused before it is opened.
    let copy = env.tmp_dir().join("copy.db");
    fs::copy(&inside, &copy).unwrap();
    let copy = fs::canonicalize(copy).unwrap();
    untrusted(&top(&copy, &[]), &s(&copy));
    // The hint quotes a path the shell would split.
    let spaced = env.tmp_dir().join("my scans");
    fs::create_dir_all(&spaced).unwrap();
    let spaced = spaced.join("copy.db");
    fs::copy(&inside, &spaced).unwrap();
    untrusted(&top(&spaced, &[]), &format!("'{}'", spaced.display()));
    // --trust names it, by any path to it.
    let trusted = ok(&top(&copy, &[&copy]));
    let link = env.tmp_dir().join("link.db");
    symlink(&copy, &link).unwrap();
    ok(&top(&copy, &[&link]));
    // The next commands name the file trusted, as this one did.
    let next = trusted["next"].as_array().unwrap();
    assert!(!next.is_empty());
    for line in next {
        assert!(
            line.as_str()
                .unwrap()
                .contains(&format!("--trust {0} --scan {0}", copy.display())),
            "{line}"
        );
    }
    // Trusting one file trusts no other.
    let other = env.tmp_dir().join("other.db");
    fs::copy(&inside, &other).unwrap();
    untrusted(&top(&other, &[&copy]), &s(&other));
    // A link in the index directory to a file outside it is outside, and,
    // named as a saved scan, no saved scan: neither an id nor latest finds
    // it, nor does scans list it.
    let planted = env.index_dir().join("scans/planted.db");
    symlink(&other, &planted).unwrap();
    untrusted(&top(&planted, &[]), &s(&planted));
    fs::remove_file(&planted).unwrap();
    let planted = env.index_dir().join("scans/aaaaaaaaaaaa.db");
    symlink(&other, &planted).unwrap();
    let out = run(&env, &["top", root, "--scan", "aaaa", "--json"]);
    assert_eq!(envelope(&out)["error"]["code"], "scan_not_found");
    let scans = ok(&run(&env, &["scans", "--json"]));
    assert_eq!(scans["entries"].as_array().unwrap().len(), 1, "{scans}");
    fs::remove_file(&planted).unwrap();
    // Each file diff reads is trusted, or in the index directory.
    let diff = |trust: &[&str]| {
        let mut args = vec!["diff", "--json"];
        args.extend(trust);
        let (inside, copy) = (s(&inside), s(&copy));
        env.cmd().args(&args).args([inside, copy]).output().unwrap()
    };
    untrusted(&diff(&[]), &s(&copy));
    ok(&diff(&["--trust", &s(&copy)]));
    // With no index directory to be in, a file is outside it, wherever the
    // command runs.
    let out = env
        .cmd()
        .args(["top", root, "--scan", "./copy.db", "--json"])
        .current_dir(copy.parent().unwrap())
        .env_remove("DUSCOPE_INDEX_DIR")
        .env_remove("XDG_CACHE_HOME")
        .env_remove("HOME")
        .output()
        .unwrap();
    untrusted(&out, &s(&copy.parent().unwrap().join("./copy.db")));
}

#[test]
fn p2_sec_01_saved_scans_open_to_others_are_read_with_a_warning() {
    let env = TestEnv::new();
    let (root, file) = saved(&env);
    let root = root.to_str().unwrap();
    let stderr = |out: &Output| String::from_utf8_lossy(&out.stderr).into_owned();
    // The writer makes the scans private: no warning.
    let out = run(&env, &["top", root, "--json"]);
    ok(&out);
    assert!(!stderr(&out).contains("not private"), "{}", stderr(&out));
    // A directory others may list, then an index file others may read.
    let dir = env.index_dir();
    fs::set_permissions(&dir, Permissions::from_mode(0o755)).unwrap();
    let out = run(&env, &["top", root, "--json"]);
    ok(&out);
    assert!(
        stderr(&out).contains(&format!(
            "the saved scans are not private: {} is mode 755; run chmod 700 {}",
            dir.display(),
            dir.display()
        )),
        "{}",
        stderr(&out)
    );
    fs::set_permissions(&dir, Permissions::from_mode(0o700)).unwrap();
    fs::set_permissions(&file, Permissions::from_mode(0o644)).unwrap();
    let out = run(&env, &["top", root, "--json"]);
    ok(&out);
    assert!(
        stderr(&out).contains(&format!(
            "the saved scans are not private: others may read, or another user owns, 1 of \
             the index files in {0}; run chmod 600 {0}/*.db",
            dir.join("scans").display()
        )),
        "{}",
        stderr(&out)
    );
}
