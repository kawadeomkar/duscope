//! P3-MCP-07: the analysis group's arguments as their verbs' flags; the
//! saved scan `duscope_query` and `duscope_schema` read is the fixture
//! tests' (`duscope/tests/mcp.rs`), which save one.

use duscope_testkit::TestEnv;
use serde_json::json;

use super::scan_group_tests::command_line;
use super::*;

#[test]
fn p3_mcp_07_each_argument_becomes_its_flag() {
    let env = TestEnv::new();
    let root = env.work_dir().canonicalize().unwrap();
    std::fs::create_dir_all(root.join("sub")).unwrap();
    let index_dir = env.index_dir();
    let top = root.to_string_lossy().into_owned();
    let sub = root.join("sub").to_string_lossy().into_owned();
    let under_top = format!("--under={top}");
    let under_sub = format!("--under={sub}");
    let cases: Vec<(&str, Value, Vec<&str>)> = vec![
        (
            "duscope_caches",
            json!({}),
            vec!["caches", "--no-scan", &top],
        ),
        (
            "duscope_caches",
            json!({"by_project": true, "loose": true, "min_size": "1G", "size": "apparent",
                "fields": ["project_root"], "limit": 4, "cursor": "-c"}),
            vec![
                "caches",
                "--no-scan",
                "--by-project",
                "--loose",
                "--min-size=1G",
                "--size=apparent",
                "--fields=project_root",
                "--limit=4",
                "--cursor=-c",
                &top,
            ],
        ),
        (
            "duscope_explain",
            json!({"path": sub}),
            vec!["explain", &sub],
        ),
        (
            "duscope_schema",
            json!({"envelope": "top", "path": sub}),
            vec!["schema", "--envelope=top"],
        ),
        (
            "duscope_diff",
            json!({"old": "latest", "new": "latest"}),
            vec!["diff", "latest", "latest", &under_top],
        ),
        (
            "duscope_diff",
            json!({"old": "latest", "new": "latest", "path": sub, "min_delta": "5M", "depth": 1,
                "size": "apparent", "limit": 9}),
            vec![
                "diff",
                "latest",
                "latest",
                &under_sub,
                "--min-delta=5M",
                "--depth=1",
                "--size=apparent",
                "--limit=9",
            ],
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

#[test]
fn p3_mcp_07_a_whole_scan_tool_with_no_saved_scan_refuses() {
    let env = TestEnv::new();
    let root = env.work_dir().canonicalize().unwrap();
    let roots = Roots::new([&root]).unwrap();
    let index_dir = env.index_dir();
    let context = Context {
        roots: &roots,
        index_dir: &index_dir,
    };
    for tool in shipped()
        .into_iter()
        .filter(|tool| matches!(tool.name(), "duscope_query" | "duscope_schema"))
    {
        let arguments = json!({"sql": "SELECT 1"});
        let refusal = tool
            .invocation(arguments.as_object().unwrap(), &context)
            .unwrap_err();
        assert_eq!(refusal.code, "scan_not_found", "{}: {refusal}", tool.name());
        // An index file is never a scan_id.
        let file = json!({"sql": "SELECT 1", "scan_id": index_dir.join("x.db")});
        let refusal = tool
            .invocation(file.as_object().unwrap(), &context)
            .unwrap_err();
        assert_eq!(refusal.code, "refused_path", "{}: {refusal}", tool.name());
    }
}
