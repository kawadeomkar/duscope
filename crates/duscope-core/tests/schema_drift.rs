//! P0-REPO-05: the envelope schema checks that need no derive.
//!
//! The drift test itself, which compares the schema derived from each envelope
//! type with its file under `schemas/`, is a unit test in `src/schema_drift.rs`.
//! The envelope types derive `schemars::JsonSchema` only under `cfg(test)`, and
//! that cfg applies when the library is compiled as its own unit-test target,
//! not to the library an integration test such as this one links.
//!
//! This file holds what can run against the library as shipped: the check that
//! the schema crates stay dev-only, so the derive can never reach the binary.
//! P2-CLI-01's history checks, which read files only, belong here too.
//!
//! These tests read `cargo metadata` only; they never run the duscope binary.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

use cargo_metadata::{DependencyKind, MetadataCommand, Node, PackageId};

/// The crates that derive and check schemas: test-only by design (D36).
const SCHEMA_CRATES: &[&str] = &["schemars", "schemars_derive", "jsonschema"];

/// The host triple of the cargo running the tests.
fn host() -> String {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    let output = Command::new(cargo).arg("-vV").output().unwrap();
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .expect("cargo -vV prints the host")
        .to_owned()
}

#[test]
fn p0_repo_05_schema_crates_stay_dev_only() {
    let metadata = MetadataCommand::new()
        .manifest_path(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml"))
        .other_options(vec!["--filter-platform".to_owned(), host()])
        .exec()
        .unwrap();
    let names: BTreeMap<&PackageId, &str> = metadata
        .packages
        .iter()
        .map(|p| (&p.id, p.name.as_str()))
        .collect();
    let nodes: BTreeMap<&PackageId, &Node> = metadata
        .resolve
        .as_ref()
        .expect("cargo metadata resolved the graph")
        .nodes
        .iter()
        .map(|node| (&node.id, node))
        .collect();

    // The derive needs schemars as a dev-dependency of duscope-core.
    let core = metadata
        .workspace_packages()
        .into_iter()
        .find(|p| p.name.as_str() == "duscope-core")
        .unwrap();
    assert!(
        core.dependencies
            .iter()
            .any(|d| d.name == "schemars" && d.kind == DependencyKind::Development),
        "schemars is not a dev-dependency of duscope-core"
    );

    // Everything a published crate builds with, through normal and build edges.
    let published: Vec<&PackageId> = metadata
        .workspace_packages()
        .into_iter()
        .filter(|p| p.publish.as_ref().is_none_or(|r| !r.is_empty()))
        .map(|p| &p.id)
        .collect();
    assert!(published.len() >= 6, "the published crates: {published:?}");
    let mut shipped = BTreeSet::new();
    let mut queue = published;
    while let Some(id) = queue.pop() {
        if shipped.insert(id) {
            queue.extend(
                nodes[id]
                    .deps
                    .iter()
                    .filter(|dep| {
                        dep.dep_kinds
                            .iter()
                            .any(|k| k.kind != DependencyKind::Development)
                    })
                    .map(|dep| &dep.pkg),
            );
        }
    }
    let leaked: Vec<&str> = shipped
        .iter()
        .map(|id| names[id])
        .filter(|name| SCHEMA_CRATES.contains(name))
        .collect();
    assert!(
        leaked.is_empty(),
        "schema crates reachable from a published crate through normal or build dependencies: \
         {leaked:?}; they must stay dev-dependencies, with the derive under cfg_attr(test, ...)"
    );
}
