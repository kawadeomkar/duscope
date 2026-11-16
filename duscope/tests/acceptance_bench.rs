//! P5-BENCH-01: the 1.0 acceptance run as one harness command, its
//! judgement against the goal-1 and goal-2 targets, and the off-Proxmox
//! cross-check of the Linux ratios (`linux-crosscheck.yml`, DEV-157).
//!
//! `bench/run.sh acceptance` runs whole against the binary this test run
//! built, on scaled trees, with `bench/testdata/hyperfine_stub.py` for
//! hyperfine, `true` for the cache drop and a stand-in for diskus that answers
//! as its pin: nothing touches the network, and its numbers are a smoke
//! tree's. The workflow is read as text, as `musl_loop.rs` reads its own, and
//! its comparison step runs for real under bash against result files the test
//! writes. The Python tests run under the interpreter `bench/run.sh` picks.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use duscope_testkit::TestEnv;
use serde_json::{Value, json};

const WORKFLOW: &str = ".github/workflows/linux-crosscheck.yml";
const SCALE: &str = "0.0002";

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

/// The interpreter `bench/run.sh` picks: `$PYTHON`, else `/usr/bin/python3`,
/// else `python3`.
fn python() -> String {
    std::env::var("PYTHON")
        .ok()
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| {
            if Path::new("/usr/bin/python3").is_file() {
                "/usr/bin/python3".to_owned()
            } else {
                "python3".to_owned()
            }
        })
}

fn script(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn platform() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    }
}

