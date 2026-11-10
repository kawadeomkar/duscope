//! P5-SEC-05: what duscope writes to stderr for people shows control
//! characters as escapes, as its tables do, so that a file name or an
//! argument in a message cannot drive the terminal.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use duscope_testkit::TestEnv;

/// A name holding ESC, BEL, DEL and a C1 control (CSI, as UTF-8).
const NAME: &[u8] = b"evil\x1b[31mRED\x07\x7f\xc2\x9b";

/// `NAME` as a message should show it.
const SHOWN: &str = "evil\\x1b[31mRED\\x07\\x7f\\x9b";

fn duscope(env: &TestEnv) -> Command {
    Command::from_std(env.cmd())
}

/// `dir/NAME`.
fn evil_path(dir: &Path) -> PathBuf {
    dir.join(OsStr::from_bytes(NAME))
}

/// `stderr`, checked to hold no control character but its newlines.
fn clean(stderr: &[u8]) -> String {
    let text = String::from_utf8(stderr.to_vec()).unwrap();
    let raw: Vec<char> = text
        .chars()
        .filter(|c| c.is_control() && *c != '\n')
        .collect();
    assert!(raw.is_empty(), "raw control characters {raw:?} in {text:?}");
    text
}

#[test]
fn p5_sec_05_a_failure_naming_a_path_shows_its_control_characters_as_escapes() {
    let env = TestEnv::new();
    let missing = evil_path(&env.work_dir());
    for verb in [["explain", "--plain"], ["scan", "--plain"]] {
        let out = duscope(&env).args(verb).arg(&missing).output().unwrap();
        assert!(!out.status.success(), "{verb:?}");
        let text = clean(&out.stderr);
        assert!(text.contains(SHOWN), "{verb:?}: {text}");
    }
}

#[test]
fn p5_sec_05_an_argument_clap_refuses_shows_its_control_characters_as_escapes() {
    let env = TestEnv::new();
    let out = duscope(&env)
        .arg("--plain")
        .arg(OsStr::from_bytes(b"--bad\x1b[31mflag"))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let text = clean(&out.stderr);
    assert!(text.contains("--bad\\x1b[31mflag"), "{text}");
    assert!(text.contains("error:"), "{text}");
    assert!(
        text.contains("Usage:"),
        "the rest of clap's text stays: {text}"
    );
    assert!(text.lines().count() > 1, "its lines stay lines: {text}");
}

#[test]
fn p5_sec_05_an_argument_with_no_control_character_keeps_clap_s_message() {
    let env = TestEnv::new();
    let out = duscope(&env).args(["--plain", "--bad"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    let text = clean(&out.stderr);
    assert!(text.contains("unexpected argument '--bad'"), "{text}");
}

#[test]
fn p5_sec_05_a_log_line_naming_a_directory_shows_its_control_characters_as_escapes() {
    let env = TestEnv::new();
    if !env.fixtures().unreadable_effective() {
        eprintln!("skipped: running as root, a mode-000 directory is readable");
        return;
    }
    let root = env.work_dir().join("tree");
    let closed = evil_path(&root);
    fs::create_dir_all(&closed).unwrap();
    fs::set_permissions(&closed, fs::Permissions::from_mode(0o000)).unwrap();
    let out = duscope(&env)
        .args(["scan", "--plain", "--debug", "--treemap", "never"])
        .arg(&root)
        .output()
        .unwrap();
    fs::set_permissions(&closed, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = clean(&out.stderr);
    assert!(text.contains(SHOWN), "{text}");
}

#[test]
fn p5_sec_05_quotes_and_backslashes_in_a_message_stay_as_they_are() {
    let env = TestEnv::new();
    let missing = env.work_dir().join("it's \"a\" \\name");
    let out = duscope(&env)
        .args(["explain", "--plain"])
        .arg(&missing)
        .output()
        .unwrap();
    assert!(!out.status.success());
    let text = clean(&out.stderr);
    assert!(text.contains("it's \"a\" \\name"), "{text}");
}
