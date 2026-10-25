//! P0-BENCH-02: the self-hosted benchmark runners and the nightly job.
//!
//! The workflows are read as text, as `ci_workflow.rs` reads ci.yml: jobs are
//! the keys indented by two spaces under `jobs:`, job-level keys by four. The
//! rule that keeps pull requests off the self-hosted machines is checked on
//! every workflow under `.github/workflows/`. The probe, the offline-issue and
//! the publish steps of nightly-bench.yml run for real: the first two against a
//! stand-in for `gh` that answers from files, the last against a throwaway
//! origin repository. They need bash, jq and git on PATH, as the runners have
//! them. The unit tests of bench/compare.py run under the Python that
//! `bench/run.sh` picks. These tests never run the duscope binary.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const NIGHTLY: &str = ".github/workflows/nightly-bench.yml";
const RUNNERS_DOC: &str = "docs/runners.md";

/// The bench labels and the job that runs on each.
const BENCH_JOBS: &[(&str, &str)] = &[("bench-macos", "macos"), ("bench-linux", "linux")];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn read(relative: &str) -> String {
    let path = root().join(relative);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Every workflow file, by file name.
fn workflows() -> Vec<(String, String)> {
    let dir = root().join(".github/workflows");
    let mut files: Vec<(String, String)> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "yml" || x == "yaml"))
        .map(|p| {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            (name, fs::read_to_string(&p).unwrap())
        })
        .collect();
    files.sort();
    files
}

struct Job {
    id: String,
    lines: Vec<String>,
}

impl Job {
    /// The value of a job-level key (`    key: value`), if the job sets it.
    fn key(&self, key: &str) -> Option<&str> {
        let prefix = format!("    {key}:");
        self.lines
            .iter()
            .find_map(|line| line.strip_prefix(&prefix))
            .map(str::trim)
    }

    /// The entries of a job-level block (`    key:` and the deeper lines below
    /// it), comments dropped and list dashes removed.
    fn block(&self, key: &str) -> Vec<&str> {
        let head = format!("    {key}:");
        self.lines
            .iter()
            .skip_while(|line| line.trim_end() != head)
            .skip(1)
            .take_while(|line| line.starts_with("      ") || line.trim().is_empty())
            .map(|line| line.trim().trim_start_matches("- "))
            .filter(|entry| !entry.is_empty() && !entry.starts_with('#'))
            .collect()
    }

    /// Where the job runs: its `runs-on` value or block, and when that is an
    /// expression, its `strategy` block too, which holds the values.
    fn runs_on(&self) -> String {
        let mut target = match self.key("runs-on") {
            Some("") | None => self.block("runs-on").join(" "),
            Some(value) => value.to_owned(),
        };
        if target.contains("${{") {
            target.push(' ');
            target.push_str(&self.block("strategy").join(" "));
        }
        target
    }

    fn self_hosted(&self) -> bool {
        let target = self.runs_on();
        target.contains("self-hosted") || target.contains("bench-")
    }

    /// The `run: |` script of the step with this name, dedented.
    fn step_script(&self, name: &str) -> String {
        let mut lines = self
            .lines
            .iter()
            .skip_while(|line| line.trim() != format!("- name: {name}"))
            .skip(1);
        let run = lines
            .by_ref()
            .find(|line| line.trim() == "run: |")
            .unwrap_or_else(|| panic!("job `{}`: no `run: |` in step `{name}`", self.id));
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

fn job<'a>(jobs: &'a [Job], id: &str) -> &'a Job {
    jobs.iter()
        .find(|j| j.id == id)
        .unwrap_or_else(|| panic!("no job `{id}`"))
}

/// The events that trigger a workflow, in any of the `on:` forms.
fn triggers(workflow: &str) -> Vec<String> {
    let mut lines = workflow.lines().skip_while(|l| !l.starts_with("on:"));
    let Some(head) = lines.next() else {
        return Vec::new();
    };
    let inline = head["on:".len()..].trim();
    if !inline.is_empty() {
        return inline
            .trim_matches(['[', ']'])
            .split(',')
            .map(|t| t.trim().to_owned())
            .filter(|t| !t.is_empty())
            .collect();
    }
    lines
        .take_while(|l| l.starts_with(' ') || l.trim().is_empty())
        .filter_map(|l| l.strip_prefix("  "))
        .filter(|l| !l.starts_with([' ', '#', '-']) && !l.trim().is_empty())
        .map(|l| l.split(':').next().unwrap().trim().to_owned())
        .collect()
}