/// P5-BENCH-01: the unit tests of the acceptance run, the rescans, the
/// judgement, the summary and the cross-check's comparison.
#[test]
fn p5_bench_01_python_unit_tests() {
    for pattern in ["test_acceptance.py", "test_crosscheck.py"] {
        let out = Command::new(python())
            .args([
                "-B", "-E", "-s", "-m", "unittest", "discover", "-s", "bench", "-p",
            ])
            .arg(pattern)
            .current_dir(root())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "bench/{pattern}:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// `bench/run.sh acceptance` on the scaled trees under `scratch`, with a
/// stand-in diskus as the one competitor.
fn acceptance(env: &TestEnv, scratch: &Path) -> Output {
    let diskus = scratch.join("diskus");
    script(
        &diskus,
        "#!/bin/sh\n[ \"$1\" = --version ] && { echo 'diskus 0.9.0'; exit 0; }\nexec du -sk \"$1\" > /dev/null\n",
    );
    let stub = root().join("bench/testdata/hyperfine_stub.py");
    env.command(root().join("bench/run.sh"))
        .arg("acceptance")
        .arg("--duscope")
        .arg(env.cmd().get_program())
        .args(["--platform", platform(), "--scale", SCALE, "--jobs", "1"])
        .args(["--competitors", "diskus", "--cold-cmd", "true"])
        .arg("--tool-path")
        .arg(format!("diskus={}", diskus.display()))
        .args(["--runs", "2", "--warmup", "0", "--index-runs", "2"])
        .arg("--hyperfine")
        .arg(format!("{} {}", python(), stub.display()))
        .arg("--trees-dir")
        .arg(scratch.join("trees"))
        .arg("--tools-dir")
        .arg(scratch.join("tools"))
        .arg("--out-dir")
        .arg(scratch.join("out"))
        .output()
        .unwrap()
}

/// The rescan part of an acceptance file of this platform: each tree measured
/// or not with a reason, and every failure a rescan that read less than its
/// change, which only a busy machine's slow history makes (macOS).
fn check_rescans(result: &Value) {
    let rescans = result["rescan"]["trees"].as_array().unwrap();
    let names: Vec<&str> = rescans
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["T1", "T2", "T3"]);
    for tree in rescans {
        let measured = tree["status"] == "measured";
        assert!(
            measured || tree["reason"].as_str().is_some_and(|r| !r.is_empty()),
            "{tree}"
        );
        if platform() == "linux" {
            assert!(!measured, "{tree}");
            assert!(
                tree["reason"]
                    .as_str()
                    .unwrap()
                    .contains("keeps no change history"),
                "{tree}"
            );
            assert_eq!(tree["rounds"], 0, "{tree}");
        }
    }
    assert_eq!(rescans[2]["status"], "not measured", "{}", rescans[2]);
    for failure in result["failures"].as_array().unwrap() {
        let failure = failure.as_str().unwrap();
        assert!(
            platform() == "macos" && failure.contains("raise --settle"),
            "{failure}"
        );
    }
}

/// P5-BENCH-01: one command measures the harness's matrix, the index on T2
/// and the incremental rescans against full ones, and writes them in one
/// file named for this platform (a scaled run's name says so), whose
/// targets it judges as it ends. Linux keeps no change history: each tree
/// says so in the binary's words. T3's one directory is every directory it
/// has, so no platform reads it incrementally. The trees match their stamps
/// after the rescans' changes.
#[test]
fn p5_bench_01_the_acceptance_run_writes_one_file_for_this_platform() {
    let env = TestEnv::new();
    let scratch = env.tmp_dir();
    let out = acceptance(&env, &scratch);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let file = scratch.join(format!("out/{}-x{SCALE}.json", platform()));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        file.to_str().unwrap(),
        "{stderr}"
    );
    let result: Value = serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(result["kind"], "acceptance");
    assert_eq!(result["platform"], platform());
    // The harness: duscope paired with diskus on every tree, warm and cold.
    let run = &result["run"];
    assert_eq!(run["subject"], "duscope");
    let mut pairs: Vec<String> = run["results"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["paired_with"] == "duscope")
        .map(|r| format!("{} {} {}", r["tree"], r["mode"], r["tool"]).replace('"', ""))
        .collect();
    pairs.sort();
    assert_eq!(
        pairs,
        [
            "T1 cold diskus",
            "T1 warm diskus",
            "T2 cold diskus",
            "T2 warm diskus",
            "T3 cold diskus",
            "T3 warm diskus"
        ]
    );
    // The index measurements, on T2, the tree the gates read.
    assert_eq!(result["index"]["kind"], "index");
    assert_eq!(result["index"]["gates_tree"], "T2");
    check_rescans(&result);
    assert_eq!(
        out.status.success(),
        result["failures"].as_array().unwrap().is_empty(),
        "{stderr}"
    );
    // The run judges its own platform's targets as it ends.
    let target = if platform() == "linux" {
        "bench: goal 1: Linux warm p50 <= 1.2x diskus"
    } else {
        "bench: goal 1: macOS warm p50 <= 1.0x diskus"
    };
    assert!(stderr.contains(target), "{stderr}");
    assert!(
        stderr.contains("not the acceptance run"),
        "a smoke run says so: {stderr}"
    );
    let verified = env
        .command(root().join("bench/run.sh"))
        .args(["verify", "--trees", "all", "--scale", SCALE, "--trees-dir"])
        .arg(scratch.join("trees"))
        .output()
        .unwrap();
    assert!(
        verified.status.success(),
        "{}",
        String::from_utf8_lossy(&verified.stderr)
    );
    // `judge` reads the file back; the other platform has none.
    let judged = env
        .command(root().join("bench/run.sh"))
        .arg("judge")
        .arg(&file)
        .output()
        .unwrap();
    assert_eq!(judged.status.code(), Some(1));
    let other = if platform() == "linux" {
        "macos"
    } else {
        "linux"
    };
    assert!(
        String::from_utf8_lossy(&judged.stdout).contains(&format!("{other}: no result file")),
        "{judged:?}"
    );
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

    /// The steps of the job: each one's lines, from its `- ` on.
    fn steps(&self) -> Vec<Vec<&str>> {
        let mut steps: Vec<Vec<&str>> = Vec::new();
        for line in self.lines.iter().skip_while(|line| *line != "    steps:") {
            if line.starts_with("      - ") {
                steps.push(vec![line.as_str()]);
            } else if let Some(step) = steps.last_mut() {
                step.push(line.as_str());
            }
        }
        steps
    }

    /// The step with this name, as one text.
    fn step(&self, name: &str) -> String {
        self.steps()
            .into_iter()
            .find(|step| {
                step.iter().any(|line| {
                    line.trim() == format!("name: {name}")
                        || line.trim() == format!("- name: {name}")
                })
            })
            .unwrap_or_else(|| panic!("job `{}` has no step `{name}`", self.id))
            .join("\n")
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

/// The lines of a top-level block (`key:` and the indented lines below it),
/// comments and blank lines dropped.
fn block<'a>(workflow: &'a str, key: &str) -> Vec<&'a str> {
    workflow
        .lines()
        .skip_while(|line| *line != format!("{key}:"))
        .skip(1)
        .take_while(|line| line.starts_with(' ') || line.trim().is_empty())
        .filter(|line| !line.trim().is_empty() && !line.trim_start().starts_with('#'))
        .collect()
}

