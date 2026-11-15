//! P5-SEC-01: a write a symbolic link planted by someone else turned onto
//! another of the user's files, since v0.2.0. The test plants the link as
//! that person would and runs the binary.
//!
//! `export -o FILE` writes the export to `.FILE.<pid>.part` beside the file
//! first. It opened that name with `File::create`, which follows a link
//! there, and the name is guessable, so in a directory others may write
//! (`/tmp`) a link planted at it had the export overwrite the file it led
//! to.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fs;
use std::path::PathBuf;

use duscope_testkit::{TestEnv, duscope_bin};

/// What the victim of each test holds, before and, for the test to pass,
/// after.
const MINE: &[u8] = b"another file of the user's";

/// A file of the user's outside the folders the command writes to.
fn victim(env: &TestEnv) -> PathBuf {
    let victim = env.tmp_dir().join("victim");
    fs::write(&victim, MINE).unwrap();
    victim
}

#[test]
fn p5_sec_01_an_export_never_writes_through_a_link_at_its_temporary_name() {
    let env = TestEnv::new();
    let tree = env.tmp_dir().join("tree");
    fs::create_dir_all(tree.join("sub")).unwrap();
    fs::write(tree.join("sub/file.bin"), vec![b'x'; 10_000]).unwrap();
    let saved = env
        .cmd()
        .args(["scan", tree.to_str().unwrap(), "--save", "--json"])
        .output()
        .unwrap();
    assert!(saved.status.success());
    let shared = env.tmp_dir().join("shared");
    fs::create_dir_all(&shared).unwrap();
    let victim = victim(&env);
    // The shell plants the link at the name the export will take, its own
    // pid's, then becomes duscope, which keeps that pid.
    let out = env
        .command("/bin/sh")
        .arg("-c")
        .arg(
            "ln -s \"$1\" \"$2/.out.json.$$.part\" && \
             exec \"$0\" export \"$3\" --format ncdu -o \"$2/out.json\"",
        )
        .arg(duscope_bin())
        .arg(&victim)
        .arg(&shared)
        .arg(&tree)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        fs::read(&victim).unwrap(),
        MINE,
        "the export wrote through the link: {stderr}"
    );
    assert!(
        fs::symlink_metadata(shared.join("out.json")).is_err(),
        "out.json was made: {stderr}"
    );
    // The export fails, naming the temporary file whose name was taken,
    // not the output, which is not there.
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    let line = stderr.lines().next().unwrap_or_default();
    assert!(
        line.contains("/.out.json.") && line.contains(".part"),
        "{stderr}"
    );
}
