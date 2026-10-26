//! P0-CI-02: the release pipeline. release.yml's shape (every job waits for
//! verify-tag, the token is widened only where a job needs it, actions are
//! pinned, the crates publish in dependency order), the cargo-dist
//! configuration, and the verify-tag step itself, run against throwaway
//! repositories with tags signed by throwaway keys.
//!
//! release.yml is read as text, as `ci_workflow.rs` reads ci.yml: jobs are the
//! keys indented by two spaces under `jobs:`, job-level keys by four. These
//! tests never run the duscope binary. The verify-tag test needs git (2.34 or
//! later, for SSH signatures), ssh-keygen, bash, jq and cargo on PATH, as the
//! release runner has them.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use cargo_metadata::{DependencyKind, MetadataCommand, Package};

const WORKFLOW: &str = ".github/workflows/release.yml";
const CI_WORKFLOW: &str = ".github/workflows/ci.yml";
const DIST_CONFIG: &str = "dist-workspace.toml";
const ALLOWED_SIGNERS: &str = ".github/allowed_signers";

/// The publish order of the spec (P0-CI-02).
const PUBLISH_ORDER: &[&str] = &[
    "duscope-walk",
    "duscope-core",
    "duscope-treemap",
    "duscope-tui",
    "duscope-mcp",
    "duscope",
];

/// The release targets of the spec (P0-CI-02).
const TARGETS: &[&str] = &[
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "aarch64-unknown-linux-musl",
    "x86_64-unknown-linux-musl",
];

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

    /// The jobs this job needs, in either `needs:` form.
    fn needs(&self) -> Vec<&str> {
        match self.key("needs") {
            Some("") => self.block("needs"),
            Some(one) => vec![one],
            None => Vec::new(),
        }
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
        .unwrap_or_else(|| panic!("release.yml has no job `{id}`"))
}

/// The value of a top-level `NAME: "value"` line under the workflow's `env:`.
fn workflow_env(workflow: &str, name: &str) -> Option<String> {
    workflow
        .lines()
        .skip_while(|line| *line != "env:")
        .take_while(|line| *line == "env:" || line.starts_with(' ') || line.is_empty())
        .find_map(|line| line.trim().strip_prefix(&format!("{name}:")))
        .map(|value| value.trim().trim_matches('"').to_owned())
}

#[test]
fn p0_ci_02_every_job_waits_for_verify_tag() {
    let jobs = jobs(&read(WORKFLOW));
    let needs: BTreeMap<&str, Vec<&str>> =
        jobs.iter().map(|j| (j.id.as_str(), j.needs())).collect();
    assert!(
        needs["verify-tag"].is_empty(),
        "verify-tag runs first and needs nothing"
    );
    assert!(
        needs["plan"].contains(&"verify-tag"),
        "plan, which every other job needs, needs verify-tag"
    );
    for job in &jobs {
        // Every job reaches verify-tag through its needs.
        let mut seen = BTreeSet::new();
        let mut queue = vec![job.id.as_str()];
        while let Some(id) = queue.pop() {
            if seen.insert(id) {
                queue.extend(needs.get(id).into_iter().flatten().copied());
            }
        }
        assert!(
            seen.contains("verify-tag"),
            "job `{}` does not wait for verify-tag",
            job.id
        );
        // always() runs a job even when a job it needs failed, so such a job
        // must require plan or host (both downstream of verify-tag) to have
        // succeeded.
        if let Some(condition) = job.key("if").filter(|c| c.contains("always()")) {
            assert!(
                ["plan", "host"]
                    .iter()
                    .any(|j| condition.contains(&format!("needs.{j}.result == 'success'"))),
                "job `{}` runs under always() without requiring plan or host: {condition}",
                job.id
            );
        }
        assert!(
            job.key("timeout-minutes")
                .is_some_and(|m| m.parse::<u32>().is_ok()),
            "job `{}` has no timeout-minutes",
            job.id
        );
    }
    let ids: Vec<&str> = jobs.iter().map(|j| j.id.as_str()).collect();
    for id in [
        "build-local-artifacts",
        "host",
        "publish-crates",
        "announce",
    ] {
        assert!(ids.contains(&id), "release.yml has no job `{id}`");
    }
}

