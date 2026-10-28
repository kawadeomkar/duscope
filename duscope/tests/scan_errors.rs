//! P1-WALK-08: what a scan reports of unreadable directories, symbolic
//! links and its root, and how Ctrl-C ends it.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::process::{Command as StdCommand, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use assert_cmd::Command;
use duscope_testkit::TestEnv;

fn duscope(env: &TestEnv) -> Command {
    Command::from_std(env.cmd())
}

fn envelope(env: &TestEnv, args: &[&str]) -> serde_json::Value {
    let out = duscope(env).args(args).arg("--json").output().unwrap();
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

#[test]
fn p1_walk_08_an_unreadable_directory_counts_one_error() {
    let env = TestEnv::new();
    let fixtures = env.fixtures();
    if !fixtures.unreadable_effective() {
        eprintln!("skipped: running as root, the mode-000 directory is readable");
        return;
    }
    let root = fixtures.case_root("unreadable");
    let envelope = envelope(&env, &["scan", root.to_str().unwrap()]);
    assert_eq!(envelope["scan"]["error_count"], 1);
    let warning = &envelope["warnings"][0];
    assert_eq!(warning["code"], "partial_read");
    assert_eq!(warning["count"], 1);
    // The unreadable directory is listed, flagged, with no contents.
    let rows = envelope["entries"].as_array().unwrap();
    let closed = rows.iter().find(|r| r["name"] == "closed").unwrap();
    assert_eq!(closed["flags"], serde_json::json!(["unreadable"]));
    assert_eq!(closed["file_count"], 0);
}

#[test]
fn p1_walk_08_a_symlink_cycle_terminates_under_follow_symlinks() {
    let env = TestEnv::new();
    let root = env.fixtures().case_root("symlinks");
    let root = root.to_str().unwrap();
    // Not followed: every link is an entry of its own, and nothing fails.
    let plain = envelope(&env, &["scan", root]);
    assert_eq!(plain["scan"]["error_count"], 0);
    // Followed: the five links that loop or lead back up are errors, and
    // the scan ends.
    let started = Instant::now();
    let followed = envelope(&env, &["scan", root, "--follow-symlinks"]);
    assert!(started.elapsed() < Duration::from_secs(30));
    assert_eq!(followed["scan"]["error_count"], 5);
    // to-dir and to-file now count their targets' bytes.
    let total = |e: &serde_json::Value| e["scan"]["total_apparent_bytes"].as_u64().unwrap();
    assert!(total(&followed) > total(&plain));
}

#[test]
fn p1_walk_08_a_root_given_as_a_symlink_is_resolved() {
    let env = TestEnv::new();
    let target =
        std::fs::canonicalize(env.fixtures().case_root("symlinks").join("real-dir")).unwrap();
    let link = env.tmp_dir().join("root-link");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let envelope = envelope(&env, &["scan", link.to_str().unwrap()]);
    assert_eq!(envelope["scan"]["root"], target.to_str().unwrap());
    assert_eq!(envelope["scan"]["root_as_given"], link.to_str().unwrap());
    // The directory it leads to was scanned, not the link.
    assert_eq!(envelope["scan"]["file_count"], 1);
}

#[test]
fn p1_walk_08_a_root_link_that_leads_nowhere_fails() {
    let env = TestEnv::new();
    let links = env.fixtures().case_root("symlinks");
    // Dangling: not found (exit 3), as a missing path is.
    let out = duscope(&env)
        .args(["scan", "--json"])
        .arg(links.join("dangling"))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    let error: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(error["error"]["code"], "scan_not_found");
    // A link to itself: a runtime error, not a scan of the link.
    let out = duscope(&env)
        .args(["scan", "--json"])
        .arg(links.join("self"))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let error: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(error["error"]["code"], "io_error");
}

#[test]
fn p1_walk_08_sigint_exits_130_within_200_ms() {
    let env = TestEnv::new();
    let tree = env.fixtures().root();
    // macOS checks a newly built binary on its first launch, which can
    // take longer than the wait below: launch it once first.
    duscope(&env).arg("--version").assert().success();
    // One worker on the whole fixture tree takes about a second; the signal
    // comes once it is under way. A scan that finished and wrote its result
    // first is retried with a shorter wait, one the signal reached before it
    // started (killed by SIGINT itself) with a longer one.
    let mut wait = 300;
    for _ in 0..6 {
        let child = env
            .cmd()
            .args(["scan", "--threads", "1", "--json"])
            .arg(&tree)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        thread::sleep(Duration::from_millis(wait));
        let killed = StdCommand::new("kill")
            .args(["-INT", &child.id().to_string()])
            .status()
            .unwrap();
        let signalled = Instant::now();
        let out = child.wait_with_output().unwrap();
        let took = signalled.elapsed();
        match out.status.code() {
            // The scan finished, and wrote its result, before the signal.
            Some(_) if !out.stdout.is_empty() => {
                eprintln!("the scan finished within {wait} ms; trying sooner");
                wait /= 2;
                continue;
            }
            None => {
                eprintln!("the signal came before the scan began at {wait} ms; trying later");
                wait *= 2;
                continue;
            }
            Some(code) => assert_eq!(code, 130, "{}", String::from_utf8_lossy(&out.stderr)),
        }
        assert!(killed.success());
        assert!(took < Duration::from_millis(200), "exit took {took:?}");
        // One line on stderr, and nothing written to stdout.
        assert_eq!(
            String::from_utf8_lossy(&out.stderr),
            "duscope: interrupted\n"
        );
        assert!(out.stdout.is_empty());
        return;
    }
    panic!("no scan was under way when the signal came");
}

#[test]
fn p1_walk_08_an_ignored_sigint_is_left_ignored() {
    let env = TestEnv::new();
    let tree = env.fixtures().root();
    duscope(&env).arg("--version").assert().success();
    // A shell starts a background job with SIGINT ignored; the scan keeps
    // it so, and a Ctrl-C meant for the foreground does not end it.
    let child = env
        .command("sh")
        .args(["-c", "trap '' INT; exec \"$0\" \"$@\""])
        .arg(env.cmd().get_program())
        .args(["scan", "--threads", "1", "--json"])
        .arg(&tree)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    thread::sleep(Duration::from_millis(300));
    StdCommand::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let envelope: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(envelope["scan"]["file_count"].as_u64() > Some(0));
}
