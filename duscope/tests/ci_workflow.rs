//! P0-CI-01: the shape of the CI workflow, and the settings checklist that makes
//! its jobs required checks.
//!
//! ci.yml is read as text, without a YAML crate: its jobs are the keys indented
//! by two spaces under the top-level `jobs:`, and job-level keys are indented by
//! four, which is how the file is written. These tests read files only; they never
//! run the duscope binary.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

const WORKFLOW: &str = ".github/workflows/ci.yml";
const SETTINGS: &str = "docs/github-settings.md";

/// The per-job budget recorded in ci.yml.
const JOB_BUDGET_MINUTES: u32 = 20;

/// The required checks as branch protection names them: the job name with its
/// matrix values, as in the PUT of docs/github-settings.md.
const REQUIRED_CHECKS: &[&str] = &[
    "fmt",
    "clippy (macos-26)",
    "clippy (ubuntu-24.04)",
    "clippy (ubuntu-24.04-arm)",
    "test (macos-26, default)",
    "test (ubuntu-24.04, default)",
    "test (ubuntu-24.04-arm, default)",
    "test (macos-26, locale)",
    "test (ubuntu-24.04, locale)",
    "test (ubuntu-24.04-arm, locale)",
    "msrv (pinned)",
    "msrv (stable)",
    "deny",
    "docs",
    "schemas",
    "size (macos-26)",
    "size (ubuntu-24.04)",
    "build-time",
    "repro",
    "changelog",
];

/// Jobs that run but do not gate a merge.
const OPTIONAL_JOBS: &[&str] = &["intel-mac", "size-comment"];

/// The runners of the spec: the three first-class ones and best-effort Intel macOS.
const RUNNERS: &[&str] = &[
    "macos-26",
    "ubuntu-24.04",
    "ubuntu-24.04-arm",
    "macos-26-intel",
];

/// The justfile recipe each job runs, so CI and a developer's `just check` agree.
const RECIPES: &[(&str, &str)] = &[
    ("fmt", "fmt"),
    ("clippy", "clippy"),
    ("test", "test"),
    ("msrv", "test"),
    ("deny", "deny"),
    ("docs", "docs"),
    ("size", "size"),
    ("repro", "repro"),
];

fn read(relative: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(relative);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

struct Job {
    id: String,
    lines: Vec<String>,
}

impl Job {
    /// The `(key, value)` pairs of the job's `strategy.matrix`, from both the
    /// `key: [a, b]` and the `include: - key: value` forms.
    fn matrix(&self) -> Vec<(String, String)> {
        let mut pairs = Vec::new();
        let mut indent = None;
        for line in &self.lines {
            let depth = line.len() - line.trim_start().len();
            match indent {
                None if line.trim() == "matrix:" => indent = Some(depth),
                Some(matrix) if depth > matrix => {
                    let entry = line.trim().trim_start_matches("- ");
                    if entry.starts_with('#') {
                        continue;
                    }
                    let Some((key, value)) = entry.split_once(':') else {
                        continue;
                    };
                    let value = value.split(" #").next().unwrap_or_default().trim();
                    for value in value.trim_matches(['[', ']']).split(',') {
                        let value = value.trim();
                        if !value.is_empty() {
                            pairs.push((key.trim().to_owned(), value.to_owned()));
                        }
                    }
                }
                Some(_) if !line.trim().is_empty() => indent = None,
                _ => {}
            }
        }
        pairs
    }
}

fn jobs(workflow: &str) -> Vec<Job> {
    let mut jobs: Vec<Job> = Vec::new();
    let mut in_jobs = false;
    for line in workflow.lines() {
        if !line.starts_with(' ') && !line.trim().is_empty() && !line.starts_with('#') {
            in_jobs = line == "jobs:";
            continue;
        }
        if !in_jobs {
            continue;
        }
        let id = line
            .strip_prefix("  ")
            .filter(|rest| !rest.starts_with([' ', '#']))
            .and_then(|rest| rest.strip_suffix(':'));
        match (id, jobs.last_mut()) {
            (Some(id), _) => jobs.push(Job {
                id: id.to_owned(),
                lines: Vec::new(),
            }),
            (None, Some(job)) => job.lines.push(line.to_owned()),
            (None, None) => {}
        }
    }
    jobs
}

/// The value of a job-level key (`    key: value`), if the job sets it.
fn job_key<'a>(job: &'a Job, key: &str) -> Option<&'a str> {
    let prefix = format!("    {key}:");
    job.lines
        .iter()
        .find_map(|line| line.strip_prefix(&prefix))
        .map(str::trim)
}