#[test]
fn p0_ci_02_token_is_widened_only_where_a_job_needs_it() {
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
    // host creates the GitHub Release; build-local-artifacts attests the archives.
    let expected: BTreeMap<&str, Vec<&str>> = [
        ("host", vec!["contents: write"]),
        (
            "build-local-artifacts",
            vec!["attestations: write", "id-token: write"],
        ),
    ]
    .into();
    for job in jobs(&workflow) {
        let writes: Vec<&str> = job
            .lines
            .iter()
            .map(|line| line.trim())
            .filter(|line| line.ends_with(": write"))
            .collect();
        let want = expected.get(job.id.as_str()).cloned().unwrap_or_default();
        assert_eq!(
            writes, want,
            "job `{}` widens the token differently",
            job.id
        );
    }
}

#[test]
fn p0_ci_02_actions_are_pinned_to_a_full_commit_sha() {
    let workflow = read(WORKFLOW);
    let mut count = 0;
    for (n, line) in workflow.lines().enumerate() {
        let Some((_, rest)) = line.split_once("uses:") else {
            continue;
        };
        let action = rest.split_whitespace().next().unwrap_or_default();
        count += 1;
        let line_no = n + 1;
        let (_, sha) = action
            .split_once('@')
            .unwrap_or_else(|| panic!("release.yml:{line_no}: `{action}` has no @ref"));
        assert!(
            sha.len() == 40
                && sha
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "release.yml:{line_no}: `{action}` is not pinned to a full commit SHA"
        );
        assert!(
            rest.contains("# v"),
            "release.yml:{line_no}: `{action}` has no `# vX.Y.Z` version comment"
        );
    }
    assert!(count > 0, "no actions found in release.yml");
    assert_eq!(
        workflow_env(&workflow, "JUST_VERSION"),
        workflow_env(&read(CI_WORKFLOW), "JUST_VERSION"),
        "release.yml installs the just that ci.yml pins"
    );
}

#[test]
fn p0_ci_02_crates_publish_in_dependency_order() {
    let jobs = jobs(&read(WORKFLOW));
    let publish = job(&jobs, "publish-crates");
    let script = publish.step_script("Publish to crates.io");
    let order: Vec<&str> = script
        .lines()
        .find_map(|line| line.trim().strip_prefix("for crate in "))
        .and_then(|rest| rest.strip_suffix("; do"))
        .expect("publish-crates has no `for crate in ...; do` loop")
        .split_whitespace()
        .collect();
    assert_eq!(order, PUBLISH_ORDER);
    assert!(
        publish.needs().contains(&"host"),
        "the crates publish after the GitHub Release"
    );

    let metadata = MetadataCommand::new()
        .manifest_path(root().join("Cargo.toml"))
        .no_deps()
        .exec()
        .unwrap();
    let is_published = |p: &Package| p.publish.as_ref().is_none_or(|r| !r.is_empty());
    let members: BTreeMap<&str, &Package> = metadata
        .workspace_packages()
        .into_iter()
        .map(|p| (p.name.as_str(), p))
        .collect();
    let published: BTreeSet<&str> = members
        .iter()
        .filter(|(_, p)| is_published(p))
        .map(|(name, _)| *name)
        .collect();
    assert_eq!(
        order.iter().copied().collect::<BTreeSet<_>>(),
        published,
        "the loop publishes exactly the workspace crates without `publish = false`"
    );
    for (i, name) in order.iter().enumerate() {
        for dep in &members[name].dependencies {
            let earlier = &order[..i];
            if dep.kind != DependencyKind::Development && members.contains_key(dep.name.as_str()) {
                assert!(
                    earlier.contains(&dep.name.as_str()),
                    "{name} is published before its dependency {}",
                    dep.name
                );
            }
        }
    }
}

