//! P1-CLI-01: the command line: help, flags, exit codes, a closed pipe.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use assert_cmd::Command;
use duscope_testkit::{TestEnv, golden};

fn duscope(env: &TestEnv) -> Command {
    Command::from_std(env.cmd())
}

#[test]
fn p1_cli_01_help_matches_its_golden() {
    let env = TestEnv::new();
    let out = duscope(&env).arg("--help").output().unwrap();
    assert!(out.status.success());
    assert!(out.stderr.is_empty());
    golden(
        "help_text_duscope.snap",
        &String::from_utf8(out.stdout).unwrap(),
    );
}

/// Every flag of the proposal's verbs table and the CLI-01 brief, for each
/// phase-1 verb, with a value where it takes one.
fn flags(dir: &Path) -> Vec<(&'static str, Vec<String>)> {
    let classes = dir.join("classes.toml");
    let patterns = dir.join("patterns");
    let ignore = dir.join("gitignore");
    fs::write(&classes, "[ext]\nrs = \"code\"\n").unwrap();
    fs::write(&patterns, "*.tmp\n").unwrap();
    fs::write(&ignore, "*.log\n!keep.log\n").unwrap();
    let s = |v: &str| v.to_owned();
    let p = |v: &PathBuf| v.display().to_string();
    let walk = vec![
        s("--threads=2"),
        s("-x"),
        s("--include-network"),
        s("--follow-symlinks"),
        s("--size=apparent"),
        s("--attrs=full"),
        s("--clones"),
        s("--no-clones"),
        format!("--exclude-from={}", p(&patterns)),
        format!("--ignore-from={}", p(&ignore)),
        s("--exclude-caches"),
        s("--provider=fallback"),
        s("--test-now=2037-01-01T00:00:00Z"),
    ];
    let query = vec![
        s("--limit=5"),
        s("--max-depth=2"),
        s("--min-depth=1"),
        s("--min-size=1K"),
        s("--max-size=1.5G"),
        s("--older-than=30d"),
        s("--newer-than=1y"),
        s("--since=2020-01-01"),
        s("--until=2036-12-31T00:00:00Z"),
        s("--time=ctime"),
        s("--type=file,dir"),
        s("--sort=mtime"),
        s("-r"),
        s("--glob=**/*.rs"),
        s("--regex=.*"),
        s("--invert"),
        s("--exclude=*.tmp"),
        s("--strict"),
        s("--apparent-size"),
    ];
    let with = |base: &[String], extra: &[String]| {
        let mut all = base.to_vec();
        all.extend_from_slice(extra);
        // --apparent-size and --size conflict: keep one of them.
        all.retain(|f| f != "--size=apparent" || !extra.contains(&s("--apparent-size")));
        all
    };
    vec![
        (
            "scan",
            with(
                &walk,
                &[
                    s("--exclude=*.tmp"),
                    s("--depth=3"),
                    s("--treemap=never"),
                    s("--color-by=folder"),
                    s("--theme=mocha"),
                    s("--pool-files"),
                ],
            ),
        ),
        (
            "top",
            with(
                &walk,
                &[query.clone(), vec![s("--files"), s("--no-cut-through")]].concat(),
            ),
        ),
        (
            "tree",
            with(&walk, &[query.clone(), vec![s("--min-pct=1.5")]].concat()),
        ),
        (
            "by-type",
            with(
                &walk,
                &[
                    query.clone(),
                    vec![format!("--classes={}", p(&classes)), s("--examples=3")],
                ]
                .concat(),
            ),
        ),
        ("stale", with(&walk, &[query, vec![s("--atime")]].concat())),
    ]
}

