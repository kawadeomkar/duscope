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

/// `schemas/envelope/` at the repository root.
fn envelope_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schemas/envelope")
}

/// Where `old`, a schema of a published minor, says more than `new`, its
/// verb's schema now, one line each: a property, a `$defs` entry, a
/// required name or an enumerated value `new` dropped, or a type it
/// changed. The envelope changes additively only, so an old schema is a
/// subset of the new one; `x-*` keywords and descriptions are free to
/// change.
fn not_subset(at: &str, old: &serde_json::Value, new: &serde_json::Value, out: &mut Vec<String>) {
    use serde_json::Value;
    let (Value::Object(old), Value::Object(new)) = (old, new) else {
        if old != new {
            out.push(format!("{at}: {old} became {new}"));
        }
        return;
    };
    for (keyword, was) in old {
        if keyword.starts_with("x-") || keyword == "description" || keyword == "title" {
            continue;
        }
        let here = format!("{at}/{keyword}");
        let Some(is) = new.get(keyword) else {
            out.push(format!("{here}: dropped"));
            continue;
        };
        match keyword.as_str() {
            "properties" | "$defs" => {
                let (Value::Object(was), Value::Object(is)) = (was, is) else {
                    out.push(format!("{here}: not an object"));
                    continue;
                };
                for (name, schema) in was {
                    match is.get(name) {
                        Some(current) => {
                            not_subset(&format!("{here}/{name}"), schema, current, out);
                        }
                        None => out.push(format!("{here}/{name}: dropped")),
                    }
                }
            }
            "required" | "enum" => {
                let (Value::Array(was), Value::Array(is)) = (was, is) else {
                    out.push(format!("{here}: not an array"));
                    continue;
                };
                for value in was.iter().filter(|v| !is.contains(v)) {
                    out.push(format!("{here}: {value} dropped"));
                }
            }
            "items" | "additionalProperties" | "propertyNames" => not_subset(&here, was, is, out),
            "anyOf" | "oneOf" | "allOf" => {
                let (Value::Array(was), Value::Array(is)) = (was, is) else {
                    out.push(format!("{here}: not an array"));
                    continue;
                };
                for (i, schema) in was.iter().enumerate() {
                    match is.get(i) {
                        Some(current) => not_subset(&format!("{here}/{i}"), schema, current, out),
                        None => out.push(format!("{here}/{i}: dropped")),
                    }
                }
            }
            _ if was != is => out.push(format!("{here}: {was} became {is}")),
            _ => {}
        }
    }
}

#[test]
fn p2_cli_01_every_published_minor_is_a_subset_of_the_current_schema() {
    let history = envelope_dir().join("history");
    let mut problems = Vec::new();
    let mut checked = 0;
    // Each minor's directory, and in it the schema files: nothing else a
    // file manager may leave there.
    let entries = |dir: &Path| -> Vec<std::path::PathBuf> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect()
    };
    for minor in entries(&history).into_iter().filter(|p| p.is_dir()) {
        let schemas = entries(&minor)
            .into_iter()
            .filter(|p| p.to_string_lossy().ends_with(".schema.json"));
        for file in schemas {
            let name = file.file_name().unwrap().to_string_lossy().into_owned();
            let old: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
            let Ok(text) = std::fs::read_to_string(envelope_dir().join(&name)) else {
                problems.push(format!("{}: the verb's schema is gone", file.display()));
                continue;
            };
            let new: serde_json::Value = serde_json::from_str(&text).unwrap();
            let mut lines = Vec::new();
            not_subset("", &old, &new, &mut lines);
            problems.extend(
                lines
                    .into_iter()
                    .map(|l| format!("{}: {l}", file.display())),
            );
            checked += 1;
        }
    }
    assert!(
        checked > 0,
        "no published minor under {}",
        history.display()
    );
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn p2_cli_01_the_subset_check_fails_a_dropped_field_and_passes_an_added_one() {
    let old = serde_json::json!({
        "type": "object",
        "required": ["a"],
        "properties": {"a": {"type": "string", "enum": ["x", "y"]}},
    });
    let added = serde_json::json!({
        "type": "object",
        "required": ["a", "b"],
        "properties": {"a": {"type": "string", "enum": ["x", "y", "z"]}, "b": {"type": "integer"}},
        "x-since": "0.3",
    });
    let mut lines = Vec::new();
    not_subset("", &old, &added, &mut lines);
    assert_eq!(lines, Vec::<String>::new());
    let dropped = serde_json::json!({
        "type": "object",
        "required": [],
        "properties": {"a": {"type": "integer", "enum": ["x"]}},
    });
    not_subset("", &old, &dropped, &mut lines);
    assert_eq!(
        lines,
        [
            "/properties/a/enum: \"y\" dropped",
            "/properties/a/type: \"string\" became \"integer\"",
            "/required: \"a\" dropped",
        ]
    );
}
