//! P0-CI-01: the shape of the CI workflow, and the settings checklist that makes
//! its jobs required checks.
//!
//! ci.yml is read as text, without a YAML crate: its jobs are the keys indented
//! by two spaces under the top-level `jobs:`, and job-level keys are indented by
//! four, which is how the file is written. These tests read files, and run one
//! step's script under bash with stub commands; they never run the duscope binary.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

const WORKFLOW: &str = ".github/workflows/ci.yml";
const RELEASE_WORKFLOW: &str = ".github/workflows/release.yml";
const SETTINGS: &str = "docs/github-settings.md";

/// The per-job budget recorded in ci.yml.
const JOB_BUDGET_MINUTES: u32 = 20;

/// The required checks as branch protection names them: the job name with its
/// matrix values, as in the PUT of docs/github-settings.md. Every check a pull
/// request runs is required but the [`OPTIONAL_JOBS`]: each other job of
/// ci.yml, then the jobs of release.yml that are not [`TAG_ONLY_JOBS`].
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
    "platform (macos-26)",
    "platform (ubuntu-24.04)",
    "platform (ubuntu-24.04-arm)",
    "bench-regression (macos-26)",
    "bench-regression (ubuntu-24.04)",
    "bench-regression (ubuntu-24.04-arm)",
    "msrv (pinned)",
    "msrv (stable)",
    "deny",
    "fuzz-smoke (sql)",
    "fuzz-smoke (ncdu)",
    "fuzz-smoke (gdu)",
    "fuzz-smoke (gpscan)",
    "fuzz-smoke (args)",
    "docs",
    "schemas",
    "size (macos-26)",
    "size (ubuntu-24.04)",
    "build-time",
    "repro",
    "changelog",
    "size-comment",
    "verify-tag",
    "plan",
    "deb-assets",
    "deb (amd64)",
    "deb (arm64)",
    "deb-install (debian:12, amd64)",
    "deb-install (debian:12, arm64)",
    "deb-install (ubuntu:22.04, amd64)",
    "deb-install (ubuntu:22.04, arm64)",
];

/// Jobs of ci.yml that run on every pull request but do not gate a merge:
/// intel-mac builds on GitHub's best-effort Intel macOS runner.
const OPTIONAL_JOBS: &[&str] = &["intel-mac"];

