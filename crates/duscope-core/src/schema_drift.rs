//! P0-REPO-05: the schema drift test.
//!
//! Every schema under `schemas/` that describes a Rust type must equal the
//! schema `schemars` derives from that type, once `x-*` extension keywords are
//! stripped from both. The file is hand-maintained: a change to the type
//! without a matching change to the file fails here, and the failure prints the
//! derived schema to copy from.
//!
//! This is a unit-test module rather than `tests/schema_drift.rs`. The envelope
//! types derive `JsonSchema` under `#[cfg_attr(test, ...)]`, so that `schemars`
//! stays a dev-dependency and never reaches the binary. `cfg(test)` is set only
//! when the library is compiled as its own unit-test target; an integration
//! test links the library compiled without it, where the derive does not exist.
//! `tests/schema_drift.rs` holds the checks that need no derive.
//!
//! Derived schemas use the serialize contract (`SchemaSettings::for_serialize`):
//! a fixture describes what duscope prints, so a nullable field is required and
//! may be null, never absent. `$id` comes from the file name, because one type
//! may back more than one file (a verb's envelope and its MCP tool output).

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use schemars::generate::SchemaSettings;
use serde_json::Value;

use crate::envelope::{
    ByTypeEntry, CacheEntry, CheckedEntry, DiffEntry, DiffEnvelope, Envelope, ErrorEnvelope,
    Explain, Listing, ObjectEnvelope, ProjectEntry, PrunedEntry, SavedScanEntry, ScanErrorEntry,
    SchemaInfo, SpilledEnvelope, SqlQueryBlock, SqlResult, StaleEntry, Summary, TopEntry,
    TreeEntry,
};

/// Each envelope schema file: its path under `schemas/`, and the schema derived
/// from its type now: one file per verb, the error envelope, and the stub
/// `--max-chars` prints. MCP tool output files need no entry here: each is
/// checked against its verb's envelope (`tool_output_schemas`).
fn derived_schemas() -> Vec<(&'static str, Value)> {
    vec![
        ("envelope/scan.schema.json", derive::<Envelope<TopEntry>>()),
        ("envelope/top.schema.json", derive::<Envelope<TopEntry>>()),
        ("envelope/tree.schema.json", derive::<Envelope<TreeEntry>>()),
        (
            "envelope/by-type.schema.json",
            derive::<Envelope<ByTypeEntry>>(),
        ),
        (
            "envelope/stale.schema.json",
            derive::<Envelope<StaleEntry>>(),
        ),
        (
            "envelope/caches.schema.json",
            derive::<Envelope<CachesRow>>(),
        ),
        (
            "envelope/summary.schema.json",
            derive::<ObjectEnvelope<Summary>>(),
        ),
        (
            "envelope/diff.schema.json",
            derive::<DiffEnvelope<'static, Envelope<DiffEntry>>>(),
        ),
        ("envelope/explain.schema.json", derive::<ExplainOutput>()),
        (
            "envelope/query.schema.json",
            derive::<ObjectEnvelope<SqlResult, SqlQueryBlock>>(),
        ),
        ("envelope/scans.schema.json", derive::<Listing<ScansRow>>()),
        (
            "envelope/schema.schema.json",
            derive::<ObjectEnvelope<SchemaInfo>>(),
        ),
        ("envelope/error.schema.json", derive::<ErrorEnvelope>()),
        (
            "envelope/spilled.schema.json",
            derive::<SpilledEnvelope<Value>>(),
        ),
    ]
}

// The verbs that print more than one shape, as their schemas list them: a
// row of either type, or an envelope of either kind. Plain comments, not doc
// comments, which would become the schemas' descriptions.

// `caches`: a cache, or with `--by-project` a project.
#[derive(JsonSchema)]
#[serde(untagged)]
#[allow(dead_code, clippy::large_enum_variant)]
enum CachesRow {
    Cache(CacheEntry),
    Project(ProjectEntry),
}

// `explain`: one path's facts, or with `--errors` what a scan could not
// read.
#[derive(JsonSchema)]
#[serde(untagged)]
#[allow(dead_code, clippy::large_enum_variant)]
enum ExplainOutput {
    Facts(ObjectEnvelope<Explain>),
    Errors(Envelope<ScanErrorEntry>),
}

// `scans`: the saved scans, or with `--prune` the files removed, or with
// `--check` the scans found sound.
#[derive(JsonSchema)]
#[serde(untagged)]
#[allow(dead_code, clippy::large_enum_variant)]
enum ScansRow {
    Saved(SavedScanEntry),
    Pruned(PrunedEntry),
    Checked(CheckedEntry),
}