/// The events allowed to start a workflow that has a self-hosted job: the
/// runners serve scheduled and dispatched runs of main only.
const SELF_HOSTED_TRIGGERS: &[&str] = &["schedule", "workflow_dispatch"];

/// Every way a workflow lets a pull request reach a self-hosted runner.
fn violations(workflow: &str) -> Vec<String> {
    let triggers = triggers(workflow);
    let mut found = Vec::new();
    for job in jobs(workflow).iter().filter(|j| j.self_hosted()) {
        for trigger in &triggers {
            if !SELF_HOSTED_TRIGGERS.contains(&trigger.as_str()) {
                found.push(format!(
                    "job `{}` runs on `{}` in a workflow triggered by `{trigger}`",
                    job.id,
                    job.runs_on()
                ));
            }
        }
        if !job
            .key("if")
            .is_some_and(|c| c.contains("github.event_name != 'pull_request'"))
        {
            found.push(format!(
                "job `{}` runs on `{}` without `if: github.event_name != 'pull_request'`",
                job.id,
                job.runs_on()
            ));
        }
    }
    found
}

#[test]
fn p0_bench_02_no_pull_request_job_targets_a_self_hosted_label() {
    let mut self_hosted = Vec::new();
    for (name, workflow) in workflows() {
        let found = violations(&workflow);
        assert!(found.is_empty(), "{name}:\n{}", found.join("\n"));
        self_hosted.extend(
            jobs(&workflow)
                .into_iter()
                .filter(Job::self_hosted)
                .map(|j| format!("{name}:{}", j.id)),
        );
    }
    assert_eq!(
        self_hosted,
        [
            "nightly-bench.yml:bench-macos",
            "nightly-bench.yml:bench-linux"
        ],
        "the jobs on the self-hosted runners"
    );
}

#[test]
fn p0_bench_02_the_self_hosted_rule_classifies_workflows() {
    let job = |runs_on: &str, condition: &str| {
        format!("jobs:\n  x:\n    runs-on: {runs_on}\n{condition}    steps: []\n")
    };
    let guard = "    if: github.event_name != 'pull_request'\n";
    let ok = [
        format!(
            "on:\n  schedule:\n    - cron: \"0 3 * * *\"\n{}",
            job("[self-hosted, bench-linux]", guard)
        ),
        format!("on:\n  pull_request:\n{}", job("ubuntu-24.04", "")),
        format!("on: [pull_request, push]\n{}", job("ubuntu-24.04", "")),
        format!(
            "on:\n  workflow_dispatch:\n{}",
            job("[self-hosted, bench-macos]", guard)
        ),
    ];
    for workflow in &ok {
        assert_eq!(violations(workflow), Vec::<String>::new(), "{workflow}");
    }
    let bad = [
        // A pull request trigger, guarded or not.
        format!("on:\n  pull_request:\n{}", job("[self-hosted, bench-linux]", guard)),
        format!("on: [pull_request, schedule]\n{}", job("[self-hosted]", guard)),
        // pull_request_target and workflow_run pass the guard: their event name
        // is not `pull_request`.
        format!("on:\n  pull_request_target:\n{}", job("[self-hosted, bench-macos]", guard)),
        format!("on:\n  workflow_run:\n    workflows: [CI]\n{}", job("bench-linux", guard)),
        // The guard is missing.
        format!("on:\n  schedule:\n    - cron: \"0 3 * * *\"\n{}", job("[self-hosted, bench-linux]", "")),
        // The label hides in a matrix.
        "on:\n  pull_request:\njobs:\n  x:\n    strategy:\n      matrix:\n        runner: [ubuntu-24.04, bench-macos]\n    runs-on: ${{ matrix.runner }}\n    steps: []\n"
            .to_owned(),
    ];
    for workflow in &bad {
        assert!(!violations(workflow).is_empty(), "not flagged:\n{workflow}");
    }
}