fn required_job_ids() -> BTreeSet<&'static str> {
    REQUIRED_CHECKS
        .iter()
        .map(|check| check.split(" (").next().unwrap())
        .collect()
}

#[test]
fn p0_ci_01_spec_jobs_exist() {
    let ids: BTreeSet<String> = jobs(&read(WORKFLOW)).into_iter().map(|j| j.id).collect();
    for id in required_job_ids().iter().chain(OPTIONAL_JOBS) {
        assert!(ids.contains(*id), "ci.yml has no `{id}` job: {ids:?}");
    }
    let classified: BTreeSet<&str> = required_job_ids()
        .into_iter()
        .chain(OPTIONAL_JOBS.iter().copied())
        .collect();
    let unclassified: Vec<&String> = ids
        .iter()
        .filter(|id| !classified.contains(id.as_str()))
        .collect();
    assert!(
        unclassified.is_empty(),
        "jobs neither required (docs/github-settings.md) nor listed as optional: {unclassified:?}"
    );
}

#[test]
fn p0_ci_01_every_job_has_a_timeout_within_the_budget() {
    let workflow = read(WORKFLOW);
    assert!(
        workflow.contains(&format!("{JOB_BUDGET_MINUTES} minutes per job")),
        "ci.yml records the {JOB_BUDGET_MINUTES}-minute per-job budget"
    );
    let jobs = jobs(&workflow);
    assert!(!jobs.is_empty(), "no jobs parsed from ci.yml");
    for job in &jobs {
        let minutes: u32 = job_key(job, "timeout-minutes")
            .unwrap_or_else(|| panic!("job `{}` has no timeout-minutes", job.id))
            .parse()
            .unwrap_or_else(|e| panic!("job `{}`: timeout-minutes is not a number: {e}", job.id));
        assert!(
            (1..=JOB_BUDGET_MINUTES).contains(&minutes),
            "job `{}` has timeout-minutes {minutes}, over the {JOB_BUDGET_MINUTES}-minute budget",
            job.id
        );
    }
}

#[test]
fn p0_ci_01_actions_are_pinned_to_a_full_commit_sha() {
    let mut count = 0;
    for (n, line) in read(WORKFLOW).lines().enumerate() {
        let Some((_, rest)) = line.split_once("uses:") else {
            continue;
        };
        let action = rest.split_whitespace().next().unwrap_or_default();
        if action.starts_with("./") || action.starts_with("docker://") {
            continue;
        }
        count += 1;
        let line_no = n + 1;
        let (_, sha) = action
            .split_once('@')
            .unwrap_or_else(|| panic!("ci.yml:{line_no}: `{action}` has no @ref"));
        assert!(
            sha.len() == 40
                && sha
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "ci.yml:{line_no}: `{action}` is not pinned to a full commit SHA"
        );
        assert!(
            rest.contains("# v"),
            "ci.yml:{line_no}: `{action}` has no `# vX.Y.Z` version comment"
        );
    }
    assert!(count > 0, "no actions found in ci.yml");
}

#[test]
fn p0_ci_01_token_is_read_only_unless_a_job_widens_it() {
    let workflow = read(WORKFLOW);
    let top_level: Vec<&str> = workflow
        .lines()
        .skip_while(|line| *line != "permissions:")
        .skip(1)
        .take_while(|line| line.starts_with(' '))
        .collect();
    assert_eq!(
        top_level,
        ["  contents: read"],
        "the workflow-wide permissions are `contents: read` alone"
    );
    for job in jobs(&workflow) {
        let writes: Vec<&String> = job
            .lines
            .iter()
            .filter(|line| line.trim_end().ends_with(": write"))
            .collect();
        if job.id == "size-comment" {
            assert_eq!(
                writes.iter().map(|l| l.trim()).collect::<Vec<_>>(),
                ["pull-requests: write"],
                "size-comment needs pull-requests: write and nothing more"
            );
        } else {
            assert!(
                writes.is_empty(),
                "job `{}` widens the token: {writes:?}",
                job.id
            );
        }
    }
}

