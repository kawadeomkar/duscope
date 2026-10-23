//! P0-REPO-05: the fixtures under `schemas/` and the directory's layout.
//!
//! A fixture is any `.json` file under `schemas/`, and each one must:
//! - parse as a JSON object
//! - declare `"$schema": "https://json-schema.org/draft/2020-12/schema"`
//! - pass the JSON Schema 2020-12 meta-schema, and compile as a validator, so
//!   every `$ref` resolves inside the file (nothing is fetched: the jsonschema
//!   crate is built without its HTTP and file resolvers)
//! - have an `$id` equal to its file name
//! - name every property with `[A-Za-z0-9_.-]{1,64}`, the pattern MCP clients
//!   enforce; Claude Code silently drops a tool whose schema breaks it
//! - sit where the layout puts it: `envelope/**/<verb>.schema.json`,
//!   `mcp/<tool>.input.json` or `mcp/<tool>.output.json`
//!
//! These tests read files only; they never run the duscope binary.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

const DRAFT_2020_12: &str = "https://json-schema.org/draft/2020-12/schema";

/// Keywords whose value maps names to subschemas.
const NAMED_SUBSCHEMAS: &[&str] = &[
    "properties",
    "patternProperties",
    "$defs",
    "definitions",
    "dependentSchemas",
];

/// Keywords whose value is instance data rather than a schema.
const INSTANCE_DATA: &[&str] = &["const", "enum", "default", "examples"];

fn schemas_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../schemas")
}

/// Every `.json` file under `dir`, as paths relative to `schemas/` with `/`
/// separators, sorted.
fn json_files(root: &Path, dir: &Path, out: &mut Vec<String>) {
    let mut entries: Vec<_> = fs::read_dir(dir).unwrap().map(|e| e.unwrap()).collect();
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        if entry.file_type().unwrap().is_dir() {
            json_files(root, &path, out);
        } else if path.extension().is_some_and(|e| e == "json") {
            let relative = path.strip_prefix(root).unwrap();
            let parts: Vec<String> = relative
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            out.push(parts.join("/"));
        }
    }
}

fn valid_property_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
}

/// The property names that break the pattern, anywhere in the schema, with
/// the JSON pointer of the `properties` keyword that names them.
fn bad_property_names(pointer: &str, value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(schema) => {
            for (keyword, value) in schema {
                let at = format!(
                    "{pointer}/{}",
                    keyword.replace('~', "~0").replace('/', "~1")
                );
                if INSTANCE_DATA.contains(&keyword.as_str()) {
                    continue;
                }
                match value {
                    Value::Object(named) if NAMED_SUBSCHEMAS.contains(&keyword.as_str()) => {
                        for (name, subschema) in named {
                            if keyword == "properties" && !valid_property_name(name) {
                                out.push(format!("{at}: {name:?}"));
                            }
                            let token = name.replace('~', "~0").replace('/', "~1");
                            bad_property_names(&format!("{at}/{token}"), subschema, out);
                        }
                    }
                    _ => bad_property_names(&at, value, out),
                }
            }
        }
        Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                bad_property_names(&format!("{pointer}/{i}"), item, out);
            }
        }
        _ => {}
    }
}

/// Whether the layout has a place for a fixture at `name` (relative to
/// `schemas/`).
fn in_layout(name: &str) -> bool {
    let parts: Vec<&str> = name.split('/').collect();
    match parts.as_slice() {
        ["envelope", .., file] => file.ends_with(".schema.json"),
        ["mcp", file] => file.ends_with(".input.json") || file.ends_with(".output.json"),
        _ => false,
    }
}

