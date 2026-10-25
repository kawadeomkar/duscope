//! P0-DOC-01: `scripts/changelog.py`, the one parser of the changelog fragments.
//!
//! `check` validates `changelog.d/<ID>.md` fragments with the rule of the
//! `changelog` CI job; `assemble` merges them into a dated section of
//! `CHANGELOG.md` at release and deletes them. These tests run the script on
//! the fragments under `tests/fixtures/changelog/`, laid out as a repository in
//! a scratch directory under `CARGO_TARGET_TMPDIR`. They read repository files
//! and run Python; they never run the duscope binary.
//!
//! The interpreter is `$PYTHON` when it is set, else `/usr/bin/python3` when it
//! exists (the system Python on macOS and Linux), else `python3` from `PATH`.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

/// The release date every assembly in these tests uses.
const DATE: &str = "2026-11-02";

/// Each fragment under `malformed/`, with the line and the message `check`
/// reports for it.
const MALFORMED: &[(&str, usize, &str)] = &[
    (
        "unknown-section.md",
        1,
        "unknown section '### Notes': use one of Added, Changed, Deprecated, Removed, Fixed, Security",
    ),
    (
        "version-heading.md",
        1,
        "only '### <Section>' headings belong in a fragment",
    ),
    (
        "deeper-heading.md",
        5,
        "only '### <Section>' headings belong in a fragment",
    ),
    (
        "text-before-heading.md",
        1,
        "text before the first '### <Section>' heading",
    ),
    (
        "duplicate-section.md",
        5,
        "section '### Added' appears twice",
    ),
    (
        "empty-section.md",
        1,
        "section '### Added' has no '- ' items",
    ),
    (
        "star-bullet.md",
        3,
        "expected a '- ' list item or an indented continuation line",
    ),
    (
        "orphan-continuation.md",
        3,
        "expected a '- ' list item or an indented continuation line",
    ),
    ("no-heading.md", 1, "no '### <Section>' heading"),
];

/// A fragment that is not valid UTF-8 (`café` in Latin-1), written by the tests
/// rather than committed.
const NOT_UTF8: &[u8] = b"### Added\n\n- caf\xe9\n";

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn script() -> PathBuf {
    repo_root().join("scripts/changelog.py")
}

fn changelog_fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/changelog")
        .join(name)
}

fn python() -> OsString {
    if let Some(python) = std::env::var_os("PYTHON").filter(|p| !p.is_empty()) {
        return python;
    }
    let system = Path::new("/usr/bin/python3");
    if system.is_file() {
        system.into()
    } else {
        "python3".into()
    }
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

/// The markdown files of a directory, by name: `*.md`, matched case-sensitively
/// as the script and the CI job match fragments.
fn markdown_files(dir: &Path) -> BTreeSet<String> {
    fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("cannot list {}: {e}", dir.display()))
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| Path::new(name).extension().is_some_and(|e| e == "md"))
        .collect()
}

/// A directory under `CARGO_TARGET_TMPDIR`, removed when dropped.
#[derive(Debug)]
struct Scratch(PathBuf);

