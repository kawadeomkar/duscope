//! P1-REL-01: docs/RELEASING.md, the checklist every tag follows, has its
//! sections, and its secrets table names every secret a workflow reads.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_owned()
}

#[test]
fn p1_rel_01_releasing_has_the_checklist_and_names_every_secret() {
    let doc = fs::read_to_string(root().join("docs/RELEASING.md")).unwrap();
    for heading in [
        "## Release checklist",
        "### Patch releases",
        "## Secrets",
        "## Pulling a release",
    ] {
        assert!(doc.lines().any(|l| l == heading), "no `{heading}`");
    }
    assert!(doc.contains(
        "| Name | Where used | Scope | Where set | Created | Expires | Rotation owner |"
    ));
    // Every `secrets.NAME` the workflows read, but the run's own token.
    let mut used = BTreeSet::new();
    for entry in fs::read_dir(root().join(".github/workflows")).unwrap() {
        let text = fs::read_to_string(entry.unwrap().path()).unwrap();
        for (at, _) in text.match_indices("secrets.") {
            let name: String = text[at + "secrets.".len()..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if name != "GITHUB_TOKEN" {
                used.insert(name);
            }
        }
    }
    let secrets: &str = doc.split("## Secrets").nth(1).unwrap();
    for name in &used {
        assert!(
            secrets.contains(&format!("`{name}`")),
            "{name} is read by a workflow but has no row in docs/RELEASING.md"
        );
    }
}

fn read(relative: &str) -> String {
    fs::read_to_string(root().join(relative)).unwrap()
}

/// The text with every run of whitespace made one space, so a restatement
/// is found however its file wraps it.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// P0-CI-03: the size ceiling is 10 MB (`size_budget` in the justfile) and the
/// cold build's 120 s (`BUILD_TIME_BUDGET_SECS` in ci.yml), each one figure
/// with no ratchet below it, and every file that restates them says the same.
#[test]
fn p0_ci_03_the_ceilings_are_10_mb_and_120_s_wherever_they_are_stated() {
    let value = |text: &str, prefix: &str| -> u64 {
        let line = text
            .lines()
            .find_map(|l| l.strip_prefix(prefix))
            .unwrap_or_else(|| panic!("no `{prefix}`"));
        line.trim().trim_matches('"').parse().unwrap()
    };
    let bytes = value(&read("justfile"), "size_budget := ");
    let secs = value(
        &read(".github/workflows/ci.yml"),
        "  BUILD_TIME_BUDGET_SECS: ",
    );
    assert_eq!(bytes, 10_000_000, "the size ceiling is 10 MB");
    assert_eq!(secs, 120, "the build ceiling is 120 s");
    let (mb, s) = (format!("{} MB", bytes / 1_000_000), format!("{secs} s"));
    for (file, statements) in [
        (
            "CLAUDE.md",
            vec![format!("(<= {mb} stripped)"), format!("(<= {s})")],
        ),
        ("CONTRIBUTING.md", vec![format!("(`size_budget`, {mb})")]),
        (
            "DEPENDENCIES.md",
            vec![format!("at {mb} or less"), format!("at {s} or less")],
        ),
        (
            "docs/RELEASING.md",
            vec![
                "against size_budget in the justfile".to_owned(),
                "`BUILD_TIME_BUDGET_SECS` in `ci.yml`".to_owned(),
            ],
        ),
    ] {
        let text = one_line(&read(file));
        for statement in statements {
            assert!(
                text.contains(&statement),
                "{file} does not say `{statement}`"
            );
        }
    }
}

#[test]
fn p1_rel_01_step_3_runs_checks_that_exist() {
    let doc = read("docs/RELEASING.md");
    let step: &str = doc
        .split("3. **Within budget.**")
        .nth(1)
        .and_then(|rest| rest.split("4. **").next())
        .unwrap();
    assert!(step.contains("cargo auditable build --release --locked -p duscope"));
    assert!(step.contains("cargo test -p duscope --test dependencies"));
    assert!(root().join("duscope/tests/dependencies.rs").exists());
    assert!(
        !step
            .lines()
            .any(|l| l.trim_start().starts_with("cargo tree")),
        "a cargo tree count does not match DEPENDENCIES.md"
    );
}

#[test]
fn p1_rel_01_the_index_waits_fit_the_crates_io_tokens_life() {
    let workflow = read(".github/workflows/release.yml");
    let job: &str = workflow
        .split("\n  publish-crates:\n")
        .nth(1)
        .and_then(|rest| rest.split("\n  announce:\n").next())
        .unwrap();
    let number_after = |prefix: &str, end: char| -> u64 {
        let rest = job
            .split(prefix)
            .nth(1)
            .unwrap_or_else(|| panic!("no `{prefix}`"));
        rest.split(end).next().unwrap().trim().parse().unwrap()
    };
    let (tries, pause) = (number_after("$(seq ", ')'), number_after("sleep ", ';'));
    // Six crates, each waiting up to a minute in cargo publish and then the
    // loop's tries; the token trusted publishing buys lives 30 minutes, and
    // building and uploading need some of it.
    let waits = 6 * (60 + tries * pause);
    assert!(
        waits <= 20 * 60,
        "the index waits alone can take {} min of the token's 30",
        waits / 60
    );
}