/// Everything wrong with the fixture at `name` (relative to `schemas/`) whose
/// content is `text`.
fn problems(name: &str, text: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let mut problem = |what: String| problems.push(format!("schemas/{name}: {what}"));
    if !in_layout(name) {
        problem(
            "not in the layout: envelope/**/<verb>.schema.json, mcp/<tool>.input.json or \
             mcp/<tool>.output.json"
                .to_owned(),
        );
    }
    let schema: Value = match serde_json::from_str(text) {
        Ok(Value::Object(schema)) => Value::Object(schema),
        Ok(_) => {
            problem("not a JSON object".to_owned());
            return problems;
        }
        Err(e) => {
            problem(format!("not JSON: {e}"));
            return problems;
        }
    };
    if schema.get("$schema") != Some(&Value::from(DRAFT_2020_12)) {
        problem(format!(
            "$schema is {}, not \"{DRAFT_2020_12}\"",
            schema.get("$schema").unwrap_or(&Value::Null)
        ));
    }
    let file_name = name.rsplit('/').next().unwrap_or(name);
    if schema.get("$id") != Some(&Value::from(file_name)) {
        problem(format!(
            "$id is {}, not the file name \"{file_name}\"",
            schema.get("$id").unwrap_or(&Value::Null)
        ));
    }
    let meta = jsonschema::draft202012::meta::validator();
    let mut errors: Vec<String> = Vec::new();
    for e in meta.iter_errors(&schema) {
        let error = format!(
            "fails the 2020-12 meta-schema at {:?}: {e}",
            e.instance_path().as_str()
        );
        // One mistake can fail several branches of the meta-schema alike.
        if !errors.contains(&error) {
            errors.push(error);
        }
    }
    if errors.is_empty() {
        if let Err(e) = jsonschema::draft202012::new(&schema) {
            problem(format!("does not compile as a validator: {e}"));
        }
    }
    errors.into_iter().for_each(&mut problem);
    let mut names = Vec::new();
    bad_property_names("", &schema, &mut names);
    for name in names {
        problem(format!(
            "property name breaks [A-Za-z0-9_.-]{{1,64}} at {name}"
        ));
    }
    problems
}

#[test]
fn p0_repo_05_fixtures_pass_the_schema_checks() {
    let root = schemas_dir();
    let mut names = Vec::new();
    json_files(&root, &root, &mut names);
    assert!(
        names.iter().any(|n| n.starts_with("envelope/")),
        "no envelope schema under {}: {names:?}",
        root.display()
    );
    let all: Vec<String> = names
        .iter()
        .flat_map(|name| problems(name, &fs::read_to_string(root.join(name)).unwrap()))
        .collect();
    eprintln!("p0_repo_05: checked {names:?}");
    assert!(all.is_empty(), "{}", all.join("\n"));
}

/// The fixture `envelope/top.schema.json` of the self-tests, valid as it is.
const TOP: &str = "envelope/top.schema.json";

fn valid_top() -> Value {
    serde_json::json!({
        "$schema": DRAFT_2020_12,
        "$id": "top.schema.json",
        "type": "object",
        "properties": { "ok": { "type": "boolean" }, "entries": { "$ref": "#/$defs/Entry" } },
        "$defs": { "Entry": { "type": "object", "properties": { "path": { "type": "string" } } } }
    })
}

/// The valid fixture with the value at `pointer` replaced.
fn with(pointer: &str, value: Value) -> String {
    let mut schema = valid_top();
    *schema.pointer_mut(pointer).unwrap() = value;
    schema.to_string()
}

/// The valid fixture without the top-level `key`.
fn without(key: &str) -> String {
    let mut schema = valid_top();
    schema.as_object_mut().unwrap().remove(key);
    schema.to_string()
}

/// Each case is a fixture's name, its text, and part of the problem it must get.
fn assert_rejected(cases: &[(&str, String, &str)]) {
    for (name, text, expected) in cases {
        let found = problems(name, text);
        assert!(
            found.iter().any(|p| p.contains(expected)),
            "{name} with {text}: expected a problem containing {expected:?}, got {found:#?}"
        );
    }
}

