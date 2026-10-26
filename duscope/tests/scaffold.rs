//! P0-REPO-01: the workspace shape and the placeholder binary.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use cargo_metadata::{DependencyKind, Metadata, MetadataCommand, Package};

/// The published crates and the workspace crates each may depend on, from the
/// spec's "Architecture and crate boundaries". Dev-dependencies keep the same
/// direction, with one exemption: the unpublished workspace members (the
/// testkit, the fixture generator), as dev-dependencies only.
const ALLOWED: &[(&str, &[&str])] = &[
    ("duscope-walk", &[]),
    ("duscope-core", &["duscope-walk"]),
    ("duscope-treemap", &["duscope-core"]),
    ("duscope-tui", &["duscope-treemap", "duscope-core"]),
    ("duscope-mcp", &["duscope-core"]),
    (
        "duscope",
        &[
            "duscope-walk",
            "duscope-core",
            "duscope-treemap",
            "duscope-tui",
            "duscope-mcp",
        ],
    ),
];

fn metadata() -> Metadata {
    MetadataCommand::new()
        .manifest_path(concat!(env!("CARGO_MANIFEST_DIR"), "/../Cargo.toml"))
        .no_deps()
        .exec()
        .unwrap()
}

fn is_published(package: &Package) -> bool {
    package
        .publish
        .as_ref()
        .is_none_or(|registries| !registries.is_empty())
}

#[test]
fn p0_repo_01_crate_dependency_direction() {
    let metadata = metadata();
    let packages = metadata.workspace_packages();
    let members: BTreeSet<&str> = packages.iter().map(|p| p.name.as_str()).collect();
    let allowed: BTreeMap<&str, &[&str]> = ALLOWED.iter().copied().collect();

    let published: BTreeSet<&str> = packages
        .iter()
        .filter(|p| is_published(p))
        .map(|p| p.name.as_str())
        .collect();
    assert_eq!(
        published,
        allowed.keys().copied().collect(),
        "the published workspace crates are the six of the architecture"
    );

    // A dev-dependency on a published crate stays in the packaged manifest, so
    // cargo resolves it from crates.io when the crate is published: one on a
    // crate published later in the order (or not yet at this version) fails
    // the release. An unpublished member is dropped when the crate is packaged,
    // but only while it carries no version requirement.
    let mut violations = Vec::new();
    for package in packages.iter().filter(|p| is_published(p)) {
        let ok = allowed[package.name.as_str()];
        for dep in &package.dependencies {
            if !members.contains(dep.name.as_str()) {
                continue;
            }
            let test_only =
                dep.kind == DependencyKind::Development && !published.contains(dep.name.as_str());
            if test_only && dep.req.to_string() != "*" {
                violations.push(format!(
                    "{} -> {} (an unpublished crate with the version requirement {})",
                    package.name, dep.name, dep.req
                ));
            } else if !test_only && !ok.contains(&dep.name.as_str()) {
                violations.push(format!("{} -> {} ({:?})", package.name, dep.name, dep.kind));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "dependency edges against the architecture: {violations:?}"
    );
}

#[test]
fn p0_repo_01_crates_share_the_workspace_version() {
    let metadata = metadata();
    let binary = metadata
        .workspace_packages()
        .into_iter()
        .find(|p| p.name.as_str() == "duscope")
        .unwrap();
    for package in metadata
        .workspace_packages()
        .iter()
        .filter(|p| is_published(p))
    {
        assert_eq!(
            package.version, binary.version,
            "{} is not at the workspace version",
            package.name
        );
    }
}

#[test]
fn p0_repo_01_published_crates_ship_a_readme_and_both_licences() {
    let metadata = metadata();
    let root = metadata.workspace_root.as_std_path();
    let read = |path: &Path| {
        fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    };
    let workspace_manifest = read(&root.join("Cargo.toml"));
    for file in ["/src", "/README.md", "/LICENSE-MIT", "/LICENSE-APACHE"] {
        assert!(
            workspace_manifest.contains(&format!("\"{file}\"")),
            "[workspace.package] include lacks {file}"
        );
    }
    for package in metadata
        .workspace_packages()
        .iter()
        .filter(|p| is_published(p))
    {
        let name = package.name.as_str();
        let dir = package.manifest_path.parent().unwrap().as_std_path();
        assert!(
            read(package.manifest_path.as_std_path()).contains("include.workspace = true"),
            "{name} does not take the workspace's include list"
        );
        assert!(
            package
                .readme
                .as_ref()
                .is_some_and(|readme| readme.as_str().ends_with("README.md")),
            "{name}: readme = {:?}",
            package.readme
        );
        // The licence files are symlinks to the root's; cargo packages their
        // contents.
        for licence in ["LICENSE-MIT", "LICENSE-APACHE"] {
            assert_eq!(
                read(&dir.join(licence)),
                read(&root.join(licence)),
                "{name}/{licence} differs from the root's"
            );
        }
        let readme = read(&dir.join("README.md"))
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if name == "duscope" {
            assert!(readme.contains("cargo install duscope --locked"), "{name}");
        } else {
            for promise in [
                "no stability promise before 1.0",
                "the `duscope` binary is the only supported entry point",
            ] {
                assert!(
                    readme.contains(promise),
                    "{name}/README.md does not say {promise:?}"
                );
            }
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

/// `just` on this workspace's justfile.
fn just() -> Command {
    let mut command = Command::new(tool(
        "just",
        "just, the task runner (CONTRIBUTING.md, Prerequisites)",
    ));
    command
        .arg("--justfile")
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/../justfile"));
    command
}

#[test]
fn p0_repo_01_test_pkg_quotes_the_id() {
    // The script `just test-pkg` would run, up to the filter it builds.
    let dry = just()
        .args(["--dry-run", "test-pkg", "P9-X'Y-01"])
        .output()
        .unwrap();
    let script = String::from_utf8_lossy(&dry.stderr);
    let head = script
        .lines()
        .take_while(|line| !line.starts_with("count="))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(head.contains("filter="), "{script}");
    let mut bash = Command::new("bash")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let input = format!("{head}\nprintf '%s' \"$filter\"\n");
    bash.stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let out = bash.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{script}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "p9_x'y_01_");
}

#[test]
fn p0_repo_01_repro_refuses_a_path_with_whitespace() {
    let output = just()
        .arg("repro")
        .env("CARGO_HOME", "/nonexistent/cargo home")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(
        stderr.contains("'/nonexistent/cargo home' contains whitespace"),
        "{stderr}"
    );
}

#[test]
fn p0_repo_01_version_prints_package_version() {
    let output = Command::new(env!("CARGO_BIN_EXE_duscope"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(output.status.success(), "exit status {}", output.status);
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("duscope {}\n", env!("CARGO_PKG_VERSION"))
    );
    assert!(output.stderr.is_empty());
}
