//! P1-CLI-04: the manual page is clean under `mandoc -T lint`, and the
//! completion scripts load in bash, zsh and fish. Outside CI a missing
//! program skips its check; in CI it fails it.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fs;
use std::io::Write;
use std::process::{Command as StdCommand, Stdio};

use assert_cmd::Command;
use duscope_testkit::TestEnv;

fn duscope(env: &TestEnv) -> Command {
    Command::from_std(env.cmd())
}

/// Whether this is a CI run (`CI` set, and not `0` or `false`). CI installs
/// every program these tests drive (ci.yml installs zsh, fish and mandoc),
/// so there a missing one fails the test instead of skipping it, which a
/// passing test's captured output would hide.
fn in_ci() -> bool {
    std::env::var("CI").is_ok_and(|v| !matches!(v.as_str(), "" | "0" | "false"))
}

/// Skips a check that needs `program`, which is not installed here; fails
/// it in CI.
fn missing(program: &str) {
    assert!(
        !in_ci(),
        "{program} is not installed, and CI must run this check: install it in ci.yml"
    );
    eprintln!("skipped: {program} is not installed");
}

/// Whether `program` runs here.
fn available(env: &TestEnv, program: &str) -> bool {
    env.command(program)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[test]
fn p1_cli_04_the_page_is_clean_under_mandoc_lint() {
    let env = TestEnv::new();
    let page = duscope(&env).arg("man").output().unwrap();
    assert!(page.status.success());
    let text = String::from_utf8(page.stdout.clone()).unwrap();
    assert!(text.contains(".TH DUSCOPE 1 "));
    assert!(text.contains(".SH ENVIRONMENT"));
    // mandoc has no --version; `-T lint` on empty input checks it runs.
    let mandoc = env
        .command("mandoc")
        .arg("-T")
        .arg("lint")
        .stdin(Stdio::null())
        .output();
    if mandoc.is_err() {
        missing("mandoc");
        return;
    }
    let mut lint = StdCommand::new("mandoc")
        .args(["-T", "lint"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    lint.stdin.take().unwrap().write_all(&page.stdout).unwrap();
    let out = lint.wait_with_output().unwrap();
    let messages = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success() && messages.is_empty(), "{messages}");
}

#[test]
fn p1_cli_04_completions_load_in_bash_zsh_and_fish() {
    let env = TestEnv::new();
    let mut loaded = 0;
    for (shell, file, check) in [
        (
            "bash",
            "duscope.bash",
            "source \"$1\" && complete -p duscope",
        ),
        (
            "zsh",
            "_duscope",
            "autoload -Uz compinit && compinit -u -d \"$1.dump\" && source \"$1\" && (( $+functions[_duscope] ))",
        ),
        (
            "fish",
            "duscope.fish",
            "source $argv[1]; and complete -C 'duscope --fol'",
        ),
    ] {
        if !available(&env, shell) {
            missing(shell);
            continue;
        }
        let script = duscope(&env).args(["completions", shell]).output().unwrap();
        assert!(script.status.success(), "{shell}");
        let path = env.tmp_dir().join(file);
        fs::write(&path, &script.stdout).unwrap();
        let mut command = env.command(shell);
        if shell == "fish" {
            command.args(["-c", check]).arg(&path);
        } else {
            command.args(["-c", check, shell]).arg(&path);
        }
        let out = command.output().unwrap();
        assert!(
            out.status.success(),
            "{shell}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        if shell == "fish" {
            assert!(String::from_utf8_lossy(&out.stdout).contains("--follow-symlinks"));
        }
        loaded += 1;
    }
    assert!(loaded >= 1, "no shell to load the completions in");
    // The other two shells are generated as well.
    for shell in ["elvish", "powershell"] {
        let script = duscope(&env).args(["completions", shell]).output().unwrap();
        assert!(
            script.status.success() && !script.stdout.is_empty(),
            "{shell}"
        );
    }
}
