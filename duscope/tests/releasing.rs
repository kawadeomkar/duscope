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

#[test]
fn p1_rel_01_the_build_time_budget_is_the_measured_one() {
    let ci = read(".github/workflows/ci.yml");
    let line = ci
        .lines()
        .find(|l| l.trim_start().starts_with("BUILD_TIME_BUDGET_SECS:"))
        .unwrap();
    let secs: u32 = line
        .split(':')
        .nth(1)
        .unwrap()
        .trim()
        .trim_matches('"')
        .parse()
        .unwrap();
    assert!(secs < 90, "the build-time budget is still the 90 s ceiling");
    // Its comment names the CI run that measured it, as size_budget's does.
    let before: Vec<&str> = ci
        .lines()
        .take_while(|l| !l.trim_start().starts_with("BUILD_TIME_BUDGET_SECS:"))
        .collect();
    let mut comment: Vec<&str> = before
        .iter()
        .rev()
        .take_while(|l| l.trim_start().starts_with('#'))
        .map(|l| l.trim_start().trim_start_matches('#').trim())
        .collect();
    comment.reverse();
    let comment = comment.join(" ");
    assert!(
        comment.contains("(run ") && comment.contains(" s"),
        "BUILD_TIME_BUDGET_SECS's comment cites no measured run: {comment}"
    );
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
