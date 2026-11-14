//! P3-MCP-04: the scan group's tools as the registry sees them: their
//! schemas are the files under `schemas/mcp/`, they register, and each
//! call's arguments become the verb's flags.

use std::path::{Path, PathBuf};

use duscope_testkit::TestEnv;
use serde_json::json;

use super::*;

/// `schemas/mcp/` in the repository.
fn schemas_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schemas/mcp")
}

#[test]
fn p3_mcp_04_each_tools_schema_is_its_file_under_schemas_mcp() {
    let tools = shipped();
    for tool in &tools {
        let file = schemas_dir().join(format!("{}.input.json", tool.name()));
        let text =
            std::fs::read_to_string(&file).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
        assert_eq!(
            tool.input_schema(),
            text,
            "crates/duscope-mcp/src/tools/schemas/{0}.input.json differs from \
             schemas/mcp/{0}.input.json: copy it",
            tool.name()
        );
    }
    // Every input schema there is a shipped tool's.
    for entry in std::fs::read_dir(schemas_dir()).unwrap() {
        let name = entry.unwrap().file_name().to_string_lossy().into_owned();
        if let Some(tool) = name.strip_suffix(".input.json") {
            assert!(
                tools.iter().any(|t| t.name() == tool),
                "schemas/mcp/{name} is no shipped tool's"
            );
        }
    }
}

#[test]
fn p3_mcp_04_the_shipped_tools_register() {
    let mut registry = Registry::new(24_000, false);
    for tool in shipped() {
        registry.register(tool).unwrap();
    }
    let names: Vec<&str> = registry.names().collect();
    assert_eq!(
        names,
        [
            "duscope_by_type",
            "duscope_caches",
            "duscope_diff",
            "duscope_explain",
            "duscope_query",
            "duscope_scan",
            "duscope_scans",
            "duscope_schema",
            "duscope_stale",
            "duscope_summary",
            "duscope_top",
        ]
    );
    for listed in registry.list() {
        let value = serde_json::to_value(&listed).unwrap();
        let read_only = value["name"] != "duscope_scan";
        assert_eq!(value["annotations"]["readOnlyHint"], read_only, "{value}");
        assert_eq!(value["annotations"]["idempotentHint"], read_only, "{value}");
        assert_eq!(value["annotations"]["destructiveHint"], false, "{value}");
    }
}

/// The command line `tool` builds for `arguments`, with `root` the one root.
pub(super) fn command_line(
    tool: &str,
    arguments: &Value,
    root: &Path,
    index_dir: &Path,
) -> Vec<String> {
    let roots = Roots::new([root]).unwrap();
    let context = Context {
        roots: &roots,
        index_dir,
    };
    let tool = shipped().into_iter().find(|t| t.name() == tool).unwrap();
    let invocation = tool
        .invocation(arguments.as_object().unwrap(), &context)
        .unwrap();
    std::iter::once(invocation.verb.to_owned())
        .chain(
            invocation
                .args
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned()),
        )
        .collect()
}

#[test]
fn p3_mcp_04_each_argument_becomes_its_flag() {
    let env = TestEnv::new();
    let root = env.work_dir().canonicalize().unwrap();
    std::fs::create_dir_all(root.join("sub")).unwrap();
    let index_dir = env.index_dir();
    let top = root.to_string_lossy().into_owned();
    let sub = root.join("sub").to_string_lossy().into_owned();
    let cases: Vec<(&str, Value, Vec<&str>)> = vec![
        ("duscope_scan", json!({}), vec!["scan", "--save", &top]),
        (
            "duscope_scan",
            json!({"root": sub, "attrs": "full", "clones": false, "one_file_system": true,
                "max_age": "2h"}),
            vec![
                "scan",
                "--save",
                "--attrs=full",
                "--no-clones",
                "--one-file-system",
                "--reuse=2h",
                &sub,
            ],
        ),
        (
            "duscope_scan",
            json!({"clones": true, "one_file_system": false}),
            vec!["scan", "--save", "--clones", &top],
        ),
        ("duscope_scans", json!({}), vec!["scans", &top]),
        (
            "duscope_summary",
            json!({"path": sub}),
            vec!["summary", "--no-scan", &sub],
        ),
        (
            "duscope_top",
            json!({"files": true, "depth": 2, "min_size": "-1", "glob": ["*.mp4", "-x"],
                "exclude": ["a"], "size": "apparent", "fields": ["path", "name"], "limit": 5,
                "cursor": "--all"}),
            vec![
                "top",
                "--no-scan",
                "--files",
                "--depth=2",
                "--min-size=-1",
                "--glob=*.mp4",
                "--glob=-x",
                "--exclude=a",
                "--size=apparent",
                "--fields=path,name",
                "--limit=5",
                "--cursor=--all",
                &top,
            ],
        ),
        (
            "duscope_top",
            json!({"files": false, "fields": [], "glob": []}),
            vec!["top", "--no-scan", &top],
        ),
        (
            "duscope_by_type",
            json!({"examples": 0, "limit": 1}),
            vec!["by-type", "--no-scan", "--examples=0", "--limit=1", &top],
        ),
        (
            "duscope_stale",
            json!({"older_than": "1y", "min_size": "1G", "atime": true}),
            vec![
                "stale",
                "--no-scan",
                "--older-than=1y",
                "--min-size=1G",
                "--atime",
                "--attrs=full",
                &top,
            ],
        ),
        (
            "duscope_stale",
            json!({"atime": false}),
            vec!["stale", "--no-scan", &top],
        ),
    ];
    for (tool, arguments, expected) in cases {
        assert_eq!(
            command_line(tool, &arguments, &root, &index_dir),
            expected,
            "{tool} {arguments}"
        );
    }
}

/// P3-MCP-10: the list tools, whose verbs take `--strict`, take `strict`;
/// `false` sets nothing, as an absent `strict` does.
#[test]
fn p3_mcp_10_strict_becomes_strict_in_each_list_tool() {
    let env = TestEnv::new();
    let root = env.work_dir().canonicalize().unwrap();
    let index_dir = env.index_dir();
    for tool in [
        "duscope_top",
        "duscope_stale",
        "duscope_caches",
        "duscope_by_type",
    ] {
        let strict = command_line(tool, &json!({"strict": true}), &root, &index_dir);
        assert_eq!(
            strict.iter().filter(|arg| *arg == "--strict").count(),
            1,
            "{tool}: {strict:?}"
        );
        let lenient = command_line(tool, &json!({"strict": false}), &root, &index_dir);
        assert!(
            !lenient.iter().any(|arg| arg == "--strict"),
            "{tool}: {lenient:?}"
        );
        assert_eq!(
            lenient,
            command_line(tool, &json!({}), &root, &index_dir),
            "{tool}"
        );
    }
}

#[test]
fn p3_mcp_04_a_spilled_result_says_where_the_rows_went() {
    let text = spilled_text(&json!({"path": "/tmp/out/x.json", "rows": 400, "bytes": 90_000}));
    assert!(
        text.contains("400 rows are in /tmp/out/x.json (90000 bytes)"),
        "{text}"
    );
    assert!(text.contains("--max-chars"), "{text}");
}
