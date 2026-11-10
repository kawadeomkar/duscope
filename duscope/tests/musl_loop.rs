//! P5-BENCH-02: `musl-loop.yml`, the `x86_64` musl build scanning T2 thousands
//! of times on GitHub's hosted runners, on a dispatch alone.
//!
//! The workflow is read as text, as `ci_workflow.rs` reads ci.yml: jobs are the
//! keys indented by two spaces under `jobs:`, job-level keys by four. The loop's
//! step runs for real under bash, against a stand-in for duscope and for
//! `timeout`; these tests never run the duscope binary.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fs;
use std::path::{Path, PathBuf};

const WORKFLOW: &str = ".github/workflows/musl-loop.yml";

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

    /// The step with this name, as its lines.
    fn step(&self, name: &str) -> Vec<&str> {
        self.steps()
            .into_iter()
            .find(|step| step[0].trim() == format!("- name: {name}"))
            .unwrap_or_else(|| panic!("job `{}` has no step `{name}`", self.id))
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

    /// The values of the matrix line `key: [a, b]`.
    fn matrix(&self, key: &str) -> Vec<String> {
        let prefix = format!("{key}: [");
        let line = self
            .lines
            .iter()
            .find_map(|line| line.trim().strip_prefix(&prefix))
            .unwrap_or_else(|| panic!("job `{}` has no matrix `{key}`", self.id));
        line.trim_end_matches(']')
            .split(',')
            .map(|value| value.trim().to_owned())
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

/// P5-BENCH-02: the loop runs on a dispatch and nothing else, so no pull
/// request, push or schedule starts it and none of its jobs is a required
/// check; it reads no secret, widens no token, and two dispatches never
/// overlap. 8,000 scans by default, T2 at its full scale.
#[test]
fn p5_bench_02_the_musl_loop_runs_on_a_dispatch_alone_and_gates_no_merge() {
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
            "musl-loop.yml is triggered by {event}"
        );
    }
    // The inputs and their defaults.
    let input = |name: &str| -> Vec<String> {
        on.iter()
            .skip_while(|line| line.trim_end() != format!("      {name}:"))
            .skip(1)
            .take_while(|line| line.starts_with("        "))
            .map(|line| line.trim().to_owned())
            .collect()
    };
    assert!(
        input("runs").contains(&"default: \"8000\"".to_owned()),
        "{on:?}"
    );
    assert!(
        input("scale").contains(&"default: \"1\"".to_owned()),
        "{on:?}"
    );
    assert!(
        input("threads").contains(&"default: \"\"".to_owned()),
        "{on:?}"
    );
    // One run at a time, never cancelled by the next.
    assert_eq!(
        block(&workflow, "concurrency"),
        ["  group: musl-loop", "  cancel-in-progress: false"]
    );
    assert_eq!(block(&workflow, "permissions"), ["  contents: read"]);
    assert!(
        !workflow.contains("secrets."),
        "musl-loop.yml reads a secret"
    );
    assert!(
        !workflow.contains(": write") && !workflow.contains("write-all"),
        "musl-loop.yml widens the token"
    );
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
    for job in jobs(&workflow) {
        let name = job.key("name").unwrap_or(&job.id);
        let stem = name.split(" (").next().unwrap();
        assert!(
            required
                .iter()
                .all(|check| check.split(" (").next().unwrap() != stem),
            "job `{}` reports a check named like a required one: {name}",
            job.id
        );
    }
}