/// The value of a top-level `NAME: "value"` line under the workflow's `env:`.
fn workflow_env(workflow: &str, name: &str) -> Option<String> {
    block(workflow, "env")
        .into_iter()
        .find_map(|line| line.trim().strip_prefix(&format!("{name}:")))
        .map(|value| value.trim().trim_matches('"').to_owned())
}

/// P5-BENCH-01 (DEV-157): the cross-check runs on a dispatch and nothing
/// else, on GitHub's hosted ubuntu-24.04 and never a self-hosted label, so no
/// pull request, push or schedule starts it and its job is no required check;
/// it reads no secret, widens no token, and two dispatches never overlap.
#[test]
fn p5_bench_01_the_crosscheck_runs_on_a_dispatch_alone_on_a_hosted_runner() {
    let workflow = read(WORKFLOW);
    let on = block(&workflow, "on");
    let triggers: Vec<&str> = on
        .iter()
        .filter_map(|line| line.strip_prefix("  "))
        .filter(|rest| !rest.starts_with(' '))
        .collect();
    assert_eq!(triggers, ["workflow_dispatch:"], "{on:?}");
    for event in [
        "push",
        "pull_request",
        "schedule",
        "workflow_run",
        "workflow_call",
    ] {
        assert!(
            !workflow.contains(&format!("\n  {event}:"))
                && !workflow.contains(&format!("on: [{event}")),
            "linux-crosscheck.yml is triggered by {event}"
        );
    }
    let input = |name: &str| -> Vec<String> {
        on.iter()
            .skip_while(|line| line.trim_end() != format!("      {name}:"))
            .skip(1)
            .take_while(|line| line.starts_with("        "))
            .map(|line| line.trim().to_owned())
            .collect()
    };
    for (name, default) in [("trees", "T1,T2"), ("mode", "both"), ("scale", "1")] {
        assert!(
            input(name).contains(&format!("default: \"{default}\"")),
            "{name}: {on:?}"
        );
    }
    assert_eq!(
        block(&workflow, "concurrency"),
        ["  group: linux-crosscheck", "  cancel-in-progress: false"]
    );
    assert_eq!(block(&workflow, "permissions"), ["  contents: read"]);
    assert!(
        !workflow.contains("secrets."),
        "linux-crosscheck.yml reads a secret"
    );
    assert!(
        !workflow.contains(": write")
            && !workflow.contains("write-all")
            && !workflow.contains("github.token"),
        "linux-crosscheck.yml widens or hands out the token"
    );
    let jobs = jobs(&workflow);
    let ids: Vec<&str> = jobs.iter().map(|job| job.id.as_str()).collect();
    assert_eq!(ids, ["crosscheck"]);
    let job = &jobs[0];
    assert_eq!(job.key("runs-on"), Some("ubuntu-24.04"));
    assert!(!workflow.contains("self-hosted") && !workflow.contains("bench-linux]"));
    let minutes: u32 = job.key("timeout-minutes").unwrap().parse().unwrap();
    assert!((1..=360).contains(&minutes), "{minutes} minutes");
    // No job of it reports a check that branch protection requires.
    let required: Vec<String> = read("docs/github-settings.md")
        .lines()
        .filter_map(|line| line.split_once("\"context\": \"").map(|(_, rest)| rest))
        .filter_map(|rest| rest.split_once('"').map(|(context, _)| context.to_owned()))
        .collect();
    assert!(
        !required.is_empty(),
        "no required checks in docs/github-settings.md"
    );
    let name = job.key("name").unwrap();
    assert!(
        required
            .iter()
            .all(|check| check.split(" (").next().unwrap() != name),
        "the job reports a check named like a required one: {name}"
    );
    assert_pinned_and_tokenless(&workflow, job);
}

/// The workflow's actions are pinned to full commit SHAs with a version
/// comment, as everywhere else, and no checkout keeps the token.
fn assert_pinned_and_tokenless(workflow: &str, job: &Job) {
    for line in workflow.lines().filter(|line| line.contains("uses:")) {
        let action = line
            .split("uses:")
            .nth(1)
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap();
        let sha = action
            .split_once('@')
            .map(|(_, sha)| sha)
            .unwrap_or_default();
        assert!(
            sha.len() == 40
                && sha
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
            "`{action}` is not pinned to a full commit SHA"
        );
        assert!(line.contains("# v"), "`{action}` has no version comment");
    }
    for step in job
        .steps()
        .iter()
        .filter(|step| step.iter().any(|l| l.contains("actions/checkout@")))
    {
        assert!(
            step.iter()
                .any(|l| l.trim() == "persist-credentials: false"),
            "{step:?}"
        );
    }
}