#[test]
fn p0_bench_02_secrets_stay_off_the_self_hosted_runners() {
    for (name, workflow) in workflows() {
        for job in jobs(&workflow).iter().filter(|j| j.self_hosted()) {
            for line in &job.lines {
                let code = line.split(" #").next().unwrap();
                assert!(
                    !code.contains("secrets.") && !code.contains("github.token"),
                    "{name}: job `{}` on a self-hosted runner reads a token: {}",
                    job.id,
                    line.trim()
                );
                assert!(
                    !code.trim_end().ends_with(": write"),
                    "{name}: job `{}` on a self-hosted runner widens the token: {}",
                    job.id,
                    line.trim()
                );
            }
        }
    }
}

#[test]
fn p0_bench_02_nightly_runs_at_three_pacific_on_main_only() {
    let workflow = read(NIGHTLY);
    assert_eq!(triggers(&workflow), SELF_HOSTED_TRIGGERS);
    assert!(
        workflow.contains("    - cron: \"0 3 * * *\"\n      timezone: \"America/Los_Angeles\"\n"),
        "nightly-bench.yml runs at 03:00 Pacific, daylight saving included"
    );
    let jobs = jobs(&workflow);
    let probe = job(&jobs, "probe");
    assert!(
        probe
            .key("if")
            .is_some_and(|c| c.contains("github.ref == 'refs/heads/main'")),
        "only main's copy of the workflow reaches the runners"
    );
    assert!(
        !probe.self_hosted(),
        "the probe runs on a GitHub-hosted runner"
    );
    for (label, platform) in BENCH_JOBS {
        let bench = job(&jobs, label);
        assert_eq!(
            bench.key("needs"),
            Some("probe"),
            "{label} waits for the probe"
        );
        assert_eq!(
            bench.key("runs-on"),
            Some(format!("[self-hosted, {label}]").as_str())
        );
        assert!(
            bench
                .key("if")
                .is_some_and(|c| c.contains(&format!("needs.probe.outputs.{platform} == 'online'"))),
            "{label} runs only when the probe saw its runner online"
        );
        let script = bench.step_script("Run the harness");
        assert!(
            script.contains("bench/run.sh run --subject du "),
            "{label} runs the harness with du as the subject"
        );
    }
    let publish = job(&jobs, "publish");
    assert!(
        !publish.self_hosted(),
        "the results are pushed from a GitHub-hosted runner"
    );
    for job in &jobs {
        assert!(
            job.key("timeout-minutes")
                .is_some_and(|m| m.parse::<u32>().is_ok()),
            "job `{}` has no timeout-minutes",
            job.id
        );
    }
    for (n, line) in workflow.lines().enumerate() {
        let Some((_, rest)) = line.split_once("uses:") else {
            continue;
        };
        let action = rest.split_whitespace().next().unwrap_or_default();
        let sha = action
            .split_once('@')
            .map(|(_, sha)| sha)
            .unwrap_or_default();
        assert!(
            sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit()) && rest.contains("# v"),
            "nightly-bench.yml:{}: `{action}` is not pinned to a commit SHA with a version comment",
            n + 1
        );
    }
}

/// A scratch directory with a stand-in for `gh` on PATH, for running a step
/// of nightly-bench.yml as GitHub runs it (`bash --noprofile --norc -eo
/// pipefail`).
struct Step {
    _temp: tempfile::TempDir,
    dir: PathBuf,
}

/// Logs each call's arguments to `$STUB_DIR/gh.log` and answers `api` and
/// `issue list` from the JSON files in `$STUB_DIR`, applying `--jq` as gh does.
const GH_STUB: &str = r#"#!/usr/bin/env bash
printf '%s\n' "$*" >> "$STUB_DIR/gh.log"
filter=""
prev=""
for arg in "$@"; do
  if [[ $prev == --jq ]]; then filter=$arg; fi
  prev=$arg