impl Scratch {
    fn new(test: &str) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("p0_doc_01-{test}-{}-{n}", std::process::id()));
        // A directory left by a killed run of the same pid would hold stale files.
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    /// Lays a fixture out as a repository root: its `CHANGELOG.md`, and its
    /// `fragments/` as `changelog.d/`.
    fn lay_out(&self, fixture: &str) {
        let source = changelog_fixture(fixture);
        fs::copy(source.join("CHANGELOG.md"), self.0.join("CHANGELOG.md")).unwrap();
        let fragments = self.0.join("changelog.d");
        fs::create_dir(&fragments).unwrap();
        for name in markdown_files(&source.join("fragments")) {
            fs::copy(source.join("fragments").join(&name), fragments.join(&name)).unwrap();
        }
    }

    fn changelog(&self) -> String {
        read(&self.0.join("CHANGELOG.md"))
    }

    fn fragments(&self) -> BTreeSet<String> {
        markdown_files(&self.0.join("changelog.d"))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// `python <program> <args>` in `cwd`; with `github`, as inside a GitHub
/// Actions step, which switches the script's errors to workflow annotations.
fn python_run(program: &Path, cwd: &Path, args: &[&str], github: bool) -> Output {
    let python = python();
    let mut command = Command::new(&python);
    command.arg(program).args(args).current_dir(cwd);
    if github {
        command.env("GITHUB_ACTIONS", "true");
    } else {
        command.env_remove("GITHUB_ACTIONS");
    }
    command.output().unwrap_or_else(|e| {
        panic!(
            "cannot run {}: {e}; set PYTHON to a Python 3.9 or later interpreter",
            python.to_string_lossy()
        )
    })
}

/// `scripts/changelog.py <args>` in `cwd`, outside GitHub Actions.
fn changelog_py(cwd: &Path, args: &[&str]) -> Output {
    python_run(&script(), cwd, args, false)
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn describe(out: &Output) -> String {
    format!(
        "exit status {:?}\n--- stdout\n{}--- stderr\n{}",
        out.status.code(),
        stdout(out),
        stderr(out)
    )
}

fn set(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|&name| name.to_owned()).collect()
}

#[test]
fn p0_doc_01_assembly_merges_sections_in_keep_a_changelog_order() {
    let scratch = Scratch::new("merge");
    scratch.lay_out("merge");

    let out = changelog_py(scratch.path(), &["assemble", "0.1.0", "--date", DATE]);

    assert!(out.status.success(), "{}", describe(&out));
    assert_eq!(
        scratch.changelog(),
        read(&changelog_fixture("merge/expected.md")),
        "the assembled CHANGELOG.md differs from merge/expected.md"
    );
    assert_eq!(
        scratch.fragments(),
        set(&["README.md"]),
        "assembly deletes the fragments it consumed and keeps changelog.d/README.md"
    );
}

#[test]
fn p0_doc_01_assembly_dates_the_pending_section_and_keeps_its_notes() {
    let scratch = Scratch::new("pending");
    scratch.lay_out("pending");

    // The tag form of the version is accepted too.
    let out = changelog_py(scratch.path(), &["assemble", "v0.0.1", "--date", DATE]);

    assert!(out.status.success(), "{}", describe(&out));
    assert_eq!(
        scratch.changelog(),
        read(&changelog_fixture("pending/expected.md")),
        "the assembled CHANGELOG.md differs from pending/expected.md"
    );
    assert!(scratch.fragments().is_empty(), "the fragments are deleted");
}

#[test]
fn p0_doc_01_assembly_fails_without_fragments() {
    let scratch = Scratch::new("empty");
    scratch.lay_out("merge");
    for name in scratch.fragments() {
        if name != "README.md" {
            fs::remove_file(scratch.path().join("changelog.d").join(name)).unwrap();
        }
    }
    let before = scratch.changelog();

    let out = changelog_py(scratch.path(), &["assemble", "0.1.0", "--date", DATE]);
    assert!(
        !out.status.success(),
        "a release with only changelog.d/README.md must fail\n{}",
        describe(&out)
    );
    assert!(
        stderr(&out).contains("no fragments"),
        "the failure says why\n{}",
        describe(&out)
    );
    assert_eq!(scratch.changelog(), before, "CHANGELOG.md is unchanged");
    assert_eq!(scratch.fragments(), set(&["README.md"]));

    fs::remove_dir_all(scratch.path().join("changelog.d")).unwrap();
    let out = changelog_py(scratch.path(), &["assemble", "0.1.0", "--date", DATE]);
    assert!(
        !out.status.success(),
        "a release without changelog.d/ must fail\n{}",
        describe(&out)
    );
    assert_eq!(scratch.changelog(), before, "CHANGELOG.md is unchanged");
}

#[test]
fn p0_doc_01_assembly_changes_nothing_when_a_fragment_is_malformed() {
    let scratch = Scratch::new("malformed");
    scratch.lay_out("merge");
    fs::copy(
        changelog_fixture("malformed/unknown-section.md"),
        scratch.path().join("changelog.d/P1-DDD-04.md"),
    )
    .unwrap();
    let before = scratch.changelog();
    let fragments = scratch.fragments();

    let out = changelog_py(scratch.path(), &["assemble", "0.1.0", "--date", DATE]);

    assert!(!out.status.success(), "{}", describe(&out));
    assert!(
        stderr(&out).contains("changelog.d/P1-DDD-04.md:1: unknown section '### Notes'"),
        "the error names the fragment and the line\n{}",
        describe(&out)
    );
    assert_eq!(scratch.changelog(), before, "CHANGELOG.md is unchanged");
    assert_eq!(scratch.fragments(), fragments, "no fragment is deleted");
}

#[test]
fn p0_doc_01_assembly_refuses_a_released_version() {
    let scratch = Scratch::new("released");
    scratch.lay_out("merge");
    let before = scratch.changelog();
    let fragments = scratch.fragments();

    let out = changelog_py(scratch.path(), &["assemble", "0.0.1", "--date", DATE]);

    assert!(!out.status.success(), "{}", describe(&out));
    assert!(
        stderr(&out).contains("already has a section for 0.0.1"),
        "{}",
        describe(&out)
    );
    assert_eq!(scratch.changelog(), before, "CHANGELOG.md is unchanged");
    assert_eq!(scratch.fragments(), fragments, "no fragment is deleted");
}

/// Every fragment committed in `changelog.d/` (`P0-REPO-01.md` among them until
/// the v0.0.1 release consumes it) and every valid fixture fragment parses.
#[test]
fn p0_doc_01_check_accepts_the_committed_fragments() {
    let root = repo_root();
    let mut fragments: Vec<String> = markdown_files(&root.join("changelog.d"))
        .into_iter()
        .filter(|name| name != "README.md")
        .map(|name| format!("changelog.d/{name}"))
        .collect();
    for fixture in ["merge", "pending"] {
        for name in markdown_files(&changelog_fixture(fixture).join("fragments")) {
            if name != "README.md" {
                fragments.push(format!(
                    "duscope/tests/fixtures/changelog/{fixture}/fragments/{name}"
                ));
            }
        }
    }
    let args: Vec<&str> = ["check"]
        .into_iter()
        .chain(fragments.iter().map(String::as_str))
        .collect();

    let out = changelog_py(&root, &args);

    assert!(out.status.success(), "{}", describe(&out));
    let printed = stdout(&out);
    for fragment in &fragments {
        let parses = format!("{fragment}: parses");
        assert!(
            printed.lines().any(|line| line == parses),
            "check does not report that {fragment} parses\n{}",
            describe(&out)
        );
    }
}

#[test]
fn p0_doc_01_check_rejects_malformed_fragments() {
    let dir = changelog_fixture("malformed");
    let listed: BTreeSet<String> = MALFORMED
        .iter()
        .map(|&(name, _, _)| name.to_owned())
        .collect();
    assert_eq!(
        markdown_files(&dir),
        listed,
        "every fixture under malformed/ needs its expected error in MALFORMED"
    );

    for &(name, line, message) in MALFORMED {
        let out = changelog_py(&dir, &["check", name]);
        assert_eq!(out.status.code(), Some(1), "{name}\n{}", describe(&out));
        assert!(
            stderr(&out).contains(&format!("{name}:{line}: {message}")),
            "check must report `{name}:{line}: {message}`\n{}",
            describe(&out)
        );
        assert!(
            !stdout(&out).contains("parses"),
            "{name} must not parse\n{}",
            describe(&out)
        );
    }

    let scratch = Scratch::new("not-utf8");
    fs::write(scratch.path().join("P1-EEE-05.md"), NOT_UTF8).unwrap();
    let out = changelog_py(scratch.path(), &["check", "P1-EEE-05.md"]);
    assert_eq!(out.status.code(), Some(1), "{}", describe(&out));
    assert!(
        stderr(&out).contains("P1-EEE-05.md:1: cannot read the fragment as UTF-8"),
        "{}",
        describe(&out)
    );
}

/// The bash of the check step of ci.yml's `changelog` job, unindented.
fn ci_changelog_step() -> String {
    let workflow = read(&repo_root().join(".github/workflows/ci.yml"));
    let lines: Vec<&str> = workflow.lines().collect();
    let job = lines
        .iter()
        .position(|line| *line == "  changelog:")
        .expect("ci.yml has a changelog job");
    let run = job
        + 1
        + lines[job + 1..]
            .iter()
            .position(|line| line.trim() == "run: |")
            .expect("the changelog job has a `run: |` step");
    let indent = lines[run].len() - lines[run].trim_start().len() + 2;
    let body: Vec<&str> = lines[run + 1..]
        .iter()
        .take_while(|line| line.trim().is_empty() || line.len() - line.trim_start().len() >= indent)
        .map(|line| line.get(indent..).unwrap_or(""))
        .collect();
    body.join("\n") + "\n"
}

/// A bash of version 4 or later, which the step's `mapfile` needs (the
/// job runs on ubuntu; macOS ships bash 3.2 as /bin/bash).
fn bash4() -> Option<OsString> {
    ["bash", "/opt/homebrew/bin/bash", "/usr/local/bin/bash"]
        .into_iter()
        .find(|bash| {
            Command::new(bash)
                .args(["-c", "(( BASH_VERSINFO[0] >= 4 ))"])
                .status()
                .is_ok_and(|status| status.success())
        })
        .map(OsString::from)
}

/// Runs the changelog job's step as GitHub would on a pull request titled
/// `title`, labelled `labels`, that adds or changes `files` (path, content):
/// in a repository whose HEAD merges the pull request into its base, with
/// `gh` answering the pull-request query.
fn run_changelog_job(
    bash: &OsString,
    title: &str,
    labels: &[&str],
    files: &[(&str, &str)],
) -> Output {
    let scratch = Scratch::new("ci-job");
    let repo = scratch.path().join("repo");
    let bin = scratch.path().join("bin");
    fs::create_dir_all(repo.join("changelog.d")).unwrap();
    fs::create_dir_all(repo.join("scripts")).unwrap();
    fs::create_dir_all(&bin).unwrap();
    fs::copy(script(), repo.join("scripts/changelog.py")).unwrap();
    fs::copy(
        repo_root().join("changelog.d/README.md"),
        repo.join("changelog.d/README.md"),
    )
    .unwrap();
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "init.defaultBranch=main",
            ])
            .args(args)
            .current_dir(&repo)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", describe(&out));
    };
    git(&["init", "-q"]);
    git(&["add", "-A"]);
    git(&["commit", "-qm", "base"]);
    git(&["checkout", "-qb", "pr"]);
    for (path, content) in files {
        fs::write(repo.join(path), content).unwrap();
    }
    git(&["add", "-A"]);
    git(&["commit", "-qm", "pull request", "--allow-empty"]);
    git(&["checkout", "-q", "main"]);
    git(&["merge", "-q", "--no-ff", "-m", "merge", "pr"]);

    let labels: Vec<String> = labels
        .iter()
        .map(|l| format!("{{\"name\":\"{l}\"}}"))
        .collect();
    let pr = scratch.path().join("pr.json");
    fs::write(
        &pr,
        format!("{{\"title\":{title:?},\"labels\":[{}]}}", labels.join(",")),
    )
    .unwrap();
    let shim = |name: &str, body: String| {
        let path = bin.join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    };
    shim("gh", format!("cat '{}'", pr.display()));
    shim(
        "python3",
        format!("exec '{}' \"$@\"", python().to_string_lossy()),
    );
    let path = std::env::join_paths(
        std::iter::once(bin.clone())
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    Command::new(bash)
        .args(["--noprofile", "--norc", "-eo", "pipefail", "-c"])
        .arg(ci_changelog_step())
        .current_dir(&repo)
        .env("PATH", path)
        .env("GITHUB_ACTIONS", "true")
        .env("GITHUB_REPOSITORY", "owner/repo")
        .env("PR_NUMBER", "1")
        .env("GH_TOKEN", "unused")
        .output()
        .unwrap()
}