/// The value of `key = ...` in dist-workspace.toml, with a multi-line array
/// joined onto one line.
fn dist_value(config: &str, key: &str) -> Option<String> {
    let mut lines = config.lines();
    let first = lines.by_ref().find_map(|line| {
        line.strip_prefix(key)
            .and_then(|rest| rest.trim_start().strip_prefix('='))
    })?;
    let mut value = first.trim().to_owned();
    if value.starts_with('[') {
        for line in lines {
            if value.ends_with(']') {
                break;
            }
            value.push_str(line.trim());
        }
    }
    Some(value)
}

fn quoted(value: &str) -> Vec<&str> {
    value.split('"').skip(1).step_by(2).collect()
}

#[test]
fn p0_ci_02_dist_builds_the_spec_targets() {
    let config = read(DIST_CONFIG);
    let value = |key: &str| dist_value(&config, key).unwrap_or_else(|| panic!("no `{key}`"));
    assert_eq!(quoted(&value("targets")), TARGETS);
    assert_eq!(quoted(&value("installers")), ["shell"]);
    assert_eq!(value("github-attestations"), "true");
    assert_eq!(value("cargo-auditable"), "true");
    assert_eq!(quoted(&value("ci")), ["github"]);
    // release.yml is maintained by hand from dist's output.
    assert_eq!(quoted(&value("allow-dirty")), ["ci"]);
    // Each target has a runner, and the runners are the spec's.
    for target in TARGETS {
        let runner = dist_value(&config, target)
            .unwrap_or_else(|| panic!("no runner for {target} in [dist.github-custom-runners]"));
        let runner = quoted(&runner)[0].to_owned();
        assert!(
            ["macos-26", "ubuntu-24.04", "ubuntu-24.04-arm"].contains(&runner.as_str()),
            "{target} builds on {runner}"
        );
    }
}

#[test]
fn p0_ci_02_dist_installs_from_its_pinned_installer() {
    let workflow = read(WORKFLOW);
    let env = |name: &str| {
        workflow_env(&workflow, name).unwrap_or_else(|| panic!("release.yml sets no {name}"))
    };
    let (version, sha256) = (env("DIST_VERSION"), env("DIST_INSTALLER_SHA256"));
    assert!(
        sha256.len() == 64 && sha256.chars().all(|c| c.is_ascii_hexdigit()),
        "DIST_INSTALLER_SHA256 is not a SHA-256: {sha256}"
    );
    // One dist: the version dist-workspace.toml and the justfile name.
    let config = read(DIST_CONFIG);
    assert_eq!(
        quoted(&dist_value(&config, "cargo-dist-version").unwrap()),
        [version.as_str()]
    );
    assert!(
        read("justfile").contains(&format!("cargo_dist_version := \"{version}\"")),
        "the justfile pins another dist than {version}"
    );

    // Every job that installs dist checks the installer's SHA-256 before it
    // runs it; the installer checks the archive it downloads.
    let jobs = jobs(&workflow);
    let installing: Vec<&Job> = jobs
        .iter()
        .filter(|j| j.lines.iter().any(|l| l.trim() == "- name: Install dist"))
        .collect();
    assert_eq!(
        installing.iter().map(|j| j.id.as_str()).collect::<Vec<_>>(),
        ["plan", "build-local-artifacts"]
    );
    for job in installing {
        let script = job.step_script("Install dist");
        let at = |needle: &str| {
            script.find(needle).unwrap_or_else(|| {
                panic!(
                    "job `{}`: Install dist has no `{needle}`:\n{script}",
                    job.id
                )
            })
        };
        at("/v$DIST_VERSION/cargo-dist-installer.sh");
        let check = at("$DIST_INSTALLER_SHA256");
        assert!(check < at("sha256sum -c"));
        assert!(
            at("sha256sum -c") < at("sh \"$installer\""),
            "job `{}` runs the installer before checking it",
            job.id
        );
    }
    assert!(
        !workflow.contains("cargo-dist-installer.sh | sh"),
        "release.yml pipes dist's installer into a shell unchecked"
    );
}