/// P5-BENCH-01 (DEV-157): the cross-check measures what bench-linux's
/// nightly measures, the way it does (the `x86_64` musl build, the harness's
/// command line with every pinned competitor, the same hyperfine), records the
/// runner's CPU model, keeps the result whatever happened, and compares its
/// ratios with the nightly results on `bench-results`.
#[test]
fn p5_bench_01_the_crosscheck_measures_as_bench_linux_does_and_compares_ratios() {
    let workflow = read(WORKFLOW);
    let nightly = read(".github/workflows/nightly-bench.yml");
    let job = &jobs(&workflow)[0];
    let pinned = read("justfile")
        .lines()
        .find_map(|l| l.strip_prefix("hyperfine_version := "))
        .map(|v| v.trim().trim_matches('"').to_owned());
    assert_eq!(workflow_env(&workflow, "HYPERFINE_VERSION"), pinned);
    assert_eq!(
        workflow_env(&read(".github/workflows/ci.yml"), "HYPERFINE_VERSION"),
        pinned
    );
    assert!(
        job.steps().iter().any(|step| step
            .iter()
            .any(|l| l.trim() == "tool: hyperfine@${{ env.HYPERFINE_VERSION }}")),
        "hyperfine is not installed at its pin"
    );
    // The build bench-linux's nightly measures: release, locked, musl.
    let build = job.step_script("Build duscope");
    assert!(
        build.contains(
            "cargo build --release --locked -p duscope --target x86_64-unknown-linux-musl"
        ),
        "{build}"
    );
    let linux = jobs(&nightly)
        .into_iter()
        .find(|j| j.id == "bench-linux")
        .unwrap();
    let theirs = linux.step_script("Build duscope");
    assert!(
        theirs.contains("target=\"$(uname -m)-unknown-linux-musl\""),
        "{theirs}"
    );
    assert!(
        theirs.contains("cargo build --release --locked -p duscope --target \"$target\""),
        "{theirs}"
    );
    // The runner's CPU model, beside the result.
    let runner = job.step_script("The runner");
    assert!(
        runner.contains("sed -n 's/^model name[[:space:]]*: //p' /proc/cpuinfo"),
        "{runner}"
    );
    assert!(
        runner.contains(
            "tee \"$RUNNER_TEMP/crosscheck/runner.txt\" | tee -a \"$GITHUB_STEP_SUMMARY\""
        ),
        "{runner}"
    );
    // The harness's command line, as the nightly gives it.
    let harness = job.step_script("Run the harness");
    for part in [
        "bench/run.sh run --subject duscope --tool-path \"duscope=$PWD/target/x86_64-unknown-linux-musl/release/duscope\"",
        "--trees \"$TREES\" --competitors all",
        "--mode \"$MODE\" --scale \"$SCALE\"",
        "--out \"$file\"",
    ] {
        assert!(harness.contains(part), "{part}: {harness}");
    }
    let theirs = linux.step_script("Run the harness");
    for part in [
        "bench/run.sh run --subject duscope --tool-path \"duscope=$DUSCOPE_BIN\"",
        "--trees \"$TREES\" --competitors \"$COMPETITORS\"",
        "--mode \"$MODE\" --scale \"$SCALE\"",
    ] {
        assert!(theirs.contains(part), "{part}: {theirs}");
    }
    // The nightly results, read from their branch with a read-only token.
    let results = job.step("Check out bench-linux's nightly results");
    for part in [
        "ref: bench-results",
        "path: bench-results",
        "persist-credentials: false",
    ] {
        assert!(results.contains(part), "{part}: {results}");
    }
    let compare = job.step_script("Compare with bench-linux's latest nightly");
    assert!(
        compare.contains("bench/run.sh crosscheck \"$FILE\" bench-results"),
        "{compare}"
    );
    // Kept on a failure and a timeout too.
    let upload = job
        .steps()
        .into_iter()
        .find(|step| step[0].contains("actions/upload-artifact@"))
        .expect("linux-crosscheck keeps no result")
        .join("\n");
    for part in [
        "if: ${{ always() }}",
        "name: linux-crosscheck",
        "path: ${{ runner.temp }}/crosscheck/",
    ] {
        assert!(upload.contains(part), "{part}: {upload}");
    }
}