/// The envelope file an MCP tool output file must equal, apart from `$id`. A
/// tool's `structuredContent` is byte-identical to `duscope <verb> --json`
/// (P3-MCP-03), and the verb is the tool's name without `duscope_`, with `-`
/// for `_`: `mcp/duscope_by_type.output.json` is the `by-type` envelope.
fn tool_envelope(file: &str) -> Option<String> {
    let tool = file.strip_prefix("mcp/")?.strip_suffix(".output.json")?;
    let verb = tool.strip_prefix("duscope_")?.replace('_', "-");
    Some(format!("envelope/{verb}.schema.json"))
}

/// Each MCP tool output file among `files` with the schema derived for its
/// verb's envelope, and a problem for each whose envelope has no type, so a
/// tool package adds its output file without touching this module.
fn tool_output_schemas(
    files: &[String],
    envelopes: &[(&str, Value)],
) -> (Vec<(String, Value)>, Vec<String>) {
    let mut schemas = Vec::new();
    let mut problems = Vec::new();
    for file in files.iter().filter(|f| f.starts_with("mcp/")) {
        let envelope = tool_envelope(file);
        match envelopes
            .iter()
            .find(|(name, _)| Some(*name) == envelope.as_deref())
        {
            Some((_, derived)) => schemas.push((file.clone(), derived.clone())),
            None => problems.push(match envelope {
                Some(envelope) => format!(
                    "schemas/{file}: its verb's envelope, schemas/{envelope}, has no type in \
                     derived_schemas()"
                ),
                None => format!("schemas/{file}: not named mcp/duscope_<tool>.output.json"),
            }),
        }
    }
    (schemas, problems)
}

/// The `schemas/` directory at the repository root.
fn schemas_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schemas")
}

/// The JSON Schema 2020-12 document `schemars` derives for `T` as serialised.
fn derive<T: JsonSchema>() -> Value {
    SchemaSettings::draft2020_12()
        .for_serialize()
        .into_generator()
        .into_root_schema_for::<T>()
        .to_value()
}

/// Keywords whose value maps names to subschemas: their keys are names, so an
/// `x-` prefix there is part of a name, not an extension keyword.
const NAMED_SUBSCHEMAS: &[&str] = &[
    "properties",
    "patternProperties",
    "$defs",
    "definitions",
    "dependentSchemas",
];

/// Keywords whose value is instance data rather than a schema.
const INSTANCE_DATA: &[&str] = &["const", "enum", "default", "examples"];