#[test]
fn p0_ci_02_publish_crates_outlasts_its_index_waits() {
    let jobs = jobs(&read(WORKFLOW));
    let publish = job(&jobs, "publish-crates");
    let timeout: u64 = publish
        .key("timeout-minutes")
        .and_then(|v| v.parse().ok())
        .unwrap();
    let script = publish.step_script("Publish to crates.io");
    let number_after = |prefix: &str, end: char| -> u64 {
        let rest = script
            .split(prefix)
            .nth(1)
            .unwrap_or_else(|| panic!("no `{prefix}`"));
        rest.split(end).next().unwrap().trim().parse().unwrap()
    };
    let (tries, pause) = (number_after("$(seq ", ')'), number_after("sleep ", ';'));
    // Each crate may wait for the index as long as its loop allows; the job
    // keeps as long again for cargo publish itself.
    let waits = PUBLISH_ORDER.len() as u64 * tries * pause;
    assert!(
        timeout * 60 >= 2 * waits,
        "publish-crates times out after {timeout} min; its index waits alone can take {} min",
        waits / 60
    );
}

#[test]
fn p0_ci_02_release_builds_use_the_repro_environment() {
    let jobs = jobs(&read(WORKFLOW));
    let build = job(&jobs, "build-local-artifacts");
    let position = |needle: &str| {
        build
            .lines
            .iter()
            .position(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("build-local-artifacts has no line with `{needle}`"))
    };
    let env_step = position("just repro-env target");
    assert!(
        build.lines[env_step].contains("$GITHUB_ENV"),
        "the repro environment goes into $GITHUB_ENV"
    );
    assert!(
        env_step < position("dist build"),
        "the repro environment is set before dist builds"
    );

    // `just repro` builds under the same recipe's output.
    let justfile = read("justfile");
    let repro: Vec<&str> = justfile
        .lines()
        .skip_while(|line| !line.starts_with("repro "))
        .skip(1)
        .take_while(|line| line.starts_with("    "))
        .collect();
    assert!(
        repro.iter().any(|line| line.contains("repro-env")),
        "`just repro` takes its environment from `just repro-env`"
    );

    // The dist profile is the release profile unchanged.
    let manifest = read("Cargo.toml");
    let profile: Vec<&str> = manifest
        .lines()
        .skip_while(|line| *line != "[profile.dist]")
        .skip(1)
        .take_while(|line| !line.starts_with('['))
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect();
    assert_eq!(profile, ["inherits = \"release\""]);
}

/// The keys of `.github/allowed_signers`: `principal [options] type base64`.
fn allowed_signers() -> Vec<(String, String)> {
    let mut keys = Vec::new();
    for (n, line) in read(ALLOWED_SIGNERS).lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split_whitespace().collect();
        let at = fields
            .iter()
            .position(|f| f.starts_with("ssh-") || f.starts_with("ecdsa-") || f.starts_with("sk-"))
            .unwrap_or_else(|| panic!("allowed_signers:{}: no key type", n + 1));
        assert!(at >= 1, "allowed_signers:{}: no principal", n + 1);
        let blob = fields
            .get(at + 1)
            .unwrap_or_else(|| panic!("allowed_signers:{}: no key", n + 1));
        assert!(
            blob.len() > 40
                && blob
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(&b)),
            "allowed_signers:{}: the key is not base64",
            n + 1
        );
        keys.push((fields[0].to_owned(), fields[at].to_owned()));
    }
    keys
}

#[test]
fn p0_ci_02_allowed_signers_lists_the_release_key() {
    let keys = allowed_signers();
    assert!(
        !keys.is_empty(),
        "{ALLOWED_SIGNERS} lists no key, so verify-tag would refuse every release tag"
    );
}

// --- verify-tag, run for real -------------------------------------------------

/// One throwaway release: an origin repository with a workspace on main, a
/// tag, and a clone checked out at the tag, as `actions/checkout` leaves it.
struct Sandbox {
    _temp: tempfile::TempDir,
    dir: PathBuf,
    vars: Vec<(OsString, OsString)>,
}