/// The jobs of release.yml that run only for a tag: pull requests report them
/// skipped, so they are not required.
const TAG_ONLY_JOBS: &[&str] = &[
    "build-local-artifacts",
    "build-global-artifacts",
    "host",
    "publish-crates",
    "homebrew",
    "announce",
];

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
    ("fuzz-smoke", "fuzz"),
    ("docs", "docs"),
    ("size", "size"),
    ("repro", "repro"),
    ("platform", "platform-tests"),
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

    /// The combinations of the job's `strategy.matrix`: the cross product of
    /// its `key: [a, b]` lists, with its `include:` entries (`- key: value`
    /// with the entry's other keys below) expanded as GitHub does; one empty
    /// combination without a matrix. Values lose their quotes.
    fn combinations(&self) -> Vec<BTreeMap<String, String>> {
        let mut lists: Vec<(String, Vec<String>)> = Vec::new();
        let mut include: Vec<BTreeMap<String, String>> = Vec::new();
        let mut matrix_depth = None;
        let mut include_depth = None;
        for line in &self.lines {
            let entry = line.trim();
            if entry.is_empty() || entry.starts_with('#') {
                continue;
            }
            let depth = line.len() - line.trim_start().len();
            let Some(matrix) = matrix_depth else {
                if entry == "matrix:" {
                    matrix_depth = Some(depth);
                }
                continue;
            };
            if depth <= matrix {
                break;
            }
            if entry == "include:" {
                include_depth = Some(depth);
                continue;
            }
            let (key, value) = entry.trim_start_matches("- ").split_once(':').unwrap();
            let value = value.split(" #").next().unwrap_or_default();
            let (key, value) = (key.trim().to_owned(), value.trim().to_owned());
            match include_depth {
                Some(d) if depth > d => {
                    if entry.starts_with("- ") {
                        include.push(BTreeMap::new());
                    }
                    let value = value.trim_matches('"').to_owned();
                    include.last_mut().unwrap().insert(key, value);
                }
                _ => {
                    let values = value
                        .trim_matches(['[', ']'])
                        .split(',')
                        .map(|v| v.trim().trim_matches('"').to_owned())
                        .filter(|v| !v.is_empty())
                        .collect();
                    lists.push((key, values));
                }
            }
        }
        if lists.is_empty() && !include.is_empty() {
            return include;
        }
        let mut combinations = vec![BTreeMap::new()];
        for (key, values) in &lists {
            let mut next = Vec::new();
            for combination in &combinations {
                for value in values {
                    let mut extended = combination.clone();
                    extended.insert(key.clone(), value.clone());
                    next.push(extended);
                }
            }
            combinations = next;
        }
        // An include entry beside lists adds its keys to every combination
        // whose list values it matches, or is a combination of its own when
        // it matches none.
        let crossed = combinations.len();
        for entry in include {
            let mut matched = false;
            for combination in &mut combinations[..crossed] {
                let fits = entry.iter().all(|(key, value)| {
                    lists.iter().all(|(listed, _)| listed != key)
                        || combination.get(key) == Some(value)
                });
                if fits {
                    combination.extend(entry.clone());
                    matched = true;
                }
            }
            if !matched {
                combinations.push(entry);
            }
        }
        combinations
    }

    /// The check names the job reports: its `name:` with each
    /// `${{ matrix.<key> }}` filled in, once per matrix combination. A job
    /// without a name and without a matrix reports its id.
    fn check_names(&self) -> Vec<String> {
        let combinations = self.combinations();
        let template = match job_key(self, "name") {
            Some(name) => name.to_owned(),
            None if combinations.len() == 1 && combinations[0].is_empty() => self.id.clone(),
            None => panic!("job `{}` has no name: GitHub would make one up", self.id),
        };
        combinations
            .iter()
            .map(|combination| {
                let mut name = template.clone();
                for (key, value) in combination {
                    name = name.replace(&format!("${{{{ matrix.{key} }}}}"), value);
                }
                assert!(
                    !name.contains("${{"),
                    "job `{}`: `{name}` keeps an expression",
                    self.id
                );
                name
            })
            .collect()
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
    let ci: BTreeSet<String> = jobs(&read(WORKFLOW)).into_iter().map(|j| j.id).collect();
    let release: BTreeSet<String> = jobs(&read(RELEASE_WORKFLOW))
        .into_iter()
        .map(|j| j.id)
        .collect();
    let required = required_job_ids();
    for id in &required {
        assert!(
            ci.contains(*id) || release.contains(*id),
            "neither ci.yml nor release.yml has a `{id}` job"
        );
    }
    for id in OPTIONAL_JOBS {
        assert!(ci.contains(*id), "ci.yml has no `{id}` job: {ci:?}");
    }
    for id in TAG_ONLY_JOBS {
        assert!(
            release.contains(*id),
            "release.yml has no `{id}` job: {release:?}"
        );
    }
    let unclassified: Vec<&String> = ci
        .iter()
        .chain(&release)
        .filter(|id| {
            let id = id.as_str();
            !required.contains(id) && !OPTIONAL_JOBS.contains(&id) && !TAG_ONLY_JOBS.contains(&id)
        })
        .collect();
    assert!(
        unclassified.is_empty(),
        "jobs neither required (docs/github-settings.md), optional nor tag-only: {unclassified:?}"
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
            // Block (`contents: write`) and flow (`{ contents: write }`) forms,
            // and `write-all`.
            .filter(|line| line.contains(": write") || line.contains("write-all"))
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

    // The names the jobs a pull request runs report, from each job's `name:`
    // and matrix, are exactly the required checks: ci.yml's jobs but the
    // optional ones, and release.yml's but the tag-only ones.
    let release = jobs(&read(RELEASE_WORKFLOW));
    let reported: BTreeSet<String> = jobs(&read(WORKFLOW))
        .iter()
        .filter(|job| !OPTIONAL_JOBS.contains(&job.id.as_str()))
        .chain(
            release
                .iter()
                .filter(|job| !TAG_ONLY_JOBS.contains(&job.id.as_str())),
        )
        .flat_map(Job::check_names)
        .collect();
    let required: BTreeSet<String> = REQUIRED_CHECKS.iter().map(|&c| c.to_owned()).collect();
    assert_eq!(
        reported, required,
        "the check names pull requests report differ from the required checks"
    );
}

#[test]
fn p0_ci_01_check_names_come_from_the_name_and_matrix() {
    let workflow = "jobs:\n  test:\n    name: test (${{ matrix.os }}, ${{ matrix.leg }})\n    \
                    strategy:\n      matrix:\n        os: [a, b] # two\n        # comment\n        \
                    leg: [x, y]\n    runs-on: ${{ matrix.os }}\n  size:\n    name: size \
                    (${{ matrix.os }})\n    strategy:\n      matrix:\n        include:\n          \
                    - os: m\n            key: mac\n          - os: u\n            key: ubuntu\n    \
                    runs-on: ${{ matrix.os }}\n  fmt:\n    name: fmt\n    runs-on: u\n  \
                    install:\n    name: install (${{ matrix.image }}, ${{ matrix.arch }}, \
                    ${{ matrix.runner }})\n    strategy:\n      matrix:\n        \
                    image: [\"d:12\", u]\n        arch: [amd64, arm64]\n        include:\n          \
                    - arch: amd64\n            runner: x\n          - arch: arm64\n            \
                    runner: y\n          - arch: riscv\n            image: d:12\n            \
                    runner: z\n    runs-on: ${{ matrix.runner }}\n  plan:\n    runs-on: u\n";
    let names: Vec<Vec<String>> = jobs(workflow).iter().map(Job::check_names).collect();
    assert_eq!(
        names,
        [
            vec!["test (a, x)", "test (a, y)", "test (b, x)", "test (b, y)"],
            vec!["size (m)", "size (u)"],
            vec!["fmt"],
            // An include entry extends the combinations it matches, and one
            // that matches none is a combination of its own.
            vec![
                "install (d:12, amd64, x)",
                "install (d:12, arm64, y)",
                "install (u, amd64, x)",
                "install (u, arm64, y)",
                "install (d:12, riscv, z)",
            ],
            // A job without a name or a matrix reports its id.
            vec!["plan"],
        ]
    );
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

#[test]
fn p1_test_01_the_platform_job_runs_the_ignored_tests_as_root() {
    let jobs = jobs(&read(WORKFLOW));
    let job = |id: &str| {
        jobs.iter()
            .find(|j| j.id == id)
            .unwrap_or_else(|| panic!("ci.yml has no `{id}` job"))
            .lines
            .join("\n")
    };
    // The platform tests skip quietly unless asked for and run as root, so
    // the job is only worth something with all three: the recipe asks for
    // them by name, and the job runs each test binary under sudo.
    let platform = job("platform");
    assert!(
        platform.contains("_RUNNER=sudo -E"),
        "the platform job does not run the tests under sudo"
    );
    let justfile = read("justfile");
    let recipe: Vec<&str> = justfile
        .lines()
        .skip_while(|line| !line.starts_with("platform-tests:"))
        .skip(1)
        .take_while(|line| line.starts_with(' '))
        .collect();
    let recipe = recipe.join("\n");
    for needed in ["DUSCOPE_TEST_PLATFORM=1", "--release", "--ignored --exact"] {
        assert!(
            recipe.contains(needed),
            "just platform-tests lacks {needed:?}"
        );
    }
    // Baselines come from pushes to main only, and main keeps its result
    // whatever the comparison says.
    let bench = job("bench-regression");
    assert!(
        bench.contains("--event push"),
        "bench-regression takes pull requests' results"
    );
    assert!(
        bench.contains(
            "!cancelled() && github.event_name == 'push' && github.ref == 'refs/heads/main'"
        ),
        "bench-regression drops main's result when the comparison fails"
    );
}

/// The `run: |` script of the step with this name in `job`, dedented.
fn step_script(job: &Job, name: &str) -> String {
    let mut lines = job
        .lines
        .iter()
        .skip_while(|line| line.trim() != format!("- name: {name}"))
        .skip(1);
    let run = lines
        .by_ref()
        .find(|line| line.trim() == "run: |")
        .unwrap_or_else(|| panic!("job `{}`: no `run: |` in step `{name}`", job.id));
    let indent = run.len() - run.trim_start().len() + 2;
    let mut script = String::new();
    for line in lines {
        if !line.trim().is_empty() && line.len() - line.trim_start().len() < indent {
            break;
        }
        script.push_str(line.get(indent..).unwrap_or_default());
        script.push('\n');
    }
    script
}

#[test]
fn p0_ci_01_build_time_builds_again_before_failing() {
    let jobs = jobs(&read(WORKFLOW));
    let job = jobs.iter().find(|j| j.id == "build-time").unwrap();
    let script = step_script(job, "Cold cargo build --release");
    // cargo logs its arguments (and fails when asked); date hands out the
    // instants listed in `times`, one a call, so each build takes what the
    // case says.
    let stubs = [
        (
            "cargo",
            "#!/usr/bin/env bash\necho \"$*\" >> \"$STUB_DIR/cargo.log\"\n[[ -z ${CARGO_FAILS:-} ]]\n",
        ),
        (
            "date",
            "#!/usr/bin/env bash\nhead -n 1 \"$STUB_DIR/times\"\ntail -n +2 \"$STUB_DIR/times\" > \"$STUB_DIR/times.next\"\nmv \"$STUB_DIR/times.next\" \"$STUB_DIR/times\"\n",
        ),
    ];
    let build = "build --release --locked --offline -p duscope";
    // (start and end of each build, cargo fails, succeeds, cargo's calls)
    let cases: &[(&[&str], bool, bool, &[&str])] = &[
        (&["0", "50"], false, true, &[build]),
        (
            &["0", "70", "100", "150"],
            false,
            true,
            &[build, "clean", build],
        ),
        (
            &["0", "70", "100", "170"],
            false,
            false,
            &[build, "clean", build],
        ),
        (&["0", "50"], true, false, &[build]),
    ];
    for (times, cargo_fails, succeeds, calls) in cases {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        let bin = dir.join("bin");
        fs::create_dir_all(&bin).unwrap();
        for (name, body) in stubs {
            let path = bin.join(name);
            fs::write(&path, body).unwrap();
            let mut mode = fs::metadata(&path).unwrap().permissions();
            std::os::unix::fs::PermissionsExt::set_mode(&mut mode, 0o755);
            fs::set_permissions(&path, mode).unwrap();
        }
        fs::write(dir.join("times"), times.join("\n") + "\n").unwrap();
        fs::write(dir.join("step.sh"), &script).unwrap();
        let mut search = vec![bin];
        search.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
        let mut step = std::process::Command::new("bash");
        step.args(["--noprofile", "--norc", "-eo", "pipefail"])
            .arg(dir.join("step.sh"))
            .current_dir(dir)
            .env("PATH", std::env::join_paths(search).unwrap())
            .env("STUB_DIR", dir)
            .env("BUILD_TIME_BUDGET_SECS", "63")
            .env("GITHUB_STEP_SUMMARY", dir.join("summary"));
        if *cargo_fails {
            step.env("CARGO_FAILS", "1");
        }
        let out = step.output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert_eq!(out.status.success(), *succeeds, "{times:?}\n{stdout}");
        let log = fs::read_to_string(dir.join("cargo.log")).unwrap_or_default();
        assert_eq!(log.lines().collect::<Vec<_>>(), *calls, "{times:?}");
        // A build over budget warns before the second; failing names both.
        let over = times.len() > 2;
        assert_eq!(
            stdout.contains("::warning title=build-time::"),
            over,
            "{stdout}"
        );
        assert_eq!(
            stdout.contains("::error title=build-time::"),
            over && !succeeds,
            "{stdout}"
        );
    }
}

/// The `CPU model=seconds` lines of `BUILD_TIME_BUDGETS` in the workflow's
/// `env`.
fn build_budgets(workflow: &str) -> Vec<(String, u32)> {
    workflow
        .lines()
        .skip_while(|line| line.trim_end() != "  BUILD_TIME_BUDGETS: |")
        .skip(1)
        .take_while(|line| line.starts_with("    "))
        .map(|line| {
            let (cpu, secs) = line.trim().rsplit_once('=').unwrap();
            (cpu.to_owned(), secs.parse().unwrap())
        })
        .collect()
}

#[test]
fn p0_ci_01_each_cpu_model_has_its_build_budget() {
    let workflow = read(WORKFLOW);
    let budgets = build_budgets(&workflow);
    assert!(budgets.len() >= 2, "{budgets:?}");
    for (cpu, secs) in &budgets {
        assert!(*secs < 90, "{cpu}: the spec's ceiling is 90 s");
    }
    let table: String = budgets
        .iter()
        .map(|(cpu, secs)| format!("{cpu}={secs}\n"))
        .collect();
    let largest = budgets.iter().map(|(_, secs)| *secs).max().unwrap();
    let jobs = jobs(&workflow);
    let job = jobs.iter().find(|j| j.id == "build-time").unwrap();
    let script = step_script(job, "This machine's budget");
    // The budget the step hands the build step, and what it printed, on a
    // machine whose /proc/cpuinfo names `model`.
    let pick = |model: &str| {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        let cpuinfo = dir.join("cpuinfo");
        let core = |n: u32| format!("processor\t: {n}\nmodel name\t: {model}\nflags\t\t: fpu\n\n");
        fs::write(&cpuinfo, core(0) + &core(1)).unwrap();
        fs::write(dir.join("step.sh"), &script).unwrap();
        let out = std::process::Command::new("bash")
            .args(["--noprofile", "--norc", "-eo", "pipefail"])
            .arg(dir.join("step.sh"))
            .env("BUILD_TIME_BUDGETS", &table)
            .env("DUSCOPE_TEST_CPUINFO", &cpuinfo)
            .env("GITHUB_ENV", dir.join("env"))
            .env("GITHUB_STEP_SUMMARY", dir.join("summary"))
            .output()
            .unwrap();
        assert!(out.status.success(), "{model}: {out:?}");
        let env = fs::read_to_string(dir.join("env")).unwrap();
        let secs: u32 = env
            .trim()
            .strip_prefix("BUILD_TIME_BUDGET_SECS=")
            .unwrap()
            .parse()
            .unwrap();
        (secs, String::from_utf8(out.stdout).unwrap())
    };
    // Each listed model gets its own, a trailing space in cpuinfo or not.
    for (cpu, secs) in &budgets {
        let (got, stdout) = pick(cpu);
        assert_eq!(got, *secs, "{cpu}");
        assert!(!stdout.contains("::notice"), "{stdout}");
        assert_eq!(pick(&format!("{cpu}  ")).0, *secs, "{cpu}, padded");
    }
    // A model not listed is held to the largest, and says so.
    let (got, stdout) = pick("Some Future CPU @ 9.99GHz");
    assert_eq!(got, largest);
    assert!(
        stdout.contains(
            "::notice title=build-time::no budget for Some Future CPU @ 9.99GHz in BUILD_TIME_BUDGETS"
        ),
        "{stdout}"
    );
}