/// Removes every `x-*` extension keyword (`x-since`, `x-deprecated`, ...) from a
/// schema and all its subschemas, leaving property names and instance data
/// alone.
fn strip_extensions(value: &mut Value) {
    match value {
        Value::Object(schema) => {
            schema.retain(|keyword, _| !keyword.starts_with("x-"));
            for (keyword, value) in schema.iter_mut() {
                if INSTANCE_DATA.contains(&keyword.as_str()) {
                    continue;
                }
                match value {
                    Value::Object(named) if NAMED_SUBSCHEMAS.contains(&keyword.as_str()) => {
                        named.values_mut().for_each(strip_extensions);
                    }
                    _ => strip_extensions(value),
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(strip_extensions),
        _ => {}
    }
}

/// One JSON Pointer reference token (RFC 6901).
fn pointer_token(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

/// Where `file` and `derived` differ, one `<JSON pointer>: <difference>` line
/// each.
fn differences(pointer: &str, file: &Value, derived: &Value, out: &mut Vec<String>) {
    match (file, derived) {
        (Value::Object(file), Value::Object(derived)) => {
            let keys: BTreeSet<&String> = file.keys().chain(derived.keys()).collect();
            for key in keys {
                let at = format!("{pointer}/{}", pointer_token(key));
                match (file.get(key), derived.get(key)) {
                    (Some(a), Some(b)) => differences(&at, a, b, out),
                    (Some(a), None) => out.push(format!("{at}: only in the file ({a})")),
                    (None, _) => out.push(format!("{at}: only in the derived schema")),
                }
            }
        }
        (Value::Array(a), Value::Array(b)) if a.len() == b.len() => {
            for (i, (a, b)) in a.iter().zip(b).enumerate() {
                differences(&format!("{pointer}/{i}"), a, b, out);
            }
        }
        _ if file == derived => {}
        _ => out.push(format!(
            "{}: {file} in the file, {derived} derived",
            if pointer.is_empty() { "/" } else { pointer }
        )),
    }
}

/// The drift between a schema file and the schema derived from its type, as
/// readable lines; empty when they agree once `x-*` keywords are stripped.
/// `name` is the file's path under `schemas/`, whose file name is the `$id`.
fn drift(name: &str, file_text: &str, derived: &Value) -> Vec<String> {
    let mut file: Value = match serde_json::from_str(file_text) {
        Ok(file) => file,
        Err(e) => return vec![format!("{name} is not JSON: {e}")],
    };
    let mut derived = derived.clone();
    let id = name.rsplit('/').next().unwrap_or(name);
    if let Value::Object(schema) = &mut derived {
        schema.insert("$id".to_owned(), Value::from(id));
    }
    strip_extensions(&mut file);
    strip_extensions(&mut derived);
    let mut lines = Vec::new();
    differences("", &file, &derived, &mut lines);
    if lines.is_empty() {
        return lines;
    }
    let pretty = serde_json::to_string_pretty(&derived).unwrap_or_default();
    lines.insert(
        0,
        format!("schemas/{name} differs from the schema derived from its type:"),
    );
    lines.push(format!(
        "Update the file to this schema, keeping its own x-* keywords:\n{pretty}"
    ));
    lines
}

#[test]
fn p0_repo_05_derived_schemas_equal_their_files() {
    let envelopes = derived_schemas();
    let (tool_outputs, mut problems) = tool_output_schemas(&files_derived_from_types(), &envelopes);
    let all = envelopes
        .into_iter()
        .map(|(name, derived)| (name.to_owned(), derived))
        .chain(tool_outputs);
    for (name, derived) in all {
        let path = schemas_dir().join(&name);
        match fs::read_to_string(&path) {
            Ok(text) => problems.extend(drift(&name, &text, &derived)),
            Err(e) => problems.push(format!("{}: {e}", path.display())),
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// The schema files that must come from a type: each verb's envelope and each
/// MCP tool's output. MCP input schemas are hand-written against the clap
/// definitions (`schemas/mcp/MAPPING.md`), and `history/` keeps published
/// versions, which no current type derives.
fn files_derived_from_types() -> Vec<String> {
    let mut files = Vec::new();
    for (dir, suffix) in [("envelope", ".schema.json"), ("mcp", ".output.json")] {
        let Ok(entries) = fs::read_dir(schemas_dir().join(dir)) else {
            continue;
        };
        for entry in entries {
            let name = entry.unwrap().file_name().to_string_lossy().into_owned();
            if name.ends_with(suffix) {
                files.push(format!("{dir}/{name}"));
            }
        }
    }
    files.sort();
    files
}

#[test]
fn p0_repo_05_every_derived_file_has_its_type() {
    let envelopes = derived_schemas();
    let registered: BTreeSet<&str> = envelopes.iter().map(|(name, _)| *name).collect();
    let files = files_derived_from_types();
    assert!(
        files.iter().any(|f| f.starts_with("envelope/")),
        "no envelope schema under {}: {files:?}",
        schemas_dir().display()
    );
    let unregistered: Vec<&String> = files
        .iter()
        .filter(|f| f.starts_with("envelope/") && !registered.contains(f.as_str()))
        .collect();
    assert!(
        unregistered.is_empty(),
        "these schema files have no type in derived_schemas(), so no drift test covers them: \
         {unregistered:?}"
    );
    let (_, problems) = tool_output_schemas(&files, &envelopes);
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn p0_repo_05_tool_outputs_take_their_verbs_envelope() {
    assert_eq!(
        tool_envelope("mcp/duscope_by_type.output.json").as_deref(),
        Some("envelope/by-type.schema.json")
    );
    assert_eq!(
        tool_envelope("mcp/duscope_top.output.json").as_deref(),
        Some("envelope/top.schema.json")
    );
    assert_eq!(tool_envelope("mcp/duscope_top.input.json"), None);
    assert_eq!(tool_envelope("mcp/top.output.json"), None);

    let envelopes = [("envelope/top.schema.json", derive::<Sample>())];
    let files = [
        "envelope/top.schema.json",
        "mcp/duscope_top.output.json",
        "mcp/duscope_stale.output.json",
        "mcp/top.output.json",
    ]
    .map(str::to_owned);
    let (schemas, problems) = tool_output_schemas(&files, &envelopes);
    assert_eq!(
        schemas,
        [("mcp/duscope_top.output.json".to_owned(), derive::<Sample>())]
    );
    assert_eq!(
        problems,
        [
            "schemas/mcp/duscope_stale.output.json: its verb's envelope, \
             schemas/envelope/stale.schema.json, has no type in derived_schemas()",
            "schemas/mcp/top.output.json: not named mcp/duscope_<tool>.output.json",
        ]
    );

    // An output file is its envelope's schema under its own `$id`.
    let mut file = derive::<Sample>();
    file["$id"] = Value::from("duscope_top.output.json");
    assert_eq!(
        drift(
            "mcp/duscope_top.output.json",
            &file.to_string(),
            &derive::<Sample>()
        ),
        Vec::<String>::new()
    );
    assert!(
        !drift(
            "mcp/duscope_top.output.json",
            &file.to_string(),
            &derive::<SampleWithAField>()
        )
        .is_empty(),
        "an output file that lags its envelope's type fails"
    );
}

// A type for the drift check's own tests, kept apart from the registered types
// so that changing one of those fails only
// `p0_repo_05_derived_schemas_equal_their_files`. Plain comments, not doc
// comments: a doc comment would become the schema's description.
#[derive(JsonSchema)]
#[allow(dead_code)]
struct Sample {
    /// Whether the command succeeded.
    ok: bool,
    /// Where the next page starts, or null on the last page.
    next_cursor: Option<String>,
}

// `Sample` with one more field, as a change to the type would leave it.
#[derive(JsonSchema)]
#[schemars(rename = "Sample")]
#[allow(dead_code)]
struct SampleWithAField {
    /// Whether the command succeeded.
    ok: bool,
    /// Where the next page starts, or null on the last page.
    next_cursor: Option<String>,
    /// A field added without updating the file.
    added_count: u64,
}

/// The file `envelope/sample.schema.json` as `Sample` derives it.
fn sample_file() -> Value {
    let mut file = derive::<Sample>();
    file["$id"] = Value::from("sample.schema.json");
    file
}

#[test]
fn p0_repo_05_drift_check_fails_a_changed_type() {
    let name = "envelope/sample.schema.json";
    let text = sample_file().to_string();
    assert_eq!(
        drift(name, &text, &derive::<Sample>()),
        Vec::<String>::new()
    );

    let lines = drift(name, &text, &derive::<SampleWithAField>());
    assert_eq!(
        lines[..3],
        [
            "schemas/envelope/sample.schema.json differs from the schema derived from its type:",
            "/properties/added_count: only in the derived schema",
            "/required: [\"ok\",\"next_cursor\"] in the file, \
             [\"ok\",\"next_cursor\",\"added_count\"] derived",
        ],
        "{lines:#?}"
    );
    assert!(
        lines[3].starts_with("Update the file to this schema") && lines[3].contains("added_count"),
        "{lines:#?}"
    );

    let mut wrong_id = sample_file();
    wrong_id["$id"] = Value::from("top.schema.json");
    assert_eq!(
        drift(name, &wrong_id.to_string(), &derive::<Sample>())[1],
        "/$id: \"top.schema.json\" in the file, \"sample.schema.json\" derived"
    );
    assert_eq!(
        drift(name, "{", &derive::<Sample>()).len(),
        1,
        "a file that is not JSON is one problem"
    );
}

#[test]
fn p0_repo_05_drift_check_ignores_extension_keywords() {
    let mut file = sample_file();
    file["x-since"] = Value::from("0.2");
    file["properties"]["next_cursor"]["x-deprecated"] = Value::from("0.3");
    assert_eq!(
        drift(
            "envelope/sample.schema.json",
            &file.to_string(),
            &derive::<Sample>()
        ),
        Vec::<String>::new()
    );
}

#[test]
fn p0_repo_05_strip_removes_only_extension_keywords() {
    let mut schema = serde_json::json!({
        "x-since": "0.2",
        "type": "object",
        "properties": {
            "x-name": { "type": "string", "x-deprecated": "0.3" },
            "items": { "type": "array", "items": { "x-since": "0.2", "type": "integer" } }
        },
        "$defs": { "x-def": { "anyOf": [{ "x-since": "0.2" }, { "type": "null" }] } },
        "default": { "x-data": 1 },
        "examples": [{ "x-data": 2 }]
    });
    strip_extensions(&mut schema);
    assert_eq!(
        schema,
        serde_json::json!({
            "type": "object",
            "properties": {
                "x-name": { "type": "string" },
                "items": { "type": "array", "items": { "type": "integer" } }
            },
            "$defs": { "x-def": { "anyOf": [{}, { "type": "null" }] } },
            "default": { "x-data": 1 },
            "examples": [{ "x-data": 2 }]
        })
    );
}

#[test]
fn p2_cli_01_a_field_added_to_an_envelope_type_fails_the_drift_test() {
    let name = "envelope/top.schema.json";
    let text = fs::read_to_string(schemas_dir().join(name)).unwrap();
    assert_eq!(
        drift(name, &text, &derive::<Envelope<TopEntry>>()),
        Vec::<String>::new()
    );
    // The schema `TopEntry` would derive with one more field.
    let mut changed = derive::<Envelope<TopEntry>>();
    let row = &mut changed["$defs"]["TopEntry"];
    row["properties"]["added_bytes"] = serde_json::json!({"type": "integer"});
    row["required"]
        .as_array_mut()
        .unwrap()
        .push(Value::from("added_bytes"));
    let lines = drift(name, &text, &changed);
    assert!(
        lines
            .iter()
            .any(|l| l == "/$defs/TopEntry/properties/added_bytes: only in the derived schema"),
        "{lines:#?}"
    );
}
