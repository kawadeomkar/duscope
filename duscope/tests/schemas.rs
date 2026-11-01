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
    if errors.is_empty()
        && let Err(e) = jsonschema::draft202012::new(&schema)
    {
        problem(format!("does not compile as a validator: {e}"));
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

/// The verb whose envelope the golden file `name` holds:
/// `top_json_home@macos.snap` is `top`'s, `by_type_json_verbs@linux.snap`
/// `by-type`'s; `None` for a golden of another format.
fn golden_verb(name: &str) -> Option<String> {
    let (verb, _) = name.split_once("_json_")?;
    Some(verb.replace('_', "-"))
}

/// A golden envelope as JSON, with what the golden masked put back as
/// values of their types: an unquoted placeholder (`"uid":<id>`) as 0, a
/// masked `volumes` array as empty.
fn unmask(text: &str) -> Value {
    let mut text = text.replace("\"volumes\":[\"<volumes>\"]", "\"volumes\":[]");
    while let Some(at) = text.find(":<") {
        let end = text[at..].find('>').map_or(text.len(), |n| at + n + 1);
        text.replace_range(at + 1..end, "0");
    }
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}: {text}"))
}

/// Every golden envelope: its file name, its verb, and its JSON.
fn golden_envelopes() -> Vec<(String, String, Value)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden");
    let mut goldens: Vec<(String, String, Value)> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter_map(|name| {
            let verb = golden_verb(&name)?;
            let text = fs::read_to_string(dir.join(&name)).unwrap();
            Some((name, verb, unmask(&text)))
        })
        .collect();
    goldens.sort_by(|a, b| a.0.cmp(&b.0));
    goldens
}

/// Where `instance` breaks `schema`, one line each; empty when it passes.
fn violations(schema: &Value, instance: &Value) -> Vec<String> {
    let validator = jsonschema::draft202012::new(schema).unwrap();
    validator
        .iter_errors(instance)
        .map(|e| format!("{}: {e}", e.instance_path()))
        .collect()
}

#[test]
fn p2_cli_01_every_golden_envelope_validates_against_its_verb_schema() {
    let goldens = golden_envelopes();
    assert!(goldens.len() >= 10, "{} golden envelopes", goldens.len());
    let mut problems = Vec::new();
    for (name, verb, envelope) in &goldens {
        let file = schemas_dir().join(format!("envelope/{verb}.schema.json"));
        let Ok(text) = fs::read_to_string(&file) else {
            problems.push(format!(
                "{name}: no schema for {verb} at {}",
                file.display()
            ));
            continue;
        };
        let schema: Value = serde_json::from_str(&text).unwrap();
        problems.extend(
            violations(&schema, envelope)
                .into_iter()
                .map(|v| format!("{name} against {verb}.schema.json: {v}")),
        );
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// The major of a `schema_version` (`0` of `0.2`).
fn major(version: &str) -> &str {
    version.split('.').next().unwrap_or(version)
}

#[test]
fn p2_cli_01_golden_envelopes_validate_against_every_published_minor() {
    let history = schemas_dir().join("envelope/history");
    let current = duscope_core::envelope::SCHEMA_VERSION;
    let mut minors: Vec<String> = fs::read_dir(&history)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|minor| major(minor) == major(current))
        .collect();
    minors.sort();
    assert!(
        minors.iter().any(|m| m == current),
        "history/ keeps no copy of {current}: {minors:?}"
    );
    let mut problems = Vec::new();
    for minor in &minors {
        for (name, verb, envelope) in golden_envelopes() {
            let file = history.join(minor).join(format!("{verb}.schema.json"));
            // A verb a minor did not have yet has nothing to stay compatible with.
            let Ok(text) = fs::read_to_string(&file) else {
                continue;
            };
            let schema: Value = serde_json::from_str(&text).unwrap();
            problems.extend(
                violations(&schema, &envelope)
                    .into_iter()
                    .map(|v| format!("{name} against history/{minor}/{verb}.schema.json: {v}")),
            );
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// The exit code of each `Exit` variant, as `verbs/mod.rs` numbers them.
fn exit_code(variant: &str) -> Option<u8> {
    match variant {
        "Success" => Some(0),
        "Runtime" => Some(1),
        "Usage" => Some(2),
        "NotFound" => Some(3),
        "Permission" => Some(4),
        "Index" => Some(5),
        "Interrupted" => Some(130),
        _ => None,
    }
}

/// Every `.rs` file under `dir`.
fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn p2_cli_01_every_code_the_source_writes_is_in_the_tables() {
    use duscope_core::envelope::{ERROR_CODES, WARNING_CODES};
    use duscope_core::query::Regex;
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    sources(&root.join("src"), &mut files);
    sources(&root.join("../crates/duscope-core/src"), &mut files);
    let failure = Regex::new(r#"Failure::new\(\s*Exit::(\w+),\s*"(\w+)""#).unwrap();
    let warning = Regex::new(r#"Warning \{\s*code: "(\w+)""#).unwrap();
    let mut problems = Vec::new();
    let (mut failures, mut warnings) = (0, 0);
    for file in &files {
        let text = fs::read(file).unwrap();
        for found in failure.captures_iter(&text) {
            let variant = String::from_utf8_lossy(&found[1]).into_owned();
            let code = String::from_utf8_lossy(&found[2]).into_owned();
            failures += 1;
            match ERROR_CODES.iter().find(|(c, _)| *c == code) {
                Some(&(_, exit)) if exit_code(&variant) == Some(exit) => {}
                Some(&(_, exit)) => problems.push(format!(
                    "{}: {code} exits {variant}, the table says {exit}",
                    file.display()
                )),
                None => problems.push(format!("{}: {code} is not an error code", file.display())),
            }
        }
        for found in warning.captures_iter(&text) {
            let code = String::from_utf8_lossy(&found[1]).into_owned();
            warnings += 1;
            if !WARNING_CODES.contains(&code.as_str()) {
                problems.push(format!("{}: {code} is not a warning code", file.display()));
            }
        }
    }
    assert!(
        failures > 20 && warnings > 5,
        "{failures} failures, {warnings} warnings"
    );
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}
