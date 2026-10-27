//! P1-CLI-01: the command line: help, flags, exit codes, a closed pipe.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Stdio;

use assert_cmd::Command;
use duscope_testkit::TestEnv;

/// The golden file `name` under `duscope/tests/golden/`, compared with
/// `actual`; with `DUSCOPE_TEST_UPDATE_GOLDEN=1` it is rewritten instead.
fn golden(name: &str, actual: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name);
    if std::env::var_os("DUSCOPE_TEST_UPDATE_GOLDEN").is_some_and(|v| v == "1") {
        fs::write(&path, actual).unwrap();
        return;
    }
    let expected = fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert_eq!(
        actual, expected,
        "golden {name} differs; rerun with DUSCOPE_TEST_UPDATE_GOLDEN=1 to regenerate"
    );
}

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
        s("--skip-containers"),
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
    // A flag of a later phase.
    duscope(&env)
        .args(["scan", "--save"])
        .arg(&tree)
        .assert()
        .code(2);
}

#[test]
fn p1_cli_01_sigpipe_exits_zero() {
    let env = TestEnv::new();
    let mut child = env
        .cmd()
        .arg("--help")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut first = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut first)
        .unwrap();
    // The read end closes here, with the rest unread.
    let out = child.wait_with_output().unwrap();
    assert!(!first.is_empty());
    assert_eq!(out.status.code(), Some(0));
    assert!(
        out.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
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
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains(" · 1 files · 0 dirs · "), "{stdout}");
    assert!(out.stderr.is_empty());
    // A symbolic link to a directory, and one that leads nowhere, are
    // scanned without a crash.
    for link in ["to-dir", "dangling"] {
        let out = duscope(&env)
            .arg("scan")
            .arg(links.join(link))
            .output()
            .unwrap();
        assert_eq!(
            out.status.code(),
            Some(0),
            "{link}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.stderr.is_empty(), "{link}");
        assert_eq!(String::from_utf8_lossy(&out.stdout).lines().count(), 1);
    }
}
