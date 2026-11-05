//! P3-MCP-03: `duscope mcp` through the binary: the flags it starts with,
//! `tools/list` against its golden, every tool's `structuredContent` against
//! `duscope <verb> --json` on the fixture tree, and the roots' refusals
//! through a tool.
//!
//! No shipped tool exists before P3-MCP-04 and P3-MCP-07, so the server is
//! started with `DUSCOPE_TEST_MCP_TOOLS`, which a debug build answers with
//! two fixture tools (`test_summary`, `test_top`) that go through the same
//! registry, roots and runner as every tool will.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::ffi::OsString;
use std::io::Write;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

use duscope_testkit::{TestEnv, golden};
use serde_json::{Value, json};

/// The tools a release build offers.
const SHIPPED: [&str; 0] = [];

/// The fixture tools a debug build adds under `DUSCOPE_TEST_MCP_TOOLS`.
const FIXTURE_TOOLS: [&str; 2] = ["test_summary", "test_top"];

/// What a server wrote.
struct Served {
    lines: Vec<String>,
    stderr: String,
    output: Output,
}

impl Served {
    /// The result of the response to request `id`.
    fn result(&self, id: u64) -> Value {
        let response = self.response(id);
        response
            .get("result")
            .cloned()
            .unwrap_or_else(|| panic!("request {id} got no result: {response}"))
    }

    fn response(&self, id: u64) -> Value {
        self.lines
            .iter()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .find(|message| message["id"] == id)
            .unwrap_or_else(|| panic!("no response to {id}: {:#?}", self.lines))
    }

    /// The raw line of the response to `id`.
    fn line(&self, id: u64) -> &str {
        self.lines
            .iter()
            .find(|line| serde_json::from_str::<Value>(line).unwrap()["id"] == id)
            .unwrap()
    }
}

/// `structuredContent` as the response's line holds it, byte for byte.
fn structured_content(line: &str) -> &str {
    let key = r#""structuredContent":"#;
    let start = line.find(key).unwrap_or_else(|| panic!("{line}")) + key.len();
    let mut depth = 0;
    let mut in_string = false;
    let mut escaped = false;
    for (i, c) in line[start..].char_indices() {
        match (in_string, escaped, c) {
            (true, true, _) => escaped = false,
            (true, false, '\\') => escaped = true,
            (true, false, '"') => in_string = false,
            (false, _, '"') => in_string = true,
            (false, _, '{' | '[') => depth += 1,
            (false, _, '}' | ']') => {
                depth -= 1;
                if depth == 0 {
                    return &line[start..=start + i];
                }
            }
            _ => {}
        }
    }
    panic!("structuredContent does not end: {line}")
}