/// A harness result on Linux with duscope's ratio to diskus on full-scale T1.
fn result(ratio: f64, started_at: &str) -> Value {
    json!({
        "schema_version": 1,
        "started_at": started_at,
        "platform": "linux",
        "subject": "duscope",
        "host": {"arch": "x86_64", "cpu": "a CPU", "logical_cpus": 4},
        "harness": {"commit": "c0ffee"},
        "trees": [{"name": "T1", "digest": "9762826403df6c7f0d30b21ee1c3f240"}],
        "results": [
            {"tree": "T1", "mode": "warm", "tool": "diskus", "paired_with": "duscope", "p50_s": 0.6, "subject_ratio_p50": ratio}
        ]
    })
}

/// Runs the comparison step's script as the job would, in a scratch
/// workspace holding `bench/` and a `bench-results` checkout with
/// bench-linux's ratio `theirs` (none when None).
fn compare_step(ours: f64, theirs: Option<f64>, scale: &str) -> (tempfile::TempDir, Output) {
    let job = &jobs(&read(WORKFLOW))[0];
    let script = job.step_script("Compare with bench-linux's latest nightly");
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path();
    std::os::unix::fs::symlink(root().join("bench"), dir.join("bench")).unwrap();
    fs::create_dir_all(dir.join("bench-results")).unwrap();
    fs::create_dir_all(dir.join("runner-temp/crosscheck")).unwrap();
    if let Some(ratio) = theirs {
        let nightly = result(ratio, "2026-11-08T17:00:00Z").to_string();
        fs::write(dir.join("bench-results/2026-11-08-linux.json"), nightly).unwrap();
    }
    let file = dir.join("runner-temp/crosscheck/linux-crosscheck.json");
    fs::write(&file, result(ours, "2026-11-20T10:00:00Z").to_string()).unwrap();
    fs::write(dir.join("step.sh"), script).unwrap();
    let out = Command::new("bash")
        .args(["--noprofile", "--norc", "-eo", "pipefail"])
        .arg(dir.join("step.sh"))
        .current_dir(dir)
        .env("PYTHON", python())
        .env("FILE", &file)
        .env("SCALE", scale)
        .env("RUNNER_TEMP", dir.join("runner-temp"))
        .env("GITHUB_STEP_SUMMARY", dir.join("summary"))
        .env("GITHUB_ACTIONS", "true")
        .output()
        .unwrap();
    (temp, out)
}

/// P5-BENCH-01 (DEV-157): the comparison step, run under bash with the real
/// `bench/run.sh crosscheck`. A ratio within the noise or outside it passes,
/// outside it with a warning, and the lines go to the job's summary and the
/// artifact; nothing to compare fails the step; a smoke run compares nothing.
#[test]
fn p5_bench_01_the_comparison_step_fails_only_when_nothing_compares() {
    for (ours, theirs, passes, warned) in [
        (0.36, Some(0.354), true, false),
        (0.6, Some(0.354), true, true),
        (0.36, None, false, false),
    ] {
        let (temp, out) = compare_step(ours, theirs, "1");
        let dir = temp.path();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert_eq!(out.status.success(), passes, "{ours} {theirs:?}: {out:?}");
        assert_eq!(
            stdout.contains("::warning title=linux-crosscheck::T1 warm duscope/diskus: "),
            warned,
            "{stdout}"
        );
        if theirs.is_none() {
            continue;
        }
        let summary = fs::read_to_string(dir.join("summary")).unwrap();
        assert!(
            summary.starts_with("```\nhere: linux-crosscheck.json, "),
            "{summary}"
        );
        assert!(summary.contains("T1 warm duscope/diskus: "), "{summary}");
        assert!(!summary.contains("::"), "{summary}");
        let kept = fs::read_to_string(dir.join("runner-temp/crosscheck/comparison.txt")).unwrap();
        assert!(
            kept.contains("ratios within 15% of bench-linux's"),
            "{kept}"
        );
    }
    let (temp, out) = compare_step(0.36, Some(0.354), "0.01");
    assert!(out.status.success(), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stdout)
            .contains("::notice title=linux-crosscheck::a run at scale 0.01 is a smoke run")
    );
    assert!(
        !temp
            .path()
            .join("runner-temp/crosscheck/comparison.txt")
            .exists()
    );
}