/// P5-BENCH-02: four shards on GitHub's ubuntu-24.04, each run to its end
/// whatever another found, build the shipped target in `just repro`'s
/// environment and scan T2 with the harness's command line; each keeps its
/// files whatever happened. The actions are pinned as everywhere else.
#[test]
fn p5_bench_02_each_shard_builds_the_musl_target_and_scans_t2_as_the_harness_does() {
    let workflow = read(WORKFLOW);
    let jobs = jobs(&workflow);
    let ids: Vec<&str> = jobs.iter().map(|job| job.id.as_str()).collect();
    assert_eq!(ids, ["musl-loop"]);
    let job = &jobs[0];
    assert_eq!(job.key("runs-on"), Some("ubuntu-24.04"));
    assert_eq!(job.key("name"), Some("musl-loop (${{ matrix.shard }})"));
    let minutes: u32 = job.key("timeout-minutes").unwrap().parse().unwrap();
    assert!((1..=360).contains(&minutes), "{minutes} minutes");
    assert!(
        job.lines
            .iter()
            .any(|line| line.trim() == "fail-fast: false")
    );
    assert_eq!(job.matrix("shard"), ["1", "2", "3", "4"]);
    // The pinned tools, as ci.yml pins them.
    assert_eq!(
        workflow_env(&workflow, "JUST_VERSION"),
        workflow_env(&read(".github/workflows/ci.yml"), "JUST_VERSION")
    );
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
    // The shipped target, in the reproducible build's environment.
    assert!(
        job.step("Use the reproducible build environment")
            .iter()
            .any(|line| line.trim() == "run: just repro-env target/repro | tee -a \"$GITHUB_ENV\""),
        "the build does not run in `just repro`'s environment"
    );
    let build = job.step_script("Build the x86_64 musl binary");
    assert!(
        build.contains(
            "cargo build --release --locked -p duscope --target x86_64-unknown-linux-musl --target-dir target/repro"
        ),
        "{build}"
    );
    assert!(build.contains("sha256sum target/repro/x86_64-unknown-linux-musl/release/duscope"));
    assert!(
        job.step("Build T2")
            .iter()
            .any(|line| line.trim() == "run: bench/run.sh build --trees T2 --scale \"$SCALE\" --trees-dir \"$RUNNER_TEMP/trees\""),
        "T2 is not built by the harness"
    );
    // The binary just built, on the harness's command line for duscope.
    let scan = job.step("Scan T2 again and again").join("\n");
    assert!(
        scan.contains("DUSCOPE: target/repro/x86_64-unknown-linux-musl/release/duscope"),
        "{scan}"
    );
    assert!(
        read("bench/tools.py")
            .contains(r#"("duscope", "scan", "--json", "--treemap", "never", "{tree}")"#)
    );
    assert!(
        scan.contains(
            r#""$DUSCOPE" scan --json --treemap never ${THREADS:+--threads "$THREADS"} "$tree""#
        ),
        "{scan}"
    );
    // Each shard's files are kept after a failure or a timeout too.
    let upload = job
        .steps()
        .into_iter()
        .find(|step| step[0].contains("actions/upload-artifact@"))
        .expect("musl-loop keeps no result")
        .join("\n");
    assert!(upload.contains("if: ${{ always() }}"), "{upload}");
    assert!(
        upload.contains("name: musl-loop-${{ matrix.shard }}"),
        "{upload}"
    );
    assert!(
        upload.contains("path: ${{ runner.temp }}/musl-loop/"),
        "{upload}"
    );
}

/// The stand-ins the loop's step runs with: `timeout` runs its command, and
/// duscope fails the scans listed in `fail` (`run status` lines) with a panic's
/// line on stderr, and logs its arguments.
const STUBS: &[(&str, &str)] = &[
    ("timeout", "#!/usr/bin/env bash\nshift\nexec \"$@\"\n"),
    (
        "duscope",
        "#!/usr/bin/env bash\nn=$(( $(cat \"$STUB_DIR/count\" 2>/dev/null || echo 0) + 1 ))\n\
         echo \"$n\" > \"$STUB_DIR/count\"\necho \"$*\" >> \"$STUB_DIR/args\"\n\
         status=$(awk -v n=\"$n\" '$1 == n { print $2 }' \"$STUB_DIR/fail\")\n\
         if [[ -n $status ]]; then echo \"scan $n: slice index starts at 9 but ends at 1\" >&2; exit \"$status\"; fi\n\
         echo '{}'\n",
    ),
];

/// A case of the loop's step: (runs, shards, this shard, failing scans) ->
/// (its scans, the step succeeds, failures.txt).
type Case<'a> = ((&'a str, u32, u32, &'a str), (u32, bool, &'a str));

/// Runs the loop's `script` as the job would, under bash with the stand-ins
/// and a scaled T2 beside its stamp, as `bench/run.sh build` leaves it, in a
/// directory of its own.
fn run_loop(
    script: &str,
    (runs, shards, shard, fail): (&str, u32, u32, &str),
    threads: &str,
) -> (tempfile::TempDir, std::process::Output) {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path();
    let bin = dir.join("bin");
    fs::create_dir_all(&bin).unwrap();
    for (name, body) in STUBS {
        let path = bin.join(name);
        fs::write(&path, body).unwrap();
        let mut mode = fs::metadata(&path).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut mode, 0o755);
        fs::set_permissions(&path, mode).unwrap();
    }
    fs::write(dir.join("fail"), fail).unwrap();
    fs::create_dir_all(dir.join("trees/T2-x0.01")).unwrap();
    fs::write(dir.join("trees/T2-x0.01.json"), "{}").unwrap();
    fs::write(dir.join("step.sh"), script).unwrap();
    let mut search = vec![bin.clone()];
    search.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let out = std::process::Command::new("bash")
        .args(["--noprofile", "--norc", "-eo", "pipefail"])
        .arg(dir.join("step.sh"))
        .current_dir(dir)
        .env("PATH", std::env::join_paths(search).unwrap())
        .env("STUB_DIR", dir)
        .env("RUNS", runs)
        .env("THREADS", threads)
        .env("SHARD", shard.to_string())
        .env("SHARDS", shards.to_string())
        .env("DUSCOPE", bin.join("duscope"))
        .env("TREES", dir.join("trees"))
        .env("OUT", dir.join("out"))
        .env("GITHUB_STEP_SUMMARY", dir.join("summary"))
        .output()
        .unwrap();
    (temp, out)
}