done
answer() {
  if [[ ! -f $STUB_DIR/$1 ]]; then echo "gh stub: no answer $1" >&2; exit 1; fi
  if [[ -n $filter ]]; then jq -rc "$filter" "$STUB_DIR/$1"; else cat "$STUB_DIR/$1"; fi
}
case "$1 $2" in
  "api --paginate") answer runners.json ;;
  "issue list") answer issues.json ;;
  "issue create") echo "https://github.com/kawadeomkar/duscope/issues/7" ;;
  "issue comment"|"issue close"|"issue pin") ;;
  api\ *)
    case $2 in
      */actions/workflows/*) answer runs.json ;;
      */jobs) id=${2%/jobs}; answer "jobs-${id##*/}.json" ;;
      *) echo "gh stub: unexpected endpoint $2" >&2; exit 1 ;;
    esac ;;
  *) echo "gh stub: unexpected call: $*" >&2; exit 1 ;;
esac
"#;

impl Step {
    fn new() -> Step {
        let temp = tempfile::tempdir().unwrap();
        let dir = fs::canonicalize(temp.path()).unwrap();
        let bin = dir.join("bin");
        fs::create_dir_all(&bin).unwrap();
        fs::write(bin.join("gh"), GH_STUB).unwrap();
        let mut mode = fs::metadata(bin.join("gh")).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut mode, 0o755);
        fs::set_permissions(bin.join("gh"), mode).unwrap();
        fs::create_dir_all(dir.join("runner-temp")).unwrap();
        Step { _temp: temp, dir }
    }

    fn answer(&self, name: &str, json: &str) {
        fs::write(self.dir.join(name), json).unwrap();
    }

    /// Runs `script` in `cwd` with the runner's variables and `vars`.
    fn run(&self, script: &str, cwd: &Path, vars: &[(&str, &str)]) -> Output {
        let path = self.dir.join("step.sh");
        fs::write(&path, script).unwrap();
        let _ = fs::remove_file(self.dir.join("output"));
        let mut search = vec![self.dir.join("bin")];
        search.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
        Command::new("bash")
            .args(["--noprofile", "--norc", "-eo", "pipefail"])
            .arg(&path)
            .current_dir(cwd)
            .env("PATH", std::env::join_paths(search).unwrap())
            .env("STUB_DIR", &self.dir)
            .env("GH_TOKEN", "stub")
            .env("GITHUB_REPOSITORY", "kawadeomkar/duscope")
            .env("GITHUB_SERVER_URL", "https://github.com")
            .env("GITHUB_RUN_ID", "100")
            .env("GITHUB_OUTPUT", self.dir.join("output"))
            .env("RUNNER_TEMP", self.dir.join("runner-temp"))
            .envs(vars.iter().copied())
            .output()
            .unwrap()
    }

    /// The step's `$GITHUB_OUTPUT` as key-value pairs.
    fn outputs(&self) -> BTreeMap<String, String> {
        fs::read_to_string(self.dir.join("output"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.split_once('='))
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect()
    }

    /// The `gh` calls that change something: issues opened, commented on,
    /// closed or pinned.
    fn writes(&self) -> Vec<String> {
        fs::read_to_string(self.dir.join("gh.log"))
            .unwrap_or_default()
            .lines()
            .filter(|l| {
                ["issue create", "issue comment", "issue close", "issue pin"]
                    .iter()
                    .any(|w| l.starts_with(w))
            })
            .map(|l| l.split(" --").next().unwrap().to_owned())
            .collect()
    }
}

fn succeeded(out: &Output, what: &str) -> String {
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "{what} failed ({}):\nstdout: {stdout}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    stdout
}

fn runner(status: &str, label: &str) -> String {
    format!(
        r#"{{"status": "{status}", "labels": [{{"name": "self-hosted"}}, {{"name": "{label}"}}]}}"#
    )
}