/// A release tag, good or departing from a good one in one way.
#[derive(Clone, Copy, PartialEq)]
enum Release {
    /// `v0.0.1`, signed by the listed key, on main, changelog assembled.
    Good,
    /// Another tag name on the good commit, signed by the listed key.
    Named(&'static str),
    /// Signed by a key that `allowed_signers` does not list.
    UnknownSigner,
    /// Annotated without a signature.
    Unsigned,
    /// A lightweight tag.
    Lightweight,
    /// On a commit of a side branch instead of main.
    OffMain,
    /// After tagging, main replaces the signer's key with another.
    Revoked,
    /// The changelog section still says Unreleased.
    Undated,
    /// A `changelog.d` fragment was left behind.
    Fragment,
}

impl Release {
    fn tag(self) -> &'static str {
        match self {
            Release::Named(tag) => tag,
            _ => "v0.0.1",
        }
    }
}

/// The path of `program` on `PATH`, or a panic that names it and how to get
/// it: a missing tool would otherwise surface as `Os { code: 2 }` from the
/// `output().unwrap()` of its first command.
fn tool(program: &str, how: &str) -> PathBuf {
    std::env::var_os("PATH")
        .iter()
        .flat_map(std::env::split_paths)
        .map(|dir| dir.join(program))
        .find(|path| path.is_file())
        .unwrap_or_else(|| panic!("install {program}: {how}"))
}

/// The tools a sandbox runs, each with how to get it, and git's oldest
/// version with SSH signatures.
const SANDBOX_TOOLS: &[(&str, &str)] = &[
    (
        "git",
        "git 2.34 or later (Xcode's command line tools; apt-get install git)",
    ),
    (
        "ssh-keygen",
        "the OpenSSH client (it ships with macOS; apt-get install openssh-client)",
    ),
    ("bash", "bash (apt-get install bash)"),
    (
        "jq",
        "jq (it ships with macOS 15 and later; apt-get install jq)",
    ),
    ("cargo", "rustup (CONTRIBUTING.md)"),
];
const GIT_SSH_SIGNING: (u32, u32) = (2, 34);

impl Sandbox {
    fn new() -> Sandbox {
        for (program, how) in SANDBOX_TOOLS {
            tool(program, how);
        }
        let version = Command::new("git").arg("--version").output().unwrap();
        let version = String::from_utf8_lossy(&version.stdout).into_owned();
        let numbers: Vec<u32> = version
            .split_whitespace()
            .nth(2)
            .unwrap_or_default()
            .split('.')
            .take(2)
            .filter_map(|n| n.parse().ok())
            .collect();
        assert!(
            numbers.len() == 2 && (numbers[0], numbers[1]) >= GIT_SSH_SIGNING,
            "install git 2.34 or later, for SSH signatures: this is {}",
            version.trim()
        );
        let temp = tempfile::tempdir().unwrap();
        let dir = fs::canonicalize(temp.path()).unwrap();
        let home = dir.join("home");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(dir.join("runner-temp")).unwrap();
        // No automatic maintenance: receive-pack would start a detached `git
        // maintenance run --auto` in origin.git that is still running when the
        // test clones it.
        fs::write(
            dir.join("gitconfig"),
            "[user]\n\tname = Release Test\n\temail = release-test@example.invalid\n\
             [init]\n\tdefaultBranch = main\n[advice]\n\tdetachedHead = false\n\
             [maintenance]\n\tauto = false\n[receive]\n\tautogc = false\n",
        )
        .unwrap();
        // HOME and the git configuration are private; cargo keeps its
        // toolchains, which rustup finds from these two variables.
        let real_home = std::env::var_os("HOME").map(PathBuf::from).unwrap();
        let rustup_home =
            std::env::var_os("RUSTUP_HOME").unwrap_or(real_home.join(".rustup").into());
        let cargo_home = std::env::var_os("CARGO_HOME").unwrap_or(real_home.join(".cargo").into());
        let vars = vec![
            ("HOME".into(), home.into()),
            ("GIT_CONFIG_GLOBAL".into(), dir.join("gitconfig").into()),
            ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
            ("RUSTUP_HOME".into(), rustup_home),
            ("CARGO_HOME".into(), cargo_home),
            ("CARGO_NET_OFFLINE".into(), "true".into()),
            ("RUNNER_TEMP".into(), dir.join("runner-temp").into()),
        ];
        Sandbox {
            _temp: temp,
            dir,
            vars,
        }
    }

