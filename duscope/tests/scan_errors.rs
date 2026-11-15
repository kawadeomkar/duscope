//! P1-WALK-08: what a scan reports of unreadable directories, symbolic
//! links and its root, and how Ctrl-C ends it.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::io::Read;
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

/// What a child wrote to one of its pipes, read to the end on a thread of
/// its own: the bytes, when `awaited` had come through (if it did), and
/// when the pipe ended.
struct Output {
    bytes: Vec<u8>,
    came: Option<Instant>,
    ended: Instant,
}

fn read_on_thread(
    mut pipe: impl Read + Send + 'static,
    awaited: Option<&'static [u8]>,
) -> thread::JoinHandle<Output> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let mut came = None;
        let mut chunk = [0; 4096];
        loop {
            let read = pipe.read(&mut chunk).unwrap();
            if read == 0 {
                break;
            }
            bytes.extend_from_slice(&chunk[..read]);
            if came.is_none()
                && awaited.is_some_and(|text| bytes.windows(text.len()).any(|w| w == text))
            {
                came = Some(Instant::now());
            }
        }
        Output {
            bytes,
            came,
            ended: Instant::now(),
        }
    })
}

#[test]
fn p1_walk_08_sigint_exits_130_within_200_ms() {
    let env = TestEnv::new();
    let tree = env.fixtures().root();
    // macOS checks a newly built binary on its first launch, which can
    // take longer than the wait below: launch it once first.
    duscope(&env).arg("--version").assert().success();
    let bound = Duration::from_millis(200);
    // One worker on the whole fixture tree takes about a second; the signal
    // comes once it is under way. A scan that finished and wrote its result
    // first is retried with a shorter wait, one the signal reached before it
    // started (killed by SIGINT itself) with a longer one.
    //
    // The exit is timed by waitpid(2), not by the end of the child's
    // output, which a command another test starts at that moment can hold
    // open: on macOS the standard library makes a new pipe close-on-exec
    // only after creating it. The watcher reports and exits at once, but a
    // process ends only once each of its threads has left the kernel, and
    // on a loaded disk the worker's read of a directory can hold the exit
    // back past the bound. A try whose report came in time and whose exit
    // did not is made again; a late report fails at once, and so does a
    // third late exit.
    let mut wait = 300;
    let mut late = Vec::new();
    for _ in 0..9 {
        let mut child = env
            .cmd()
            .args(["scan", "--threads", "1", "--json"])
            .arg(&tree)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = read_on_thread(child.stdout.take().unwrap(), None);
        let stderr = read_on_thread(
            child.stderr.take().unwrap(),
            Some(b"duscope: interrupted\n"),
        );
        thread::sleep(Duration::from_millis(wait));
        let killed = StdCommand::new("kill")
            .args(["-INT", &child.id().to_string()])
            .status()
            .unwrap();
        let signalled = Instant::now();
        let status = child.wait().unwrap();
        let exited = signalled.elapsed();
        let (stdout, stderr) = (stdout.join().unwrap(), stderr.join().unwrap());
        match status.code() {
            // The scan finished, and wrote its result, before the signal.
            Some(_) if !stdout.bytes.is_empty() => {
                eprintln!("the scan finished within {wait} ms; trying sooner");
                wait /= 2;
                continue;
            }
            None => {
                eprintln!("the signal came before the scan began at {wait} ms; trying later");
                wait *= 2;
                continue;
            }
            Some(code) => assert_eq!(code, 130, "{}", String::from_utf8_lossy(&stderr.bytes)),
        }
        assert!(killed.success());
        // One line on stderr, and nothing written to stdout.
        assert_eq!(
            String::from_utf8_lossy(&stderr.bytes),
            "duscope: interrupted\n"
        );
        assert!(stdout.bytes.is_empty());
        let reported = stderr
            .came
            .map(|at| at.saturating_duration_since(signalled));
        let ended = stdout
            .ended
            .max(stderr.ended)
            .saturating_duration_since(signalled);
        let timings = format!(
            "after the signal, reported {}, exited {exited:?}, output ended {ended:?}",
            reported.map_or("never".to_owned(), |reported| format!("{reported:?}"))
        );
        assert!(
            reported.is_some_and(|reported| reported < bound),
            "the interrupt was reported late: {timings}"
        );
        if exited < bound {
            return;
        }
        late.push(timings);
        assert!(
            late.len() < 3,
            "the exit took over {bound:?} three times: {late:#?}"
        );
        eprintln!(
            "the exit took over {bound:?}, the report did not; trying again: {}",
            late[late.len() - 1]
        );
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