#[test]
fn p0_ci_01_jobs_run_on_the_spec_runners() {
    for job in jobs(&read(WORKFLOW)) {
        let runs_on =
            job_key(&job, "runs-on").unwrap_or_else(|| panic!("job `{}` has no runs-on", job.id));
        if runs_on == "${{ matrix.os }}" {
            let oses: Vec<String> = job
                .matrix()
                .into_iter()
                .filter(|(key, _)| key == "os")
                .map(|(_, value)| value)
                .collect();
            assert!(!oses.is_empty(), "job `{}` has no matrix os values", job.id);
            for os in oses {
                assert!(
                    RUNNERS.contains(&os.as_str()),
                    "job `{}` runs on `{os}`",
                    job.id
                );
            }
        } else {
            assert!(
                RUNNERS.contains(&runs_on),
                "job `{}` runs on `{runs_on}`",
                job.id
            );
        }
    }
}

#[test]
fn p0_ci_01_jobs_run_the_justfile_recipes() {
    let justfile = read("justfile");
    let jobs = jobs(&read(WORKFLOW));
    for (job_id, recipe) in RECIPES {
        assert!(
            justfile
                .lines()
                .any(|line| line.starts_with(&format!("{recipe}:"))
                    || line.starts_with(&format!("{recipe} "))),
            "the justfile has no `{recipe}` recipe"
        );
        let job = jobs
            .iter()
            .find(|job| job.id == *job_id)
            .unwrap_or_else(|| panic!("ci.yml has no `{job_id}` job"));
        let runs_recipe = job.lines.iter().any(|line| {
            let command = line
                .trim()
                .trim_start_matches("- ")
                .trim_start_matches("run: ");
            command == format!("just {recipe}")
                || command.starts_with(&format!("just {recipe} "))
                || command.contains(&format!("&& just {recipe}"))
        });
        assert!(runs_recipe, "job `{job_id}` does not run `just {recipe}`");
    }
}

#[test]
fn p0_ci_01_settings_require_exactly_the_required_checks() {
    let settings = read(SETTINGS);
    let contexts: BTreeSet<&str> = settings
        .lines()
        .filter_map(|line| line.split_once("\"context\": \"").map(|(_, rest)| rest))
        .filter_map(|rest| rest.split_once('"').map(|(context, _)| context))
        .collect();
    let expected: BTreeSet<&str> = REQUIRED_CHECKS.iter().copied().collect();
    assert_eq!(
        contexts, expected,
        "the branch-protection PUT in {SETTINGS}"
    );
    assert!(
        settings.contains("\"strict\": true"),
        "branches must be up to date before merging"
    );

    // Each check names a job of ci.yml, and its matrix values are in that job's matrix.
    let jobs = jobs(&read(WORKFLOW));
    for check in REQUIRED_CHECKS {
        let (id, values) = check.split_once(" (").unwrap_or((check, ""));
        let job = jobs
            .iter()
            .find(|job| job.id == id)
            .unwrap_or_else(|| panic!("`{check}`: ci.yml has no `{id}` job"));
        assert!(!OPTIONAL_JOBS.contains(&id), "`{check}` is an optional job");
        let matrix: BTreeSet<String> = job.matrix().into_iter().map(|(_, value)| value).collect();
        for value in values
            .trim_end_matches(')')
            .split(", ")
            .filter(|v| !v.is_empty())
        {
            assert!(
                matrix.contains(value),
                "`{check}`: the `{id}` job's matrix has no value `{value}`: {matrix:?}"
            );
        }
    }
}

#[test]
fn p0_ci_01_settings_todos_carry_their_command_or_owner() {
    let settings = read(SETTINGS);
    let lines: Vec<&str> = settings.lines().collect();
    let mut items = 0;
    for (i, line) in lines.iter().enumerate() {
        let is_item = line.starts_with("- [x] ") || line.starts_with("- [ ] ");
        if !is_item {
            continue;
        }
        items += 1;
        if line.starts_with("- [x] ") {
            continue;
        }
        let item: Vec<&str> = lines[i..]
            .iter()
            .enumerate()
            .take_while(|(j, l)| *j == 0 || !(l.starts_with("- [") || l.starts_with('#')))
            .map(|(_, l)| *l)
            .collect();
        let item = item.join("\n");
        let has_command = item.contains("gh api ") || item.contains("gh label ");
        let has_owner = ["P0-REPO-03", "P0-CI-02"]
            .iter()
            .any(|id| item.contains(id))
            && !item.contains("```");
        assert!(
            has_command || has_owner,
            "to-do item without a command or an owning package:\n{item}"
        );
    }
    assert!(items > 0, "no checklist items in {SETTINGS}");
}