/// Starts `duscope mcp ARGS` with the fixture tools, sends `requests`,
/// closes stdin, and collects what it wrote.
fn serve(env: &TestEnv, args: &[OsString], requests: &[Value]) -> Served {
    let mut child = env
        .cmd()
        .arg("mcp")
        .args(args)
        .env("DUSCOPE_TEST_MCP_TOOLS", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    for request in requests {
        writeln!(stdin, "{request}").unwrap();
    }
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    Served {
        lines: String::from_utf8(output.stdout.clone())
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        output,
    }
}

fn roots(paths: &[&Path]) -> Vec<OsString> {
    paths
        .iter()
        .flat_map(|path| [OsString::from("--roots"), path.as_os_str().to_owned()])
        .collect()
}

/// A 2026-07-28 request.
fn request(id: u64, method: &str, extra: &Value) -> Value {
    let mut params = json!({
        "_meta": {
            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
            "io.modelcontextprotocol/clientCapabilities": {},
        },
    });
    params
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

fn call(id: u64, tool: &str, arguments: &Value) -> Value {
    request(
        id,
        "tools/call",
        &json!({"name": tool, "arguments": arguments}),
    )
}

/// `duscope ARGS --json`: its stdout without the newline.
fn cli_json(env: &TestEnv, args: &[OsString]) -> String {
    let out = env.cmd().args(args).arg("--json").output().unwrap();
    String::from_utf8(out.stdout)
        .unwrap()
        .trim_end_matches('\n')
        .to_owned()
}

/// Saves a scan of `path`: its id.
fn save(env: &TestEnv, path: &Path) -> String {
    let out = env
        .cmd()
        .arg("scan")
        .arg(path)
        .args(["--save", "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let envelope: Value = serde_json::from_slice(&out.stdout).unwrap();
    envelope["scan"]["id"].as_str().unwrap().to_owned()
}

/// The fixture tree's `home`, canonical, scanned and saved: it and the id.
fn saved_home(env: &TestEnv) -> (PathBuf, String) {
    let home = env.fixtures().path("home").canonicalize().unwrap();
    let id = save(env, &home);
    (home, id)
}

/// A small tree of its own: `root/{a/file, link -> ../outside}` and
/// `outside/secret`, canonical.
fn small_tree(env: &TestEnv) -> PathBuf {
    let base = env.work_dir().canonicalize().unwrap();
    std::fs::create_dir_all(base.join("root/a")).unwrap();
    std::fs::write(base.join("root/a/file"), vec![0u8; 50_000]).unwrap();
    std::fs::write(base.join("root/top-file"), vec![0u8; 9_000]).unwrap();
    std::fs::create_dir_all(base.join("outside")).unwrap();
    std::fs::write(base.join("outside/secret"), b"x").unwrap();
    symlink(base.join("outside"), base.join("root/link")).unwrap();
    base
}

#[test]
fn p3_mcp_03_mcp_refuses_to_start_without_roots() {
    let env = TestEnv::new();
    let served = serve(&env, &[], &[]);
    assert_eq!(served.output.status.code(), Some(2), "{}", served.stderr);
    assert!(served.lines.is_empty(), "{:?}", served.lines);
    assert!(served.stderr.contains("--roots"), "{}", served.stderr);
}

#[test]
fn p3_mcp_03_a_root_that_is_not_a_directory_is_refused() {
    let env = TestEnv::new();
    let base = small_tree(&env);
    for root in [base.join("nowhere"), base.join("root/a/file")] {
        let served = serve(
            &env,
            &roots(&[&root]),
            &[request(1, "tools/list", &json!({}))],
        );
        assert_eq!(served.output.status.code(), Some(2), "{}", served.stderr);
        assert!(served.lines.is_empty(), "{:?}", served.lines);
        assert!(
            served.stderr.contains(&root.display().to_string()),
            "{}",
            served.stderr
        );
    }
}

#[test]
fn p3_mcp_03_tools_list_matches_its_golden() {
    let env = TestEnv::new();
    let base = small_tree(&env);
    let served = serve(
        &env,
        &roots(&[&base.join("root")]),
        &[request(1, "tools/list", &json!({}))],
    );
    let line = served.line(1);
    let normalized = line.replace(
        &format!(r#""version":"{}""#, env!("CARGO_PKG_VERSION")),
        r#""version":"<version>""#,
    );
    golden("mcp_tools_list.snap", &format!("{normalized}\n"));
    let listed = served.result(1);
    let names: Vec<&str> = listed["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    let mut expected: Vec<&str> = SHIPPED.iter().chain(&FIXTURE_TOOLS).copied().collect();
    expected.sort_unstable();
    assert_eq!(names, expected);
    // Every tool declares the size past which it spills: --max-chars.
    let capped = serve(
        &env,
        &[
            roots(&[&base.join("root")]),
            vec!["--max-chars".into(), "5000".into()],
        ]
        .concat(),
        &[request(1, "tools/list", &json!({}))],
    );
    for tool in capped.result(1)["tools"].as_array().unwrap() {
        assert_eq!(
            tool["_meta"]["anthropic/maxResultSizeChars"], 5000,
            "{tool}"
        );
    }
}

#[test]
fn p3_mcp_03_structured_content_is_the_verbs_json_for_every_tool() {
    let env = TestEnv::new();
    let (home, id) = saved_home(&env);
    let library = home.join("Library");
    let path = |p: &Path| p.display().to_string();
    // Each case: the tool, its arguments, and the command line that prints
    // the same envelope. Every tool tools/list lists needs one.
    let cases: Vec<(&str, Value, Vec<OsString>)> = vec![
        (
            "test_summary",
            json!({"path": path(&home)}),
            vec!["summary".into(), "--no-scan".into(), home.clone().into()],
        ),
        (
            "test_summary",
            json!({"scan_id": &id[..6]}),
            vec![
                "summary".into(),
                "--no-scan".into(),
                "--scan".into(),
                id.clone().into(),
                home.clone().into(),
            ],
        ),
        (
            "test_top",
            json!({"path": path(&library), "limit": 3}),
            vec![
                "top".into(),
                "--no-scan".into(),
                "--limit".into(),
                "3".into(),
                library.clone().into(),
            ],
        ),
    ];
    let requests: Vec<Value> = std::iter::once(request(0, "tools/list", &json!({})))
        .chain(
            cases
                .iter()
                .enumerate()
                .map(|(i, (tool, arguments, _))| call(i as u64 + 1, tool, arguments)),
        )
        .collect();
    let served = serve(&env, &roots(&[&home]), &requests);
    for tool in served.result(0)["tools"].as_array().unwrap() {
        let name = tool["name"].as_str().unwrap();
        assert!(
            cases.iter().any(|(case, _, _)| *case == name),
            "{name} has no case comparing it with its verb"
        );
    }
    for (i, (tool, arguments, args)) in cases.iter().enumerate() {
        let id = i as u64 + 1;
        let result = served.result(id);
        assert_eq!(result["isError"], false, "{tool} {arguments}: {result}");
        assert_eq!(
            structured_content(served.line(id)),
            cli_json(&env, args),
            "{tool} {arguments}"
        );
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(!text.contains('\n'), "{text}");
    }
    // The verbs' footers stay out of the server's log.
    assert!(!served.stderr.contains(" allocated"), "{}", served.stderr);
}

#[test]
fn p3_mcp_03_a_verbs_failure_is_an_error_result_with_its_envelope() {
    let env = TestEnv::new();
    let base = small_tree(&env);
    let root = base.join("root");
    // Nothing is saved: the verb fails scan_not_found.
    let served = serve(
        &env,
        &roots(&[&root]),
        &[call(1, "test_top", &json!({"path": root}))],
    );
    let result = served.result(1);
    assert_eq!(result["isError"], true, "{result}");
    assert_eq!(
        structured_content(served.line(1)),
        cli_json(
            &env,
            &["top".into(), "--no-scan".into(), root.clone().into()]
        )
    );
    let text = result["content"][0]["text"].as_str().unwrap();
    let error = &result["structuredContent"]["error"];
    assert_eq!(error["code"], "scan_not_found");
    assert!(
        text.starts_with(error["message"].as_str().unwrap()),
        "{text}"
    );
    assert!(text.ends_with(error["hint"].as_str().unwrap()), "{text}");
}

#[test]
fn p3_mcp_03_the_roots_refuse_through_a_tool() {
    let env = TestEnv::new();
    let base = small_tree(&env);
    let root = base.join("root");
    let outside = save(&env, &base.join("outside"));
    save(&env, &root);
    let cases = [
        (
            json!({"path": root.join("a/../../outside")}),
            "refused_path",
        ),
        (json!({"path": root.join("link/secret")}), "refused_path"),
        (json!({"path": base.join("outside")}), "refused_path"),
        (json!({"path": "a/file"}), "refused_path"),
        (
            json!({"scan_id": env.index_dir().join(format!("scans/{outside}.db"))}),
            "refused_path",
        ),
        (json!({"scan_id": &outside[..8]}), "scan_not_found"),
        (json!({"scan_id": "0000"}), "scan_not_found"),
        (json!({"path": root, "extra": 1}), "usage"),
    ];
    let requests: Vec<Value> = cases
        .iter()
        .enumerate()
        .map(|(i, (arguments, _))| call(i as u64 + 1, "test_summary", arguments))
        .collect();
    let served = serve(&env, &roots(&[&root]), &requests);
    for (i, (arguments, code)) in cases.iter().enumerate() {
        let result = served.result(i as u64 + 1);
        assert_eq!(result["isError"], true, "{arguments}: {result}");
        assert_eq!(
            result["structuredContent"]["error"]["code"], *code,
            "{arguments}: {result}"
        );
        assert_eq!(result["structuredContent"]["ok"], false);
    }
}

#[test]
fn p3_mcp_03_a_scan_above_a_root_is_read_at_the_root() {
    let env = TestEnv::new();
    let base = small_tree(&env);
    let id = save(&env, &base.join("root"));
    let a = base.join("root/a");
    let served = serve(
        &env,
        &roots(&[&a]),
        &[
            call(1, "test_summary", &json!({"scan_id": id})),
            call(2, "test_summary", &json!({"scan_id": id, "path": "a/file"})),
            call(
                3,
                "test_summary",
                &json!({"scan_id": id, "path": "top-file"}),
            ),
        ],
    );
    let result = served.result(1);
    assert_eq!(result["isError"], false, "{result}");
    assert_eq!(
        result["structuredContent"]["result"]["path"],
        a.display().to_string()
    );
    assert_eq!(
        structured_content(served.line(1)),
        cli_json(
            &env,
            &[
                "summary".into(),
                "--no-scan".into(),
                "--scan".into(),
                id.clone().into(),
                a.clone().into()
            ]
        )
    );
    // A relative path is read from the scan's root, and must still lie
    // under a root.
    assert_eq!(served.result(2)["isError"], false);
    assert_eq!(
        served.result(3)["structuredContent"]["error"]["code"],
        "refused_path"
    );
}

#[test]
fn p3_mcp_03_stdout_holds_protocol_messages_alone() {
    let env = TestEnv::new();
    let (home, _) = saved_home(&env);
    let served = serve(
        &env,
        &[roots(&[&home]), vec!["--log-level".into(), "debug".into()]].concat(),
        &[
            request(1, "server/discover", &json!({})),
            request(2, "tools/list", &json!({})),
            call(3, "test_top", &json!({"path": home})),
            call(4, "test_top", &json!({"path": "/"})),
            call(5, "nope", &json!({})),
        ],
    );
    assert!(served.output.status.success(), "{}", served.stderr);
    assert_eq!(served.lines.len(), 5);
    for line in &served.lines {
        let message: Value = serde_json::from_str(line).unwrap();
        assert_eq!(message["jsonrpc"], "2.0", "{line}");
        assert!(message.get("id").is_some(), "{line}");
    }
    assert_eq!(served.response(5)["error"]["code"], -32602);
    // The debug log went to stderr.
    assert!(served.stderr.contains("tools/call"), "{}", served.stderr);
}

#[test]
fn p3_mcp_03_a_2025_11_25_client_lists_and_calls_the_tools() {
    let env = TestEnv::new();
    let (home, _) = saved_home(&env);
    let served = serve(
        &env,
        &roots(&[&home]),
        &[
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
                "protocolVersion": "2025-11-25", "capabilities": {},
                "clientInfo": {"name": "t", "version": "1"}}}),
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
            json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
                "params": {"name": "test_summary", "arguments": {"path": home}}}),
        ],
    );
    assert_eq!(
        served.result(1)["capabilities"]["tools"],
        json!({"listChanged": false})
    );
    assert!(served.result(2)["tools"].is_array());
    let called = served.result(3);
    assert_eq!(called["isError"], false, "{called}");
    assert!(called.get("resultType").is_none());
}

#[test]
fn p3_mcp_03_an_envelope_past_max_chars_comes_back_spilled() {
    let env = TestEnv::new();
    let (home, _) = saved_home(&env);
    let served = serve(
        &env,
        &[roots(&[&home]), vec!["--max-chars".into(), "300".into()]].concat(),
        &[call(1, "test_top", &json!({"path": home, "limit": 50}))],
    );
    let result = served.result(1);
    assert_eq!(result["isError"], false, "{result}");
    let spilled = &result["structuredContent"]["spilled"];
    let file = spilled["path"].as_str().unwrap();
    assert!(
        Path::new(file).starts_with(env.index_dir().join("out")),
        "{file}"
    );
    let whole: Value = serde_json::from_slice(&std::fs::read(file).unwrap()).unwrap();
    assert_eq!(whole["returned_count"], spilled["rows"]);
}