#[test]
fn p1_cli_01_every_phase_1_flag_is_accepted() {
    let env = TestEnv::new();
    let tree = env.fixtures().case_root("empty");
    for (verb, flags) in flags(&env.tmp_dir()) {
        let out = duscope(&env)
            .arg(verb)
            .args(&flags)
            .arg(&tree)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{verb} {flags:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    // The long forms and aliases the list above leaves out, each alone.
    for (verb, flag) in [
        ("scan", "--one-file-system"),
        ("top", "--depth=1"),
        ("scan", "--max-depth=1"),
    ] {
        let out = duscope(&env)
            .args([verb, flag])
            .arg(&tree)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{verb} {flag}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    // Without a verb, duscope scans; with scan's flags too.
    let out = duscope(&env).arg("-x").arg(&tree).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn p1_cli_01_exit_codes() {
    let env = TestEnv::new();
    let fixtures = env.fixtures();
    let tree = fixtures.case_root("empty");
    duscope(&env).arg("scan").arg(&tree).assert().code(0);
    // A usage error.
    duscope(&env)
        .args(["scan", "--no-such-flag"])
        .assert()
        .code(2);
    duscope(&env)
        .args(["scan", "--size=sideways"])
        .assert()
        .code(2);
    // A path that does not exist.
    let missing = duscope(&env)
        .args(["scan", "/nonexistent/duscope"])
        .assert()
        .code(3);
    let stderr = String::from_utf8_lossy(&missing.get_output().stderr).into_owned();
    assert!(
        stderr.starts_with("duscope: /nonexistent/duscope does not exist\nhint: "),
        "{stderr}"
    );
    // A root that cannot be read.
    if fixtures.unreadable_effective() {
        let closed = fixtures.case_root("unreadable").join("closed");
        duscope(&env).arg("top").arg(&closed).assert().code(4);
    }
    // A runtime error: a root path longer than PATH_MAX (ENAMETOOLONG).
    let long = format!("/{}", "a".repeat(5000));
    let failed = duscope(&env).args(["scan", &long]).assert().code(1);
    let stderr = String::from_utf8_lossy(&failed.get_output().stderr).into_owned();
    assert!(stderr.starts_with("duscope: cannot read /aaa"), "{stderr}");
    // Flags of a later phase: hidden, and refused before the scan.
    for args in [
        ["scan", "--interactive"],
        ["scan", "--skip-containers"],
        ["top", "--skip-containers"],
    ] {
        let refused = duscope(&env)
            .args(args)
            .arg("--json")
            .arg(&tree)
            .assert()
            .code(2);
        let envelope: serde_json::Value =
            serde_json::from_slice(&refused.get_output().stdout).unwrap();
        assert_eq!(envelope["error"]["code"], "not_available", "{args:?}");
    }
}

#[test]
fn p1_cli_01_sigpipe_exits_zero() {
    // The read end closes before the verb writes its rows: the write meets
    // EPIPE, which the binary ends quietly with success. (`--help` would
    // not do: clap prints it, and it fits in the pipe's buffer.) The
    // footer still reaches stderr, and nothing else does.
    let env = TestEnv::new();
    let mut child = env
        .cmd()
        .args(["top", "--format", "ndjson"])
        .arg(env.fixtures().path("home"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    let out = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert!(stderr.lines().count() <= 1, "{stderr}");
    assert!(!stderr.contains("duscope:"), "{stderr}");
}

#[test]
fn p1_cli_01_a_root_that_is_not_a_directory() {
    let env = TestEnv::new();
    let links = env.fixtures().case_root("symlinks");
    // A regular file is one file of its own size.
    let out = duscope(&env)
        .arg("scan")
        .arg(links.join("target.txt"))
        .output()
        .unwrap();
    // The footer, the scan verb's one line, is human output on stderr.
    let footer = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{footer}");
    assert!(footer.contains(" · 1 files · 0 dirs · "), "{footer}");
    assert!(out.stdout.is_empty());
    // A symbolic link to a directory is scanned without a crash. (One that
    // leads nowhere is an error: p1_walk_08_a_root_link_that_leads_nowhere_fails.)
    let out = duscope(&env)
        .arg("scan")
        .arg(links.join("to-dir"))
        .output()
        .unwrap();
    let footer = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{footer}");
    assert_eq!(footer.lines().count(), 1, "{footer}");
    assert!(footer.contains(" allocated · "), "{footer}");
}

#[test]
fn p1_cli_01_thread_counts() {
    let env = TestEnv::new();
    let tree = env.fixtures().case_root("empty");
    // An exported but empty DUSCOPE_THREADS is the same as an unset one.
    duscope(&env)
        .env("DUSCOPE_THREADS", "")
        .arg("scan")
        .arg(&tree)
        .assert()
        .code(0);
    let given = duscope(&env)
        .env("DUSCOPE_THREADS", "3")
        .arg("scan")
        .arg(&tree)
        .assert()
        .code(0);
    let footer = String::from_utf8_lossy(&given.get_output().stderr).into_owned();
    assert!(footer.ends_with(", 3 threads\n"), "{footer}");
    // A value that is not a count names where it came from.
    let bad = duscope(&env)
        .env("DUSCOPE_THREADS", "many")
        .arg("scan")
        .arg(&tree)
        .assert()
        .code(2);
    let stderr = String::from_utf8_lossy(&bad.get_output().stderr).into_owned();
    assert!(stderr.contains("DUSCOPE_THREADS"), "{stderr}");
    // More threads than any machine needs is a usage error, not a scan
    // that starts thousands of threads.
    duscope(&env)
        .args(["scan", "--threads=100000"])
        .arg(&tree)
        .assert()
        .code(2);
}