/// P5-BENCH-02: the loop's step, run under bash with a stand-in duscope and a
/// `timeout` that only runs it. Each shard runs its share of the scans, all of
/// them whatever fails, writes each one's exit code, names each failure's
/// signal or status with its stderr, and then fails if any scan did.
#[test]
fn p5_bench_02_the_loop_runs_every_scan_and_fails_on_any_that_exits_non_zero() {
    let jobs = jobs(&read(WORKFLOW));
    let script = jobs[0].step_script("Scan T2 again and again");
    let cases: &[Case] = &[
        (("10", 4, 0, ""), (3, true, "")),
        (("10", 4, 1, ""), (3, true, "")),
        (("10", 4, 2, ""), (2, true, "")),
        (("10", 4, 3, ""), (2, true, "")),
        (("9", 4, 0, ""), (3, true, "")),
        (("9", 4, 1, ""), (2, true, "")),
        (
            ("5", 1, 0, "2 134\n4 124\n5 1\n"),
            (
                5,
                false,
                "run 2: SIGABRT\nrun 4: timed out after 600 s\nrun 5: exit 1\n",
            ),
        ),
        (("0", 4, 0, ""), (0, false, "")),
        (("lots", 4, 0, ""), (0, false, "")),
    ];
    for &(input, (scans, succeeds, failures)) in cases {
        let (runs, shards, shard, fail) = input;
        let (temp, out) = run_loop(&script, input, "");
        let dir = temp.path();
        let case = format!("{runs} runs, shard {shard} of {shards}");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert_eq!(out.status.success(), succeeds, "{case}: {out:?}");
        let args = fs::read_to_string(dir.join("args")).unwrap_or_default();
        assert_eq!(args.lines().count(), scans as usize, "{case}");
        let command = format!(
            "scan --json --treemap never {}",
            dir.join("trees/T2-x0.01").display()
        );
        assert!(args.lines().all(|line| line == command), "{case}: {args}");
        if scans == 0 {
            assert!(
                stdout.contains("::error title=musl-loop::runs is not a whole number above 0"),
                "{case}: {stdout}"
            );
            continue;
        }
        let expected: Vec<String> = (1..=scans)
            .map(|n| {
                let status = fail
                    .lines()
                    .find_map(|l| l.strip_prefix(&format!("{n} ")))
                    .unwrap_or("0");
                format!("{n} {status}")
            })
            .collect();
        let expected = expected.join("\n") + "\n";
        let codes = fs::read_to_string(dir.join("out/exit-codes.txt")).unwrap();
        assert_eq!(codes, expected, "{case}");
        let failed = failures.lines().count();
        assert_eq!(
            fs::read_to_string(dir.join("out/failures.txt")).unwrap_or_default(),
            failures,
            "{case}"
        );
        assert_eq!(
            stdout.matches("::error title=musl-loop::").count(),
            failed,
            "{case}: {stdout}"
        );
        for line in failures.lines() {
            let n: u32 = line["run ".len()..line.find(':').unwrap()].parse().unwrap();
            let stderr = fs::read_to_string(dir.join(format!("out/run-{n}.stderr"))).unwrap();
            assert_eq!(
                stderr,
                format!("scan {n}: slice index starts at 9 but ends at 1\n"),
                "{case}"
            );
        }
        assert!(!dir.join("out/stderr").exists(), "{case}");
        let count = format!(
            "shard {} of {shards}: {scans} scans of {runs}, {failed} failed\n",
            shard + 1
        );
        assert_eq!(
            fs::read_to_string(dir.join("out/count.txt")).unwrap(),
            count
        );
        assert_eq!(fs::read_to_string(dir.join("summary")).unwrap(), count);
    }
}

/// P5-BENCH-02: the `threads` input, left blank by the test above, where the
/// command line is the harness's as it stands. A number adds `--threads N`
/// before the tree, as the harness's thread sweep writes it, and the count
/// says so, since two dispatches' artifacts must be told apart; anything else
/// is refused before any scan.
#[test]
fn p5_bench_02_the_threads_input_reaches_every_scan_and_a_bad_one_none() {
    let jobs = jobs(&read(WORKFLOW));
    let script = jobs[0].step_script("Scan T2 again and again");
    let (temp, out) = run_loop(&script, ("2", 1, 0, ""), "20");
    let dir = temp.path();
    assert!(out.status.success(), "{out:?}");
    let command = format!(
        "scan --json --treemap never --threads 20 {}",
        dir.join("trees/T2-x0.01").display()
    );
    let args = fs::read_to_string(dir.join("args")).unwrap();
    assert_eq!(args, format!("{command}\n{command}\n"));
    assert_eq!(
        fs::read_to_string(dir.join("out/count.txt")).unwrap(),
        "shard 1 of 1: 2 scans of 2, 0 failed (--threads 20)\n"
    );
    for threads in ["0", "lots", "20 --json"] {
        let (temp, out) = run_loop(&script, ("2", 1, 0, ""), threads);
        assert!(!out.status.success(), "{threads}: {out:?}");
        assert!(
            String::from_utf8_lossy(&out.stdout)
                .contains("::error title=musl-loop::threads is not blank"),
            "{threads}: {out:?}"
        );
        assert!(!temp.path().join("args").exists(), "{threads}");
    }
}