#[test]
fn p0_repo_05_schema_checks_accept_valid_files() {
    assert_eq!(
        problems(TOP, &valid_top().to_string()),
        Vec::<String>::new()
    );
    let input = with("/$id", Value::from("duscope_top.input.json"));
    assert_eq!(
        problems("mcp/duscope_top.input.json", &input),
        Vec::<String>::new()
    );
    let history = with("/$id", Value::from("top.schema.json"));
    assert_eq!(
        problems("envelope/history/0.2/top.schema.json", &history),
        Vec::<String>::new()
    );
    // Names at the edges of the pattern, and instance data, which names nothing.
    let names = with(
        "/properties",
        serde_json::json!({
            "a.b-c_D9": { "default": { "has space": 1 } },
            "x".repeat(64): { "examples": [{ "has space": 1 }] }
        }),
    );
    assert_eq!(problems(TOP, &names), Vec::<String>::new());
}

#[test]
fn p0_repo_05_schema_checks_reject_invalid_files() {
    let draft_07 = "http://json-schema.org/draft-07/schema#";
    let dangling = with("/properties/entries/$ref", Value::from("#/$defs/Missing"));
    let remote = with(
        "/properties/entries/$ref",
        Value::from("https://example.invalid/entry.json"),
    );
    assert_rejected(&[
        (TOP, "{".to_owned(), "not JSON"),
        (TOP, "[]".to_owned(), "not a JSON object"),
        (
            TOP,
            with("/type", Value::from("strng")),
            "fails the 2020-12 meta-schema at \"/type\"",
        ),
        (
            TOP,
            with("/properties/ok", Value::from(1)),
            "fails the 2020-12 meta-schema at \"/properties/ok\"",
        ),
        (TOP, without("$schema"), "$schema is null"),
        (
            TOP,
            with("/$schema", Value::from(draft_07)),
            "$schema is \"http://json-schema.org/draft-07/schema#\"",
        ),
        (
            TOP,
            without("$id"),
            "$id is null, not the file name \"top.schema.json\"",
        ),
        (
            TOP,
            with("/$id", Value::from("tree.schema.json")),
            "$id is \"tree.schema.json\"",
        ),
        (TOP, dangling, "does not compile as a validator"),
        (TOP, remote, "does not compile as a validator"),
    ]);
}

#[test]
fn p0_repo_05_schema_checks_reject_bad_names_and_places() {
    let nested = serde_json::json!({ "type": "object", "properties": { "has space": {} } });
    let long = serde_json::json!({ "x".repeat(65): {} });
    assert_rejected(&[
        (
            TOP,
            with("/properties/ok", nested),
            "breaks [A-Za-z0-9_.-]{1,64} at /properties/ok/properties: \"has space\"",
        ),
        (
            TOP,
            with("/$defs/Entry/properties", long),
            "at /$defs/Entry/properties: \"xxxx",
        ),
        (
            TOP,
            with("/properties", serde_json::json!({ "": {} })),
            "at /properties: \"\"",
        ),
        (
            "top.schema.json",
            with("/$id", Value::from("top.schema.json")),
            "not in the layout",
        ),
        (
            "envelope/top.json",
            with("/$id", Value::from("top.json")),
            "not in the layout",
        ),
        (
            "mcp/duscope_top.json",
            with("/$id", Value::from("duscope_top.json")),
            "not in the layout",
        ),
        (
            "mcp/tools/duscope_top.input.json",
            with("/$id", Value::from("duscope_top.input.json")),
            "not in the layout",
        ),
        (
            "index/v2.json",
            with("/$id", Value::from("v2.json")),
            "not in the layout",
        ),
    ]);
}

#[test]
fn p0_repo_05_schemas_directory_has_the_layout() {
    let root = schemas_dir();
    for dir in ["envelope", "envelope/history", "mcp", "index"] {
        assert!(root.join(dir).is_dir(), "schemas/{dir}/ is missing");
    }
    assert!(
        root.join("mcp/MAPPING.md").is_file(),
        "schemas/mcp/MAPPING.md is missing"
    );
    let ddl = fs::read_to_string(root.join("index/v2.sql")).unwrap();
    assert!(
        ddl.starts_with("-- "),
        "schemas/index/v2.sql starts with its header comment"
    );
}