    fn command(&self, program: &str, cwd: &Path) -> Command {
        let mut command = Command::new(program);
        for name in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
            "GIT_OBJECT_DIRECTORY",
        ] {
            command.env_remove(name);
        }
        command.envs(self.vars.iter().cloned()).current_dir(cwd);
        command
    }

    fn run(&self, cwd: &Path, program: &str, args: &[&str]) -> String {
        let out = self.command(program, cwd).args(args).output().unwrap();
        assert!(
            out.status.success(),
            "{program} {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    fn git(&self, cwd: &Path, args: &[&str]) -> String {
        self.run(cwd, "git", args)
    }

    fn path(&self, name: &str) -> String {
        self.dir.join(name).to_str().unwrap().to_owned()
    }

    /// A new ed25519 key; returns the private key's path and the public key
    /// (type and base64, without the comment).
    fn key(&self, name: &str) -> (String, String) {
        let path = self.path(name);
        self.run(
            &self.dir,
            "ssh-keygen",
            &["-q", "-t", "ed25519", "-N", "", "-C", name, "-f", &path],
        );
        let public = fs::read_to_string(format!("{path}.pub")).unwrap();
        let public: Vec<&str> = public.split_whitespace().take(2).collect();
        (path, public.join(" "))
    }

    fn write(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    /// Commits the release's workspace to main in `work`: a two-member
    /// workspace (the published crate at the release version, an unpublished
    /// one at another version, which verify-tag ignores), the changelog and
    /// `allowed_signers`.
    fn commit_workspace(&self, work: &Path, release: Release, signers: &str) {
        let write = |name: &str, contents: &str| Sandbox::write(&work.join(name), contents);
        write(
            "Cargo.toml",
            "[workspace]\nmembers = [\"probe\", \"kit\"]\nresolver = \"3\"\n",
        );
        write(
            "probe/Cargo.toml",
            "[package]\nname = \"probe\"\nversion = \"0.0.1\"\nedition = \"2024\"\n",
        );
        write("probe/src/lib.rs", "");
        write(
            "kit/Cargo.toml",
            "[package]\nname = \"kit\"\nversion = \"9.9.9\"\nedition = \"2024\"\npublish = false\n",
        );
        write("kit/src/lib.rs", "");
        self.run(
            work,
            "cargo",
            &["generate-lockfile", "--offline", "--quiet"],
        );
        let date = if release == Release::Undated {
            "Unreleased"
        } else {
            "2026-10-25"
        };
        write(
            "CHANGELOG.md",
            &format!("# Changelog\n\n## [0.0.1] - {date}\n\n### Added\n\n- A probe.\n"),
        );
        write("changelog.d/README.md", "The fragment format.\n");
        if release == Release::Fragment {
            write("changelog.d/P9-X-01.md", "### Added\n\n- Late.\n");
        }
        write(".github/allowed_signers", signers);
        self.git(work, &["add", "-A"]);
        self.git(work, &["commit", "-q", "-m", "A release"]);
    }

    /// Builds `release` and runs release.yml's verify-tag script on it.
    fn verify(release: Release) -> Output {
        let sb = Sandbox::new();
        let (key_a, public_a) = sb.key("release-a");
        let (key_b, public_b) = sb.key("release-b");
        let signers =
            |public: &str| format!("# Release keys.\nrelease namespaces=\"git\" {public}\n");
        let (origin, work, ci) = (
            sb.path("origin.git"),
            sb.dir.join("work"),
            sb.dir.join("ci"),
        );
        sb.git(&sb.dir, &["init", "-q", "--bare", &origin]);
        sb.git(&sb.dir, &["init", "-q", work.to_str().unwrap()]);
        sb.git(&work, &["remote", "add", "origin", &origin]);
        sb.commit_workspace(&work, release, &signers(&public_a));

        let tag = release.tag();
        if release == Release::OffMain {
            sb.git(&work, &["switch", "-q", "-c", "side"]);
            sb.git(&work, &["commit", "-q", "--allow-empty", "-m", "Off main"]);
        }
        let key = if release == Release::UnknownSigner {
            &key_b
        } else {
            &key_a
        };
        let signing_key = format!("user.signingKey={key}");
        match release {
            Release::Lightweight => sb.git(&work, &["tag", tag]),
            Release::Unsigned => sb.git(&work, &["tag", "-a", "-m", tag, tag]),
            _ => sb.git(
                &work,
                &[
                    "-c",
                    "gpg.format=ssh",
                    "-c",
                    &signing_key,
                    "tag",
                    "-s",
                    "-m",
                    tag,
                    tag,
                ],
            ),
        };
        if release == Release::OffMain {
            sb.git(&work, &["switch", "-q", "main"]);
        }
        if release == Release::Revoked {
            Sandbox::write(&work.join(".github/allowed_signers"), &signers(&public_b));
            sb.git(&work, &["commit", "-q", "-am", "Rotate the release key"]);
        }
        let tag_ref = format!("refs/tags/{tag}");
        sb.git(&work, &["push", "-q", "origin", "main", &tag_ref]);

        // The runner's checkout: a clone at the tagged commit, fetched through
        // upload-pack as actions/checkout fetches. A local clone copies
        // origin.git's object files instead, and one vanished mid-copy on CI.
        sb.git(
            &sb.dir,
            &["clone", "-q", "--no-local", &origin, ci.to_str().unwrap()],
        );
        sb.git(&ci, &["checkout", "-q", "--detach", &tag_ref]);
        let script = job(&jobs(&read(WORKFLOW)), "verify-tag").step_script("Verify the tag");
        let path = sb.dir.join("verify-tag.sh");
        fs::write(&path, script).unwrap();
        // As GitHub runs `shell: bash`.
        sb.command("bash", &ci)
            .args(["--noprofile", "--norc", "-eo", "pipefail"])
            .arg(&path)
            .env("TAG", tag)
            .output()
            .unwrap()
    }
}

/// Runs a release through verify-tag and asserts the outcome; a refusal must
/// carry `reason` in its error annotation.
fn check(release: Release, reason: Option<&str>) {
    let out = Sandbox::verify(release);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let name = release.tag();
    match reason {
        None => assert!(
            out.status.success(),
            "{name}: verify-tag refused a good tag\nstdout: {stdout}\nstderr: {stderr}"
        ),
        Some(reason) => {
            assert!(
                !out.status.success(),
                "verify-tag accepted a tag it must refuse ({reason})\nstdout: {stdout}"
            );
            let error = stdout
                .lines()
                .find(|line| line.starts_with("::error"))
                .unwrap_or_else(|| {
                    panic!("{name}: no error annotation\nstdout: {stdout}\nstderr: {stderr}")
                });
            assert!(
                error.contains(reason),
                "{name}: the error does not say `{reason}`: {error}\nstderr: {stderr}"
            );
        }
    }
}

#[test]
fn p0_ci_02_verify_tag_accepts_a_signed_tag_on_main() {
    check(Release::Good, None);
}

#[test]
fn p0_ci_02_verify_tag_refuses_a_signer_outside_allowed_signers() {
    let refused = Some("not signed by a key in .github/allowed_signers");
    check(Release::UnknownSigner, refused);
    check(Release::Unsigned, refused);
    check(Release::Revoked, refused);
    check(Release::Lightweight, Some("lightweight tag"));
}

#[test]
fn p0_ci_02_verify_tag_refuses_a_tag_off_main_or_off_version() {
    check(Release::OffMain, Some("which is not on main"));
    check(
        Release::Named("v0.0.2"),
        Some("these crates have another version: probe 0.0.1"),
    );
    check(
        Release::Named("v1"),
        Some("is not a vMAJOR.MINOR.PATCH tag"),
    );
}

#[test]
fn p0_ci_02_verify_tag_refuses_an_unassembled_changelog() {
    check(
        Release::Undated,
        Some("CHANGELOG.md has no dated section for 0.0.1"),
    );
    check(
        Release::Fragment,
        Some("changelog.d still holds fragments (changelog.d/P9-X-01.md)"),
    );
}