/// A pull request's title, labels and changed files (path, content), with
/// the exit status and a line of output the changelog job must give it.
type JobCase<'a> = (
    &'a str,
    &'a [&'a str],
    &'a [(&'a str, &'a str)],
    i32,
    &'a str,
);

/// ci.yml's `changelog` job, run for real: a pull request needs the fragment
/// its title names or the `no-changelog` label, and every fragment it adds
/// or changes must parse, label or not.
#[test]
fn p0_doc_01_ci_changelog_job_requires_a_fragment_or_the_label() {
    let Some(bash) = bash4() else {
        eprintln!("p0_doc_01: no bash 4 or later here, so the changelog job was not run");
        return;
    };
    let valid = "### Added\n\n- A thing.\n";
    let malformed = "### Nonsense\n\n- A thing.\n";
    let title = "P0-XYZ-01: Do a thing";
    let cases: &[JobCase] = &[
        (title, &[], &[], 1, "add changelog.d/P0-XYZ-01.md"),
        (
            title,
            &["no-changelog"],
            &[],
            0,
            "the no-changelog label is set",
        ),
        (
            title,
            &[],
            &[("changelog.d/P0-XYZ-01.md", valid)],
            0,
            "changelog: changelog.d/P0-XYZ-01.md found",
        ),
        (
            title,
            &["no-changelog"],
            &[("changelog.d/P0-XYZ-01.md", malformed)],
            1,
            "::error file=changelog.d/P0-XYZ-01.md",
        ),
        (
            title,
            &[],
            &[("changelog.d/P0-XYZ-01.md", malformed)],
            1,
            "::error file=changelog.d/P0-XYZ-01.md",
        ),
        (
            "Do a thing",
            &[],
            &[("changelog.d/P0-XYZ-01.md", valid)],
            1,
            "does not start with '<ID>: '",
        ),
        (
            title,
            &[],
            &[("changelog.d/P0-ABC-02.md", valid)],
            1,
            "add changelog.d/P0-XYZ-01.md",
        ),
        (
            title,
            &[],
            &[("changelog.d/README.md", "# Edited\n")],
            1,
            "add changelog.d/P0-XYZ-01.md",
        ),
    ];
    for (title, labels, files, code, says) in cases {
        let out = run_changelog_job(&bash, title, labels, files);
        assert_eq!(
            out.status.code(),
            Some(*code),
            "{title:?} {labels:?} {files:?}\n{}",
            describe(&out)
        );
        assert!(
            stdout(&out).contains(says),
            "{title:?} {labels:?} {files:?}: expected {says:?}\n{}",
            describe(&out)
        );
    }
}
