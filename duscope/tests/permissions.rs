//! P5-SEC-01: what duscope keeps is its user's alone, whatever the umask. A
//! saved index is 0600 in a `scans` directory 0700, the handle key 0600, a
//! spilled envelope 0600 in an `out` directory 0700, and the index
//! directory 0700, with each directory made on the way to it. One function
//! makes each (`crates/duscope-core/src/index/writer.rs`'s `write_in` and
//! `build`, `handle.rs`'s `Key::create`, `duscope/src/out/spill.rs`'s
//! `write`; `fs_allowlist.txt` lists every place a file is made), and the
//! code is the same on both platforms. Each is run here through the binary
//! under umask 000, where a mode left to the umask is open to everyone.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Output;

use duscope_testkit::{TestEnv, duscope_bin};
use serde_json::Value;

/// Runs `program` with `args` under this environment, with umask 000.
fn under_umask_000(env: &TestEnv, program: &Path, args: &[&str]) -> Output {
    env.command("/bin/sh")
        .arg("-c")
        .arg("umask 000 && exec \"$0\" \"$@\"")
        .arg(program)
        .args(args)
        .output()
        .unwrap()
}

/// duscope's envelope on stdout, from a run that succeeded.
fn ok(out: &Output) -> Value {
    assert!(
        out.status.success(),
        "exit {:?}: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

/// The mode of `path`, in octal as `chmod` takes it.
fn mode(path: &Path) -> String {
    let mode = fs::symlink_metadata(path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .permissions()
        .mode();
    format!("{:04o}", mode & 0o7777)
}

/// A small tree under the environment's temporary directory.
fn tree(env: &TestEnv) -> PathBuf {
    let root = env.tmp_dir().join("tree");
    fs::create_dir_all(root.join("sub")).unwrap();
    fs::write(root.join("sub/big.bin"), vec![b'x'; 50_000]).unwrap();
    fs::write(root.join("small.txt"), b"text").unwrap();
    fs::canonicalize(root).unwrap()
}

/// The names in `dir`, sorted.
fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

#[test]
fn p5_sec_01_the_tests_umask_reaches_what_they_run() {
    // The control: what a program makes with the modes it asks for by
    // default is open to everyone under these tests' umask.
    let env = TestEnv::new();
    fs::create_dir_all(env.tmp_dir()).unwrap();
    let file = env.tmp_dir().join("touched");
    let dir = env.tmp_dir().join("made");
    let touched = under_umask_000(&env, Path::new("/usr/bin/touch"), &[file.to_str().unwrap()]);
    assert!(touched.status.success());
    let made = under_umask_000(&env, Path::new("/bin/mkdir"), &[dir.to_str().unwrap()]);
    assert!(made.status.success());
    assert_eq!(mode(&file), "0666");
    assert_eq!(mode(&dir), "0777");
}

#[test]
fn p5_sec_01_a_saved_index_and_its_directories_are_the_owners_alone_whatever_the_umask() {
    let env = TestEnv::new();
    let root = tree(&env);
    // An index directory nothing has made yet, two levels down.
    let above = env.tmp_dir().join("new");
    let index_dir = above.join("index");
    let index_arg = index_dir.to_str().unwrap();
    let scan = ok(&under_umask_000(
        &env,
        &duscope_bin(),
        &[
            "scan",
            root.to_str().unwrap(),
            "--save",
            "--index-dir",
            index_arg,
            "--json",
        ],
    ));
    let id = scan["scan"]["id"].as_str().unwrap().to_owned();
    for dir in [&above, &index_dir, &index_dir.join("scans")] {
        assert_eq!(mode(dir), "0700", "{}", dir.display());
    }
    // The index file, and nothing else: no temporary file is left.
    assert_eq!(names(&index_dir.join("scans")), [format!("{id}.db")]);
    assert_eq!(
        mode(&index_dir.join("scans").join(format!("{id}.db"))),
        "0600"
    );
    // A rescan saves through the same writer.
    let rescan = ok(&under_umask_000(
        &env,
        &duscope_bin(),
        &[
            "rescan",
            root.to_str().unwrap(),
            "--full",
            "--index-dir",
            index_arg,
            "--json",
        ],
    ));
    let again = rescan["scan"]["id"].as_str().unwrap().to_owned();
    assert_ne!(again, id);
    let mut both = [format!("{id}.db"), format!("{again}.db")];
    both.sort();
    assert_eq!(names(&index_dir.join("scans")), both);
    for name in both {
        let file = index_dir.join("scans").join(name);
        assert_eq!(mode(&file), "0600", "{}", file.display());
    }
}

#[test]
fn p5_sec_01_an_imported_index_and_its_directories_are_the_owners_alone_whatever_the_umask() {
    let env = TestEnv::new();
    let root = tree(&env);
    // An export of a scan of the tree, imported into an index directory
    // nothing has made yet.
    let first = env.tmp_dir().join("first");
    ok(&under_umask_000(
        &env,
        &duscope_bin(),
        &[
            "scan",
            root.to_str().unwrap(),
            "--save",
            "--index-dir",
            first.to_str().unwrap(),
            "--json",
        ],
    ));
    let export = env.tmp_dir().join("tree.ncdu.json");
    let exported = under_umask_000(
        &env,
        &duscope_bin(),
        &[
            "export",
            "--format",
            "ncdu",
            "--index-dir",
            first.to_str().unwrap(),
            "-o",
            export.to_str().unwrap(),
        ],
    );
    assert!(
        exported.status.success(),
        "{}",
        String::from_utf8_lossy(&exported.stderr)
    );
    let index_dir = env.tmp_dir().join("imported").join("index");
    let imported = ok(&under_umask_000(
        &env,
        &duscope_bin(),
        &[
            "import",
            export.to_str().unwrap(),
            "--index-dir",
            index_dir.to_str().unwrap(),
            "--json",
        ],
    ));
    let id = imported["scan"]["id"].as_str().unwrap();
    for dir in [
        index_dir.parent().unwrap(),
        &index_dir,
        &index_dir.join("scans"),
    ] {
        assert_eq!(mode(dir), "0700", "{}", dir.display());
    }
    assert_eq!(names(&index_dir.join("scans")), [format!("{id}.db")]);
    assert_eq!(
        mode(&index_dir.join("scans").join(format!("{id}.db"))),
        "0600"
    );
}

#[test]
fn p5_sec_01_the_handle_key_and_its_directories_are_the_owners_alone_whatever_the_umask() {
    let env = TestEnv::new();
    let root = tree(&env);
    // No saved scan, and no index directory yet: explain --json makes the
    // key, and the directories on the way to it.
    let above = env.tmp_dir().join("keyed");
    let index_dir = above.join("index");
    let explained = ok(&under_umask_000(
        &env,
        &duscope_bin(),
        &[
            "explain",
            root.join("small.txt").to_str().unwrap(),
            "--index-dir",
            index_dir.to_str().unwrap(),
            "--json",
        ],
    ));
    assert!(explained["result"]["handle"].is_string(), "{explained}");
    for dir in [&above, &index_dir] {
        assert_eq!(mode(dir), "0700", "{}", dir.display());
    }
    // The key alone: no staged file is left beside it.
    assert_eq!(names(&index_dir), ["handle.key"]);
    assert_eq!(mode(&index_dir.join("handle.key")), "0600");
}

#[test]
fn p5_sec_01_a_spill_and_its_directories_are_the_owners_alone_whatever_the_umask() {
    let env = TestEnv::new();
    let root = tree(&env);
    let above = env.tmp_dir().join("spilled");
    let index_dir = above.join("index");
    let stub = ok(&under_umask_000(
        &env,
        &duscope_bin(),
        &[
            "scan",
            root.to_str().unwrap(),
            "--index-dir",
            index_dir.to_str().unwrap(),
            "--json",
            "--max-chars",
            "1",
        ],
    ));
    let file = PathBuf::from(stub["spilled"]["path"].as_str().unwrap());
    assert_eq!(file.parent(), Some(index_dir.join("out").as_path()));
    assert_eq!(mode(&file), "0600", "{}", file.display());
    for dir in [&above, &index_dir, &index_dir.join("out")] {
        assert_eq!(mode(dir), "0700", "{}", dir.display());
    }
}
