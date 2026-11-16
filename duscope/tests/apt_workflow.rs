//! P5-PKG-01: `apt.yml`, which builds, signs and publishes the apt repository
//! after each release, and tests it on the pull requests that change it.
//!
//! The workflow is read as text, as `ci_workflow.rs` reads ci.yml: jobs are the
//! keys indented by two spaces under `jobs:`, job-level keys by four, steps the
//! `- ` entries under a job's `steps:`. These tests never run a job; the
//! script the jobs call is `apt_repository.rs`'s, and apt.yml's `repo-test`
//! job runs it on Linux.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

const WORKFLOW: &str = ".github/workflows/apt.yml";
const RELEASE_WORKFLOW: &str = ".github/workflows/release.yml";
const SETTINGS: &str = "docs/github-settings.md";
const SCRIPT: &str = "scripts/apt-repo.sh";

/// The runs that publish: a dispatch, or a successful run of release.yml for
/// a tag; release.yml's runs for pull requests, and failed ones, skip it all.
const PUBLISHING_RUN: &str = "${{ github.event_name == 'workflow_dispatch' || \
     (github.event_name == 'workflow_run' && github.event.workflow_run.event == 'push' && \
     github.event.workflow_run.conclusion == 'success') }}";

/// The images and the runner of each architecture, as release.yml's
/// deb-install has them (P1-PKG-01).
const IMAGES: &[&str] = &["\"debian:12\"", "\"ubuntu:22.04\""];
const RUNNERS: &[(&str, &str)] = &[("amd64", "ubuntu-24.04"), ("arm64", "ubuntu-24.04-arm")];

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

    /// The entries of a job-level block (`    key:` and the deeper lines below
    /// it), comments dropped.
    fn block(&self, key: &str) -> Vec<&str> {
        let head = format!("    {key}:");
        self.lines
            .iter()
            .skip_while(|line| line.trim_end() != head)
            .skip(1)
            .take_while(|line| line.starts_with("      ") || line.trim().is_empty())
            .map(|line| line.trim())
            .filter(|entry| !entry.is_empty() && !entry.starts_with('#'))
            .collect()
    }

    /// The jobs this job needs, in either `needs:` form.
    fn needs(&self) -> Vec<String> {
        match self.key("needs") {
            None => Vec::new(),
            Some(list) if list.starts_with('[') => list
                .trim_matches(['[', ']'])
                .split(',')
                .map(|need| need.trim().to_owned())
                .collect(),
            Some(one) => vec![one.to_owned()],
        }
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

    /// The names of the steps, in order (`uses:` steps by their action).
    fn step_names(&self) -> Vec<String> {
        self.steps()
            .iter()
            .map(|step| {
                let first = step[0].trim().trim_start_matches("- ");
                first
                    .strip_prefix("name: ")
                    .or_else(|| first.strip_prefix("uses: "))
                    .unwrap_or(first)
                    .to_owned()
            })
            .collect()
    }

    /// The step with this name, as one text.
    fn step(&self, name: &str) -> String {
        self.steps()
            .into_iter()
            .find(|step| step[0].trim() == format!("- name: {name}"))
            .unwrap_or_else(|| panic!("job `{}` has no step `{name}`", self.id))
            .join("\n")
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

    /// The matrix's `include` entries, as `arch` to `runner`.
    fn runners(&self) -> BTreeMap<String, String> {
        let mut runners = BTreeMap::new();
        let mut arch = None;
        for line in self.lines.iter().map(|line| line.trim()) {
            if let Some(value) = line.strip_prefix("- arch: ") {
                arch = Some(value.to_owned());
            } else if let (Some(value), Some(a)) = (line.strip_prefix("runner: "), arch.take()) {
                runners.insert(a, value.to_owned());
            }
        }
        runners
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
        .find(|job| job.id == id)
        .unwrap_or_else(|| panic!("apt.yml has no job `{id}`"))
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

/// P5-PKG-01: apt.yml follows release.yml's completion (`release: published`
/// never fires for a release its `GITHUB_TOKEN` made), takes a dispatch naming a
/// tag, and runs on the pull requests that change its files; nothing else
/// starts it. Only a dispatch, or release.yml's successful run for a tag,
/// reaches the publishing jobs.
#[test]
fn p5_pkg_01_apt_runs_after_a_release_on_a_dispatch_and_on_pull_requests_that_change_it() {
    let workflow = read(WORKFLOW);
    let on = block(&workflow, "on");
    let triggers: Vec<&str> = on
        .iter()
        .filter_map(|line| line.strip_prefix("  "))
        .filter(|rest| !rest.starts_with(' '))
        .collect();
    assert_eq!(
        triggers,
        ["workflow_run:", "workflow_dispatch:", "pull_request:"],
        "{on:?}"
    );
    let release_name = read(RELEASE_WORKFLOW)
        .lines()
        .find_map(|line| line.strip_prefix("name: "))
        .unwrap()
        .trim()
        .to_owned();
    assert!(
        on.contains(&format!("    workflows: [{release_name}]").as_str()),
        "{on:?}"
    );
    assert!(on.contains(&"    types: [completed]"), "{on:?}");
    let tag_input: Vec<&str> = on
        .iter()
        .skip_while(|line| line.trim_end() != "      tag:")
        .skip(1)
        .take_while(|line| line.starts_with("        "))
        .map(|line| line.trim())
        .collect();
    assert!(tag_input.contains(&"required: true"), "{on:?}");
    let paths: BTreeSet<&str> = on
        .iter()
        .skip_while(|line| line.trim_end() != "    paths:")
        .skip(1)
        .filter_map(|line| line.trim().strip_prefix("- "))
        .map(|path| path.trim_matches('"'))
        .collect();
    assert_eq!(
        paths,
        BTreeSet::from([
            ".github/workflows/apt.yml",
            "apt/**",
            "docs/apt.md",
            "scripts/apt-repo.sh"
        ])
    );
    for path in &paths {
        assert!(
            root().join(path.trim_end_matches("/**")).exists(),
            "{path} names nothing"
        );
    }

    let jobs = jobs(&workflow);
    let select = job(&jobs, "select");
    assert_eq!(select.key("if"), Some(PUBLISHING_RUN));
    // The tag: a dispatch's input, or the tag release.yml ran for.
    let tag = select.step("The release");
    assert!(
        tag.contains(
            "TAG: ${{ github.event_name == 'workflow_dispatch' && inputs.tag || \
             github.event.workflow_run.head_branch }}"
        ),
        "{tag}"
    );
}

/// P5-PKG-01 (DEV-4): the token is read-only but in publish, which pushes to
/// gh-pages, requests the Pages build, and holds no secret; the two signing secrets reach build alone,
/// each one step, and build runs only after select, never for a pull request.
#[test]
fn p5_pkg_01_the_token_writes_in_publish_alone_and_each_secret_reaches_one_step() {
    let workflow = read(WORKFLOW);
    assert_eq!(block(&workflow, "permissions"), ["  contents: read"]);
    assert!(!workflow.contains("write-all") && !workflow.contains("id-token"));
    let jobs = jobs(&workflow);
    for job in &jobs {
        let writes: Vec<&str> = job
            .lines
            .iter()
            .map(|line| line.trim())
            .filter(|line| line.ends_with(": write"))
            .collect();
        // The push to gh-pages, and the Pages build a push made with the
        // GITHUB_TOKEN does not start.
        let want: &[&str] = if job.id == "publish" {
            &["contents: write", "pages: write"]
        } else {
            &[]
        };
        assert_eq!(writes, want, "job `{}` widens the token", job.id);
    }

    assert_eq!(
        workflow.matches("secrets.").count(),
        2,
        "apt.yml reads two secrets"
    );
    assert!(!workflow.contains("secrets: inherit"));
    let build = job(&jobs, "build");
    for (secret, step) in [
        ("APT_SIGNING_SUBKEY", "Import the signing subkey"),
        (
            "APT_SIGNING_PASSPHRASE",
            "Build, sign and verify the repository",
        ),
    ] {
        assert!(
            build
                .step(step)
                .contains(&format!("{secret}: ${{{{ secrets.{secret} }}}}")),
            "{secret} is not in the env of build's step `{step}`"
        );
    }
    assert_eq!(build.needs(), ["select"]);
    assert_eq!(build.key("if"), None, "build runs only when select ran");
    assert!(
        !build
            .lines
            .iter()
            .any(|line| line.trim() == "env:" && line.len() - line.trim_start().len() == 4),
        "build has a job-level env, which every step would see"
    );

    // One push to gh-pages at a time, never cancelled by the next.
    let publish = job(&jobs, "publish");
    assert_eq!(
        publish.block("concurrency"),
        ["group: apt-publish", "cancel-in-progress: false"]
    );
    assert!(
        publish
            .lines
            .iter()
            .any(|line| line.trim() == "ref: gh-pages")
    );
    assert!(
        publish
            .step("Replace apt/ on gh-pages")
            .contains("git push -q origin HEAD:gh-pages")
    );
    assert!(
        publish
            .step("Build the Pages site")
            .contains("run: gh api -X POST \"repos/$GITHUB_REPOSITORY/pages/builds\"")
    );
}

/// P5-PKG-01: a pull request runs repo-test and nothing else; every other job
/// waits for select, so it runs for a publishing run alone, and none runs
/// under `always()`. Nothing is published that was not installed first.
#[test]
fn p5_pkg_01_pull_requests_run_repo_test_alone_and_nothing_untested_is_published() {
    let workflow = read(WORKFLOW);
    let jobs = jobs(&workflow);
    let ids: Vec<&str> = jobs.iter().map(|job| job.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "select",
            "build",
            "local-install",
            "publish",
            "live-install",
            "repo-test"
        ]
    );
    let needs: BTreeMap<&str, Vec<String>> = jobs
        .iter()
        .map(|job| (job.id.as_str(), job.needs()))
        .collect();
    for job in &jobs {
        assert!(
            !job.lines.iter().any(|line| line.contains("always()")) || job.id == "build",
            "job `{}` runs under always()",
            job.id
        );
        if job.id == "select" || job.id == "repo-test" {
            assert!(job.needs().is_empty(), "job `{}` needs a job", job.id);
            continue;
        }
        let mut seen = BTreeSet::new();
        let mut queue = vec![job.id.clone()];
        while let Some(id) = queue.pop() {
            if seen.insert(id.clone()) {
                queue.extend(needs[id.as_str()].iter().cloned());
            }
        }
        assert!(
            seen.contains("select"),
            "job `{}` does not wait for select",
            job.id
        );
    }
    // build's always() is its step that forgets the key, not a job condition.
    let build = job(&jobs, "build");
    assert!(build.key("if").is_none());
    assert!(
        build
            .step("Forget the subkey")
            .contains("if: ${{ always() }}")
    );

    let repo_test = job(&jobs, "repo-test");
    assert_eq!(
        repo_test.key("if"),
        Some("${{ github.event_name == 'pull_request' }}")
    );
    assert!(!repo_test.lines.iter().any(|line| line.contains("secrets.")));
    let publish = job(&jobs, "publish");
    assert_eq!(publish.needs(), ["select", "build", "local-install"]);
    assert_eq!(job(&jobs, "live-install").needs(), ["publish"]);
    // A new push to a pull request replaces its test; no publishing run is
    // cancelled.
    assert_eq!(
        block(&workflow, "concurrency"),
        [
            "  group: apt-${{ github.event_name }}-${{ github.event.pull_request.number || github.run_id }}",
            "  cancel-in-progress: ${{ github.event_name == 'pull_request' }}",
        ]
    );
}

/// P5-PKG-01: the repository is installed from, as docs/apt.md says, in Debian
/// 12 and Ubuntu 22.04 on amd64 and arm64: before publishing, from the
/// repository served on the runner; after, from the live URL; and on a pull
/// request, from the throwaway key's repository.
#[test]
fn p5_pkg_01_the_installs_run_in_debian_12_and_ubuntu_22_04_on_amd64_and_arm64() {
    let workflow = read(WORKFLOW);
    let jobs = jobs(&workflow);
    let runners: BTreeMap<String, String> = RUNNERS
        .iter()
        .map(|(arch, runner)| ((*arch).to_owned(), (*runner).to_owned()))
        .collect();
    let release = self::jobs(&read(RELEASE_WORKFLOW));
    assert_eq!(job(&release, "deb-install").runners(), runners);
    for (id, run) in [
        (
            "local-install",
            "run: scripts/apt-repo.sh install-test \"$IMAGE\" \"$RUNNER_TEMP/site\"",
        ),
        (
            "live-install",
            "run: scripts/apt-repo.sh install-test \"$IMAGE\"",
        ),
    ] {
        let job = job(&jobs, id);
        assert_eq!(
            job.key("name"),
            Some(format!("{id} (${{{{ matrix.image }}}}, ${{{{ matrix.arch }}}})").as_str())
        );
        assert_eq!(job.matrix("image"), IMAGES);
        assert_eq!(job.matrix("arch"), ["amd64", "arm64"]);
        assert_eq!(job.runners(), runners, "{id}");
        assert_eq!(job.key("runs-on"), Some("${{ matrix.runner }}"));
        assert!(
            job.lines.iter().any(|line| line.trim() == run),
            "{id} lacks `{run}`"
        );
        assert!(
            job.lines
                .iter()
                .any(|line| line.trim() == "IMAGE: ${{ matrix.image }}")
        );
    }
    let live = job(&jobs, "live-install");
    let names = live.step_names();
    let wait = names
        .iter()
        .position(|name| name == "Wait for Pages to serve the new repository")
        .expect("live-install does not wait for Pages");
    assert!(wait < names.len() - 1, "{names:?}");

    let repo_test = job(&jobs, "repo-test");
    assert_eq!(repo_test.runners(), runners);
    let install = repo_test.step("Install from the repository as docs/apt.md says");
    assert!(
        install.contains("for image in debian:12 ubuntu:22.04; do"),
        "{install}"
    );
    assert!(
        install.contains("scripts/apt-repo.sh install-test \"$image\" \"$RUNNER_TEMP/site\""),
        "{install}"
    );
    // The script runs docs/apt.md's commands as they stand, and fails when
    // apt warns.
    let script = read(SCRIPT);
    assert!(script.contains("install_commands \"$root/docs/apt.md\" > \"$dir/install.sh\""));
    assert!(script.contains("if grep -E '^(W|E): ' /tmp/install.log; then"));
    assert!(script.contains("duscope --version"));
    assert!(script.contains("duscope top /usr/share/doc --json"));
}

/// P5-PKG-01 and P5-SEC-02: the key is checked before anything else, the
/// subkey is refused when it expires within 60 days, before it signs, and a
/// pull request runs that check against a subkey with 30 days left.
#[test]
fn p5_pkg_01_the_subkey_is_refused_within_60_days_and_tested_with_30_left() {
    let workflow = read(WORKFLOW);
    let jobs = jobs(&workflow);
    let select = job(&jobs, "select");
    assert_eq!(
        select.step_names()[..2],
        [
            "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1",
            "The public key is the one docs/apt.md records"
        ]
    );
    assert!(
        select
            .step("The public key is the one docs/apt.md records")
            .contains("run: scripts/apt-repo.sh check-key")
    );
    let build = job(&jobs, "build");
    let names = build.step_names();
    let at = |name: &str| {
        names
            .iter()
            .position(|n| n == name)
            .unwrap_or_else(|| panic!("build has no step `{name}`: {names:?}"))
    };
    let (import, expiry, sign) = (
        at("Import the signing subkey"),
        at("The subkey expires more than 60 days from now"),
        at("Build, sign and verify the repository"),
    );
    assert!(import < expiry && expiry < sign, "{names:?}");
    assert!(
        build
            .step("The subkey expires more than 60 days from now")
            .contains("run: scripts/apt-repo.sh check-expiry 60")
    );

    let script = read(SCRIPT);
    assert!(script.contains("\nEXPIRY_DAYS=60\n"));
    assert!(script.contains("cmd_test_key \"$short\" 30 > /dev/null"));
    assert!(script.contains(
        "expect \"check-expiry, 30 days left\" \"within $EXPIRY_DAYS\" \"$short/duscope.gpg\" \"$short/subkey.asc\""
    ));
    let repo_test = job(&jobs, "repo-test");
    assert!(
        repo_test
            .step("The key checks against throwaway keys, a subkey with 30 days left among them")
            .contains("run: scripts/apt-repo.sh self-test")
    );
    let build_step = repo_test.step("Build, sign and verify the repository with a throwaway key");
    for line in [
        "scripts/apt-repo.sh test-key \"$RUNNER_TEMP/key\" 365",
        "scripts/apt-repo.sh import",
        "scripts/apt-repo.sh check-expiry 60",
        "scripts/apt-repo.sh build \"$RUNNER_TEMP/site/apt\"",
    ] {
        assert!(build_step.contains(line), "repo-test lacks `{line}`");
    }
}

/// P5-PKG-01: every action pinned to a full commit SHA with its version, and
/// every job with a timeout.
#[test]
fn p5_pkg_01_actions_are_pinned_and_every_job_has_a_timeout() {
    let workflow = read(WORKFLOW);
    let mut count = 0;
    for (n, line) in workflow.lines().enumerate() {
        let Some((_, rest)) = line.split_once("uses:") else {
            continue;
        };
        count += 1;
        let action = rest.split_whitespace().next().unwrap_or_default();
        let sha = action
            .split_once('@')
            .map(|(_, sha)| sha)
            .unwrap_or_default();
        assert!(
            sha.len() == 40
                && sha
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "apt.yml:{}: `{action}` is not pinned to a full commit SHA",
            n + 1
        );
        assert!(
            rest.contains("# v"),
            "apt.yml:{}: `{action}` has no version comment",
            n + 1
        );
    }
    assert!(count > 0);
    for job in jobs(&workflow) {
        assert!(
            job.key("timeout-minutes")
                .is_some_and(|minutes| minutes.parse::<u32>().is_ok_and(|m| (1..=30).contains(&m))),
            "job `{}` has no timeout of 30 minutes or less",
            job.id
        );
    }
}

/// P5-PKG-01: no job of apt.yml is a required check. repo-test runs only on
/// the pull requests that change the repository's files, so a required check
/// of it would hold every other pull request; docs/github-settings.md says so.
#[test]
fn p5_pkg_01_no_job_of_apt_yml_is_a_required_check() {
    let settings = read(SETTINGS);
    let required: Vec<&str> = settings
        .lines()
        .filter_map(|line| line.split_once("\"context\": \"").map(|(_, rest)| rest))
        .filter_map(|rest| rest.split_once('"').map(|(context, _)| context))
        .collect();
    assert!(!required.is_empty());
    for job in jobs(&read(WORKFLOW)) {
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
    let text = settings.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        text.contains(
            "[apt.yml](../.github/workflows/apt.yml) (P5-PKG-01) runs `repo-test` on the pull \
             requests that change the apt repository's files alone"
        ),
        "{SETTINGS} does not say apt.yml's repo-test is no check"
    );
}
