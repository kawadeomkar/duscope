//! P2-TEST-01: every fuzz target runs in CI. ci.yml's fuzz-smoke runs each
//! target of `fuzz/Cargo.toml` for 60 s on every pull request, and
//! nightly-fuzz.yml for ten minutes each night, on the dated nightly the
//! justfile pins; only nightly-fuzz.yml's report job may write issues.
//!
//! The workflows are read as text, as duscope's `ci_workflow.rs` reads them.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fs;
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(relative: &str) -> String {
    let path = root().join(relative);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// The `[[bin]]` targets of `fuzz/Cargo.toml`, with their paths.
fn targets() -> Vec<(String, String)> {
    let manifest: toml::Table = toml::from_str(&read("fuzz/Cargo.toml")).unwrap();
    let mut targets: Vec<(String, String)> = manifest["bin"]
        .as_array()
        .unwrap()
        .iter()
        .map(|bin| {
            (
                bin["name"].as_str().unwrap().to_owned(),
                bin["path"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    targets.sort();
    targets
}

/// The values of the one `target: [...]` matrix line in `workflow`.
fn matrix(workflow: &str) -> Vec<String> {
    let lines: Vec<&str> = workflow
        .lines()
        .filter(|line| line.trim_start().starts_with("target: ["))
        .collect();
    assert_eq!(lines.len(), 1, "one target matrix: {lines:?}");
    let list = lines[0]
        .split('[')
        .nth(1)
        .unwrap()
        .split(']')
        .next()
        .unwrap();
    let mut values: Vec<String> = list.split(',').map(|v| v.trim().to_owned()).collect();
    values.sort();
    values
}

/// The lines of the job `id` in `workflow`: from its key, indented by two
/// spaces under `jobs:`, to the next job's.
fn job<'a>(workflow: &'a str, id: &str) -> Vec<&'a str> {
    let key = format!("  {id}:");
    let mut lines = workflow.lines().skip_while(|line| *line != key);
    let first = lines.next().unwrap_or_else(|| panic!("no job `{id}`"));
    std::iter::once(first)
        .chain(lines.take_while(|line| {
            line.is_empty() || line.starts_with("    ") || line.trim_start().starts_with('#')
        }))
        .collect()
}

#[test]
fn p2_test_01_every_fuzz_target_runs_in_both_workflows() {
    let targets = targets();
    let names: Vec<String> = targets.iter().map(|(name, _)| name.clone()).collect();
    for name in ["sql", "ncdu", "gdu", "gpscan", "args"] {
        assert!(names.iter().any(|n| n == name), "no {name} target");
    }
    for (name, path) in &targets {
        assert!(
            root().join("fuzz").join(path).is_file(),
            "the {name} target has no {path}"
        );
    }
    let ci = read(".github/workflows/ci.yml");
    let nightly = read(".github/workflows/nightly-fuzz.yml");
    assert_eq!(matrix(&job(&ci, "fuzz-smoke").join("\n")), names, "ci.yml");
    assert_eq!(matrix(&nightly), names, "nightly-fuzz.yml");
    // The same recipe, 60 s and ten minutes.
    assert!(
        job(&ci, "fuzz-smoke")
            .iter()
            .any(|l| l.trim() == "run: just fuzz \"$TARGET\" 60")
    );
    assert!(
        job(&nightly, "fuzz")
            .iter()
            .any(|l| l.trim() == "run: just fuzz \"$TARGET\" 600")
    );
    for (file, workflow) in [("ci.yml", &ci), ("nightly-fuzz.yml", &nightly)] {
        let id = if file == "ci.yml" {
            "fuzz-smoke"
        } else {
            "fuzz"
        };
        assert!(
            job(workflow, id)
                .iter()
                .any(|l| l.trim() == "runs-on: ubuntu-24.04"),
            "{file}: {id} runs on ubuntu-24.04"
        );
    }
    // A dated nightly, so a run reproduces.
    let justfile = read("justfile");
    let pin = justfile
        .lines()
        .find_map(|line| line.strip_prefix("fuzz_toolchain := "))
        .unwrap()
        .trim_matches('"');
    let date = pin.strip_prefix("nightly-").unwrap();
    assert!(
        date.len() == 10 && date.bytes().filter(|b| *b == b'-').count() == 2,
        "{pin} is not nightly-YYYY-MM-DD"
    );
}

#[test]
fn p2_test_01_nightly_fuzz_runs_each_night_and_only_its_report_writes() {
    let workflow = read(".github/workflows/nightly-fuzz.yml");
    let on: Vec<&str> = workflow
        .lines()
        .skip_while(|line| *line != "on:")
        .skip(1)
        .take_while(|line| line.starts_with(' ') || line.is_empty())
        .filter(|line| line.starts_with("  ") && !line.starts_with("   "))
        .map(str::trim)
        .collect();
    assert_eq!(on, ["schedule:", "workflow_dispatch:"], "its triggers");
    assert!(workflow.contains("- cron: "));
    let top: Vec<&str> = workflow
        .lines()
        .skip_while(|line| *line != "permissions:")
        .skip(1)
        .take_while(|line| line.starts_with(' '))
        .collect();
    assert_eq!(
        top,
        ["  contents: read"],
        "the workflow-wide token is read-only"
    );
    let writes = |id: &str| -> Vec<String> {
        job(&workflow, id)
            .iter()
            .filter(|l| !l.trim_start().starts_with('#'))
            .map(|l| l.trim().to_owned())
            .filter(|l| l.ends_with(": write"))
            .collect()
    };
    assert!(writes("fuzz").is_empty(), "the fuzzing jobs write nothing");
    assert_eq!(writes("report"), ["issues: write"]);
    let report = job(&workflow, "report").join("\n");
    assert!(report.contains("needs: fuzz"));
    assert!(report.contains("needs.fuzz.result == 'failure'"));
    assert!(report.contains("gh issue create") && report.contains("--label fuzz"));
    assert!(report.contains("base64"), "the input goes in the issue");
}