#[test]
fn p0_bench_02_an_offline_runner_is_skipped_with_a_notice() {
    let script = job(&jobs(&read(NIGHTLY)), "probe").step_script("Which bench runners are online");
    let cases: &[(Option<Vec<String>>, &str, &str)] = &[
        (
            Some(vec![
                runner("online", "bench-macos"),
                runner("online", "bench-linux"),
            ]),
            "online",
            "online",
        ),
        (
            Some(vec![
                runner("online", "bench-macos"),
                runner("offline", "bench-linux"),
            ]),
            "online",
            "offline",
        ),
        // A second runner with the label counts when it is online.
        (
            Some(vec![
                runner("offline", "bench-macos"),
                runner("online", "bench-macos"),
                runner("online", "bench-linux"),
            ]),
            "online",
            "online",
        ),
        (
            Some(vec![runner("offline", "bench-macos")]),
            "offline",
            "missing",
        ),
        // The API cannot be read: no token, or one without Administration read.
        (None, "unknown", "unknown"),
    ];
    for (runners, macos, linux) in cases {
        let step = Step::new();
        if let Some(runners) = runners {
            step.answer(
                "runners.json",
                &format!(r#"{{"runners": [{}]}}"#, runners.join(", ")),
            );
        }
        let stdout = succeeded(&step.run(&script, &step.dir, &[]), "the probe");
        let outputs = step.outputs();
        let want = BTreeMap::from([
            ("linux".to_owned(), (*linux).to_owned()),
            ("macos".to_owned(), (*macos).to_owned()),
        ]);
        assert_eq!(outputs, want, "{runners:?}\n{stdout}");
        let notices = stdout.matches("::notice").count();
        let expected = match (*macos, *linux) {
            ("unknown", _) => 1,
            _ => [*macos, *linux].iter().filter(|s| **s != "online").count(),
        };
        assert_eq!(notices, expected, "{runners:?}\n{stdout}");
    }
}

#[test]
fn p0_bench_02_two_consecutive_skips_open_the_offline_issue() {
    let script =
        job(&jobs(&read(NIGHTLY)), "probe").step_script("Open, update or close the offline issue");
    let open_issue = r#"[{"number": 7, "title": "nightly-bench offline"}, {"number": 3, "title": "nightly-bench offline, old"}]"#;
    let runs = r#"{"workflow_runs": [{"id": 100, "status": "in_progress"}, {"id": 99, "status": "completed"}, {"id": 98, "status": "completed"}]}"#;
    let jobs99 = |linux: &str| {
        format!(
            r#"{{"jobs": [{{"name": "probe", "conclusion": "success"}}, {{"name": "bench-macos", "conclusion": "success"}}, {{"name": "bench-linux", "conclusion": "{linux}"}}]}}"#
        )
    };
    // (Linux now, its job in run 99, an open issue?, the writes expected)
    let cases: &[(&str, Option<&str>, bool, &[&str])] = &[
        // A first skip opens nothing.
        ("offline", Some("success"), false, &[]),
        // There is no earlier run.
        ("offline", None, false, &[]),
        // The second skip in a row opens and pins the issue...
        (
            "offline",
            Some("skipped"),
            false,
            &[
                "issue create",
                "issue pin https://github.com/kawadeomkar/duscope/issues/7",
            ],
        ),
        // ...or comments on it when it is open.
        ("missing", Some("skipped"), true, &["issue comment 7"]),
        // Both runners online: the open issue is closed.
        (
            "online",
            Some("skipped"),
            true,
            &["issue comment 7", "issue close 7"],
        ),
        ("online", Some("success"), false, &[]),
    ];
    for (linux, before, open, want) in cases {
        let step = Step::new();
        step.answer("issues.json", if *open { open_issue } else { "[]" });
        match before {
            Some(conclusion) => {
                step.answer("runs.json", runs);
                step.answer("jobs-99.json", &jobs99(conclusion));
            }
            None => step.answer(
                "runs.json",
                r#"{"workflow_runs": [{"id": 100, "status": "in_progress"}]}"#,
            ),
        }
        let out = step.run(&script, &step.dir, &[("MACOS", "online"), ("LINUX", linux)]);
        let stdout = succeeded(&out, "the offline-issue step");
        assert_eq!(
            step.writes(),
            *want,
            "linux {linux}, before {before:?}, open {open}\n{stdout}"
        );
    }
    // The issue it opens is titled as the spec names it and links the run.
    let step = Step::new();
    step.answer("issues.json", "[]");
    step.answer("runs.json", runs);
    step.answer("jobs-99.json", &jobs99("skipped"));
    succeeded(
        &step.run(
            &script,
            &step.dir,
            &[("MACOS", "online"), ("LINUX", "offline")],
        ),
        "the offline-issue step",
    );
    let log = fs::read_to_string(step.dir.join("gh.log")).unwrap();
    let create = log.lines().find(|l| l.starts_with("issue create")).unwrap();
    assert!(create.contains("--title nightly-bench offline"), "{create}");
    assert!(create.contains("bench-linux"), "{create}");
    assert!(
        log.contains("https://github.com/kawadeomkar/duscope/actions/runs/100"),
        "{log}"
    );
}

/// git with a private configuration and no automatic maintenance.
fn git(step: &Step, cwd: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_CONFIG_GLOBAL", step.dir.join("gitconfig"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("HOME", &step.dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// A throwaway origin repository with a `main` branch, for running the publish
/// step against.
struct Origin {
    step: Step,
    script: String,
    url: String,
}

impl Origin {
    fn new() -> Origin {
        let script = job(&jobs(&read(NIGHTLY)), "publish")
            .step_script("Commit the result files to bench-results");
        let step = Step::new();
        fs::write(
            step.dir.join("gitconfig"),
            "[user]\n\tname = Bench Test\n\temail = bench-test@example.invalid\n\
             [init]\n\tdefaultBranch = main\n[advice]\n\tdetachedHead = false\n\
             [maintenance]\n\tauto = false\n[receive]\n\tautogc = false\n",
        )
        .unwrap();
        let origin = step.dir.join("origin.git");
        let url = format!("file://{}", origin.display());
        git(
            &step,
            &step.dir,
            &["init", "-q", "--bare", origin.to_str().unwrap()],
        );
        let work = step.dir.join("work");
        git(&step, &step.dir, &["init", "-q", work.to_str().unwrap()]);
        fs::write(work.join("README.md"), "main\n").unwrap();
        git(&step, &work, &["add", "-A"]);
        git(&step, &work, &["commit", "-q", "-m", "main"]);
        git(&step, &work, &["push", "-q", &url, "main"]);
        Origin { step, script, url }
    }

    /// A git command on the origin repository.
    fn query(&self, what: &[&str]) -> String {
        let git_dir = self.step.dir.join("origin.git");
        let mut args = vec!["--git-dir", git_dir.to_str().unwrap()];
        args.extend_from_slice(what);
        git(&self.step, &self.step.dir, &args)
    }

    /// Runs the step with these result files downloaded, in a fresh checkout of
    /// main as actions/checkout leaves it; returns its stdout.
    fn publish(&self, files: &[(&str, &str)]) -> String {
        let dir = &self.step.dir;
        let results = dir.join("runner-temp/results");
        let _ = fs::remove_dir_all(&results);
        fs::create_dir_all(&results).unwrap();
        for (name, body) in files {
            fs::write(results.join(name), body).unwrap();
        }
        let checkout = dir.join("checkout");
        let _ = fs::remove_dir_all(&checkout);
        let to = checkout.to_str().unwrap();
        git(
            &self.step,
            dir,
            &["clone", "-q", "--depth", "1", &self.url, to],
        );
        let script = dir.join("publish.sh");
        fs::write(&script, &self.script).unwrap();
        let out = Command::new("bash")
            .args(["--noprofile", "--norc", "-eo", "pipefail"])
            .arg(&script)
            .current_dir(&checkout)
            .env("GIT_CONFIG_GLOBAL", dir.join("gitconfig"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", dir)
            .env("RUNNER_TEMP", dir.join("runner-temp"))
            .env("GITHUB_SERVER_URL", "https://github.com")
            .env("GITHUB_REPOSITORY", "kawadeomkar/duscope")
            .env("GITHUB_RUN_ID", "100")
            .output()
            .unwrap();
        succeeded(&out, "the publish step")
    }
}

#[test]
fn p0_bench_02_publish_commits_the_result_files_to_bench_results() {
    let origin = Origin::new();
    let main_before = origin.query(&["rev-parse", "main"]);

    // The first run creates the branch with its README and both files.
    origin.publish(&[
        ("2026-10-25-macos.json", "{\"m\": 1}\n"),
        ("2026-10-25-linux.json", "{\"l\": 1}\n"),
    ]);
    assert_eq!(
        origin.query(&["ls-tree", "--name-only", "bench-results"]),
        "2026-10-25-linux.json\n2026-10-25-macos.json\nREADME.md\n"
    );
    assert_eq!(
        origin.query(&["rev-list", "--count", "bench-results"]),
        "1\n",
        "a branch of its own history"
    );
    assert_eq!(
        origin.query(&["show", "bench-results:2026-10-25-macos.json"]),
        "{\"m\": 1}\n"
    );
    let message = origin.query(&["log", "-1", "--format=%B", "bench-results"]);
    assert!(message.contains("2026-10-25-macos.json"), "{message}");
    assert!(
        message.contains("https://github.com/kawadeomkar/duscope/actions/runs/100"),
        "{message}"
    );

    // The next night adds its file on top; a same-day rerun replaces one.
    origin.publish(&[("2026-10-26-macos.json", "{\"m\": 2}\n")]);
    origin.publish(&[("2026-10-26-macos.json", "{\"m\": 3}\n")]);
    assert_eq!(
        origin.query(&["ls-tree", "--name-only", "bench-results"]),
        "2026-10-25-linux.json\n2026-10-25-macos.json\n2026-10-26-macos.json\nREADME.md\n"
    );
    assert_eq!(
        origin.query(&["rev-list", "--count", "bench-results"]),
        "3\n"
    );
    assert_eq!(
        origin.query(&["show", "bench-results:2026-10-26-macos.json"]),
        "{\"m\": 3}\n"
    );

    // Publishing what is there already, or nothing, commits nothing.
    let stdout = origin.publish(&[("2026-10-26-macos.json", "{\"m\": 3}\n")]);
    assert!(stdout.contains("already on bench-results"), "{stdout}");
    let stdout = origin.publish(&[]);
    assert!(stdout.contains("no result file to publish"), "{stdout}");
    assert_eq!(
        origin.query(&["rev-list", "--count", "bench-results"]),
        "3\n"
    );
    assert_eq!(
        origin.query(&["rev-parse", "main"]),
        main_before,
        "main is never touched"
    );
}

#[test]
fn p0_bench_02_compare_unit_tests() {
    let python: OsString = std::env::var_os("PYTHON")
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| {
            if Path::new("/usr/bin/python3").is_file() {
                "/usr/bin/python3".into()
            } else {
                "python3".into()
            }
        });
    let out = Command::new(python)
        .args([
            "-B",
            "-E",
            "-s",
            "-m",
            "unittest",
            "discover",
            "-s",
            "bench",
            "-p",
            "test_compare.py",
        ])
        .current_dir(root())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "bench/test_compare.py:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn p0_bench_02_runners_doc_covers_the_spec_topics() {
    let doc = read(RUNNERS_DOC);
    for topic in [
        "## The machines",
        "## The runner user",
        "## Disks and filesystems",
        "## Network",
        "## Security",
        "## Updates",
        "## When a runner is offline",
        "## Re-registering a runner",
        "BENCH_RESULTS_TOKEN",
        "BENCH_HOME",
        "bench-macos",
        "bench-linux",
    ] {
        assert!(doc.contains(topic), "{RUNNERS_DOC} lacks `{topic}`");
    }
    // The sudo rules are bench/README.md's, which the cache drop relies on.
    let sudoers = |text: &str| -> Vec<String> {
        text.lines()
            .map(str::trim)
            .filter(|l| l.starts_with("runner ALL=(root) NOPASSWD:"))
            .map(str::to_owned)
            .collect()
    };
    let readme = sudoers(&read("bench/README.md"));
    assert_eq!(readme.len(), 2);
    assert_eq!(
        sudoers(&doc),
        readme,
        "{RUNNERS_DOC} and bench/README.md give different sudo rules"
    );
}
