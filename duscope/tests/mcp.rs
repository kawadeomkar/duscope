//! P3-MCP-03: `duscope mcp` through the binary: the flags it starts with,
//! `tools/list` against its golden, every tool's `structuredContent` against
//! `duscope <verb> --json` on the fixture tree, and the roots' refusals
//! through a tool.
//!
//! The server is started with `DUSCOPE_TEST_MCP_TOOLS`, which a debug build
//! answers with two fixture tools (`test_summary`, `test_top`) beside the
//! shipped ones; they go through the same registry, roots and runner, and
//! test the machinery whatever the shipped tools become.
//!
//! P3-MCP-04: the scan group's tools against their verbs, argument by
//! argument, and the two verb flags they rest on: `scan --reuse` and
//! `scans PATH...`.
//!
//! P3-MCP-07: the analysis group's tools against their verbs; the scan
//! `duscope_query` and `duscope_schema` read, which must lie inside the
//! roots; hostile statements refused through the tool as through the verb;
//! and `duscope_diff` compared under the roots.
//!
//! P3-MCP-05: the calls one at a time on the worker: two scans of one root
//! in turn, a scan's progress (`DUSCOPE_TEST_MCP_PROGRESS_MS` shortens its
//! two seconds), the deadline (`DUSCOPE_TEST_MCP_DEADLINE_MS`), a cancelled
//! call, and the scans a client that died had queued.
//!
//! P3-MCP-08: a verb that panics (`DUSCOPE_TEST_MCP_TOOLS=panic` adds the
//! fixture tool `test_panic`), under `DUSCOPE_TEST_PANIC=abort`, which ends
//! the process after the panic hook as the release build's abort does, and
//! without it, where the server survives.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::symlink;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

use duscope_testkit::{TestEnv, golden};
use serde_json::{Value, json};

/// The tools a release build offers.
const SHIPPED: [&str; 11] = [
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
];

/// P3-MCP-04's tools.
const SCAN_GROUP: [&str; 6] = [
    "duscope_scan",
    "duscope_scans",
    "duscope_summary",
    "duscope_top",
    "duscope_by_type",
    "duscope_stale",
];

/// P3-MCP-07's tools.
const ANALYSIS_GROUP: [&str; 5] = [
    "duscope_caches",
    "duscope_explain",
    "duscope_query",
    "duscope_schema",
    "duscope_diff",
];

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
    ]
    .into_iter()
    .chain(scan_group_cases(&env, &home, &id))
    .chain(analysis_group_cases(&env, &home, &id))
    .collect();
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

// P3-MCP-04: the scan group.

/// A tool, its arguments, and the command line that prints the same
/// envelope.
type Case = (&'static str, Value, Vec<OsString>);

/// The command line `words`, split at spaces, then `path`.
fn line(words: &str, path: &Path) -> Vec<OsString> {
    words
        .split_whitespace()
        .map(OsString::from)
        .chain([path.as_os_str().to_owned()])
        .collect()
}

/// The ids of the scans saved under the test's index directory, sorted.
fn saved_ids(env: &TestEnv) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(env.index_dir().join("scans")) else {
        return Vec::new();
    };
    let mut ids: Vec<String> = entries
        .filter_map(|entry| {
            let name = entry.unwrap().file_name();
            name.to_str()?.strip_suffix(".db").map(str::to_owned)
        })
        .collect();
    ids.sort();
    ids
}

/// Saves a scan of `path` taken with `flags`: its id.
fn save_with(env: &TestEnv, path: &Path, flags: &[&str]) -> String {
    let out = env
        .cmd()
        .arg("scan")
        .args(flags)
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

/// `ids`, sorted.
fn sorted<S: AsRef<str>>(ids: &[S]) -> Vec<String> {
    let mut ids: Vec<String> = ids.iter().map(|id| id.as_ref().to_owned()).collect();
    ids.sort();
    ids
}

/// The ids of a listing's rows, sorted.
fn listed_ids(envelope: &Value) -> Vec<String> {
    let ids: Vec<&str> = envelope["entries"]
        .as_array()
        .unwrap_or_else(|| panic!("{envelope}"))
        .iter()
        .map(|entry| entry["id"].as_str().unwrap())
        .collect();
    sorted(&ids)
}

/// The scan group on the fixture tree's `home`, saved as `id`, the server's
/// one root: each tool without arguments and with every argument it takes,
/// beside the command line that prints the same envelope.
fn scan_group_cases(env: &TestEnv, home: &Path, id: &str) -> Vec<Case> {
    let library = home.join("Library");
    let first_page = cli_json(env, &line("top --no-scan --limit=2", home));
    let first_page: Value = serde_json::from_str(&first_page).unwrap();
    let cursor = first_page["next_cursor"]
        .as_str()
        .unwrap_or_else(|| panic!("{first_page}"));
    vec![
        // A scan younger than max_age is returned, so the envelope is the
        // saved one either way.
        (
            "duscope_scan",
            json!({"max_age": "1h"}),
            line("scan --save --reuse=1h", home),
        ),
        (
            "duscope_scan",
            json!({"root": home, "attrs": "basic", "clones": true, "one_file_system": false,
                "max_age": "1h"}),
            line("scan --save --attrs=basic --clones --reuse=1h", home),
        ),
        ("duscope_scans", json!({}), line("scans", home)),
        (
            "duscope_summary",
            json!({}),
            line("summary --no-scan", home),
        ),
        (
            "duscope_summary",
            json!({"scan_id": &id[..6], "path": "Library"}),
            line(&format!("summary --no-scan --scan {id}"), &library),
        ),
        ("duscope_top", json!({}), line("top --no-scan", home)),
        (
            "duscope_top",
            json!({"scan_id": id, "path": "Library", "files": true, "depth": 3,
                "min_size": "1K", "glob": ["*"], "exclude": ["*.log", "*.tmp"],
                "size": "apparent", "fields": ["path", "apparent_bytes"], "limit": 3}),
            line(
                &format!(
                    "top --no-scan --scan {id} --files --depth=3 --min-size=1K --glob=* \
                     --exclude=*.log --exclude=*.tmp --size=apparent \
                     --fields=path,apparent_bytes --limit=3"
                ),
                &library,
            ),
        ),
        (
            "duscope_top",
            json!({"limit": 2, "cursor": cursor}),
            line(&format!("top --no-scan --limit=2 --cursor={cursor}"), home),
        ),
        (
            "duscope_by_type",
            json!({}),
            line("by-type --no-scan", home),
        ),
        (
            "duscope_by_type",
            json!({"scan_id": id, "path": "Library", "examples": 1, "glob": ["*"],
                "exclude": ["*.log"], "size": "apparent", "fields": ["class", "allocated_bytes"],
                "limit": 2}),
            line(
                &format!(
                    "by-type --no-scan --scan {id} --examples=1 --glob=* --exclude=*.log \
                     --size=apparent --fields=class,allocated_bytes --limit=2"
                ),
                &library,
            ),
        ),
        ("duscope_stale", json!({}), line("stale --no-scan", home)),
        (
            "duscope_stale",
            json!({"scan_id": id, "path": "Library", "older_than": "1d", "min_size": "1",
                "glob": ["*"], "exclude": ["*.log"], "size": "apparent",
                "fields": ["path", "mtime"], "limit": 2}),
            line(
                &format!(
                    "stale --no-scan --scan {id} --older-than=1d --min-size=1 --glob=* \
                     --exclude=*.log --size=apparent --fields=path,mtime --limit=2"
                ),
                &library,
            ),
        ),
    ]
}

#[test]
fn p3_mcp_04_each_tool_answers_with_its_verbs_json() {
    let env = TestEnv::new();
    let (home, id) = saved_home(&env);
    let cases = scan_group_cases(&env, &home, &id);
    for tool in SCAN_GROUP {
        assert!(
            cases.iter().any(|(case, _, _)| *case == tool),
            "{tool} has no case comparing it with its verb"
        );
    }
    let requests: Vec<Value> = cases
        .iter()
        .enumerate()
        .map(|(i, (tool, arguments, _))| call(i as u64 + 1, tool, arguments))
        .collect();
    let served = serve(&env, &roots(&[&home]), &requests);
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
        assert!(
            !text.contains("null") && !text.contains('?'),
            "{tool}: {text}"
        );
    }
    // max_age found the saved scan: nothing new was saved.
    assert_eq!(saved_ids(&env), vec![id]);
}

#[test]
fn p3_mcp_04_duscope_scan_saves_a_scan_unless_max_age_finds_one() {
    let env = TestEnv::new();
    let base = small_tree(&env);
    let root = base.join("root");
    let other = json!({"attrs": "full", "clones": false, "one_file_system": true});
    let mut reused_other = other.clone();
    reused_other["max_age"] = json!("1h");
    let served = serve(
        &env,
        &roots(&[&root]),
        &[
            call(1, "duscope_scan", &json!({})),
            call(2, "duscope_scan", &other),
            call(3, "duscope_scan", &json!({"max_age": "1h"})),
            call(4, "duscope_scan", &reused_other),
            call(
                5,
                "duscope_scan",
                &json!({"root": root.join("a"), "max_age": "1h"}),
            ),
        ],
    );
    let scan = |id: u64| {
        let result = served.result(id);
        assert_eq!(result["isError"], false, "{result}");
        result["structuredContent"]["scan"].clone()
    };
    let (first, second) = (scan(1), scan(2));
    assert_eq!(first["root"], root.display().to_string());
    assert_eq!(second["attrs"], "full");
    assert_eq!(second["device_confined"], true);
    // max_age returns the scan taken with the same options, of that root.
    assert_eq!(scan(3)["id"], first["id"]);
    assert_eq!(scan(4)["id"], second["id"]);
    let third = scan(5);
    assert_eq!(third["root"], root.join("a").display().to_string());
    let ids: Vec<&str> = [&first, &second, &third]
        .iter()
        .map(|scan| scan["id"].as_str().unwrap())
        .collect();
    assert_eq!(saved_ids(&env), sorted(&ids));
    let text = served.result(1)["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(text.contains(ids[0]), "{text}");
}

#[test]
fn p3_mcp_04_duscope_scan_scans_only_inside_the_roots() {
    let env = TestEnv::new();
    let base = small_tree(&env);
    let root = base.join("root");
    let cases = [
        (json!({"root": base}), "refused_path"),
        (json!({"root": "a"}), "refused_path"),
        (json!({"root": root.join("link")}), "refused_path"),
        (
            json!({"root": root.join("a/../../outside")}),
            "refused_path",
        ),
        (json!({"attrs": "all"}), "usage"),
        (json!({"max_age": 60}), "usage"),
    ];
    let requests: Vec<Value> = cases
        .iter()
        .enumerate()
        .map(|(i, (arguments, _))| call(i as u64 + 1, "duscope_scan", arguments))
        .collect();
    let served = serve(&env, &roots(&[&root]), &requests);
    for (i, (arguments, code)) in cases.iter().enumerate() {
        let result = served.result(i as u64 + 1);
        assert_eq!(result["isError"], true, "{arguments}: {result}");
        assert_eq!(
            result["structuredContent"]["error"]["code"], *code,
            "{arguments}: {result}"
        );
    }
    // With several roots, the call names one.
    let served = serve(
        &env,
        &roots(&[&root, &base.join("outside")]),
        &[call(1, "duscope_scan", &json!({}))],
    );
    let result = served.result(1);
    assert_eq!(result["structuredContent"]["error"]["code"], "usage");
    assert!(
        result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("root"),
        "{result}"
    );
    assert!(saved_ids(&env).is_empty());
}

#[test]
fn p3_mcp_04_duscope_scans_lists_the_scans_under_the_roots_alone() {
    let env = TestEnv::new();
    let base = small_tree(&env);
    let root = base.join("root");
    let (at, under) = (save(&env, &root), save(&env, &root.join("a")));
    let (above, outside) = (save(&env, &base), save(&env, &base.join("outside")));
    let served = serve(
        &env,
        &roots(&[&root]),
        &[call(1, "duscope_scans", &json!({}))],
    );
    let result = served.result(1);
    assert_eq!(result["isError"], false, "{result}");
    assert_eq!(
        listed_ids(&result["structuredContent"]),
        sorted(&[&at, &under])
    );
    // Each root's scans, with several; a scan above a root is not listed.
    let served = serve(
        &env,
        &roots(&[&root.join("a"), &base.join("outside")]),
        &[call(1, "duscope_scans", &json!({}))],
    );
    assert_eq!(
        listed_ids(&served.result(1)["structuredContent"]),
        sorted(&[&under, &outside])
    );
    assert!(!listed_ids(&served.result(1)["structuredContent"]).contains(&above));
}

#[test]
fn p3_mcp_04_the_read_tools_never_scan_or_save() {
    let env = TestEnv::new();
    let base = small_tree(&env);
    let root = base.join("root");
    let tools = [
        "duscope_summary",
        "duscope_top",
        "duscope_by_type",
        "duscope_stale",
    ];
    let requests: Vec<Value> = tools
        .iter()
        .enumerate()
        .map(|(i, tool)| call(i as u64 + 1, tool, &json!({})))
        .chain([call(9, "duscope_scans", &json!({}))])
        .collect();
    let served = serve(&env, &roots(&[&root]), &requests);
    for (i, tool) in tools.iter().enumerate() {
        let result = served.result(i as u64 + 1);
        assert_eq!(result["isError"], true, "{tool}: {result}");
        assert_eq!(
            result["structuredContent"]["error"]["code"], "scan_not_found",
            "{tool}: {result}"
        );
    }
    let listing = served.result(9);
    assert_eq!(listing["structuredContent"]["total_count"], 0, "{listing}");
    assert!(saved_ids(&env).is_empty());
}

#[test]
fn p3_mcp_04_stale_with_atime_reads_a_scan_with_access_times() {
    let env = TestEnv::new();
    let base = small_tree(&env);
    let root = base.join("root");
    let full = save_with(&env, &root, &["--attrs", "full"]);
    save(&env, &root);
    let served = serve(
        &env,
        &roots(&[&root]),
        &[call(
            1,
            "duscope_stale",
            &json!({"atime": true, "older_than": "1h", "min_size": "1"}),
        )],
    );
    let result = served.result(1);
    assert_eq!(result["isError"], false, "{result}");
    assert_eq!(result["structuredContent"]["scan"]["id"], full.as_str());
    assert_eq!(
        structured_content(served.line(1)),
        cli_json(
            &env,
            &line(
                "stale --no-scan --older-than=1h --min-size=1 --atime --attrs=full",
                &root
            )
        )
    );
}

#[test]
fn p3_mcp_04_a_value_starting_with_a_dash_is_never_read_as_a_flag() {
    let env = TestEnv::new();
    let (home, _) = saved_home(&env);
    let served = serve(
        &env,
        &roots(&[&home]),
        &[
            call(
                1,
                "duscope_top",
                &json!({"glob": ["-x"], "exclude": ["--files"]}),
            ),
            call(2, "duscope_top", &json!({"cursor": "--files"})),
            call(3, "duscope_stale", &json!({"older_than": "-1d"})),
        ],
    );
    // A glob that matches no path.
    let globbed = served.result(1);
    assert_eq!(globbed["isError"], false, "{globbed}");
    assert_eq!(globbed["structuredContent"]["returned_count"], 0);
    // The verb reads each value, and refuses it as what it is.
    assert_eq!(
        served.result(2)["structuredContent"]["error"]["code"],
        "invalid_cursor"
    );
    let age = &served.result(3)["structuredContent"]["error"];
    assert_eq!(age["code"], "usage");
    assert!(
        age["message"].as_str().unwrap().contains("--older-than"),
        "{age}"
    );
}

#[test]
fn p3_mcp_04_scan_reuse_prints_a_saved_scan_of_the_path_younger_than_age() {
    let env = TestEnv::new();
    let base = small_tree(&env);
    let root = base.join("root");
    let id = save(&env, &root);
    let scan = |flags: &[&str], path: &Path| -> Value {
        let out = env
            .cmd()
            .arg("scan")
            .args(flags)
            .arg(path)
            .arg("--json")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{flags:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    };
    let reused = scan(&["--reuse", "1h", "--save"], &root);
    assert_eq!(reused["scan"]["id"], id.as_str(), "{reused}");
    assert_eq!(reused["scan"]["root"], root.display().to_string());
    assert!(
        reused["entries"]
            .as_array()
            .is_some_and(|rows| !rows.is_empty())
    );
    assert_eq!(saved_ids(&env), vec![id.clone()]);
    // Other options, another path, or a scan older than AGE: the tree is
    // scanned, and the scan, unsaved, has no id.
    let fresh: [(&[&str], PathBuf); 4] = [
        (&["--reuse", "1h", "--one-file-system"], root.clone()),
        (&["--reuse", "1h", "--exclude", "*.tmp"], root.clone()),
        (&["--reuse", "1h"], root.join("a")),
        (
            &["--reuse", "1h", "--test-now", "2099-01-01T00:00:00Z"],
            root.clone(),
        ),
    ];
    for (flags, path) in &fresh {
        let scanned = scan(flags, path);
        assert!(scanned["scan"]["id"].is_null(), "{flags:?}: {scanned}");
    }
    let out = env
        .cmd()
        .args(["scan", "--reuse", "soon", "--json"])
        .arg(&root)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let bad_age: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(bad_age["error"]["code"], "usage");
    assert_eq!(saved_ids(&env), vec![id]);
}

#[test]
fn p3_mcp_04_scan_reuse_draws_no_treemap_and_opens_no_viewer() {
    let env = TestEnv::new();
    let base = small_tree(&env);
    let root = base.join("root");
    save(&env, &root);
    for flags in [["--treemap", "always"], ["--interactive", "--plain"]] {
        let out = env
            .cmd()
            .args(["scan", "--reuse", "1h"])
            .args(flags)
            .arg(&root)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{flags:?}: {stderr}");
        assert!(stderr.contains("--reuse"), "{flags:?}: {stderr}");
        assert!(out.stdout.is_empty(), "{flags:?}");
    }
}

#[test]
fn p3_mcp_04_scans_lists_the_scans_of_the_paths_and_under_them() {
    let env = TestEnv::new();
    let base = small_tree(&env);
    let root = base.join("root");
    let at = save(&env, &root);
    let under = save(&env, &root.join("a"));
    let outside = save(&env, &base.join("outside"));
    let above = save(&env, &base);
    let listed = |paths: &[&str]| -> Vec<String> {
        let out = env
            .cmd()
            .current_dir(&base)
            .arg("scans")
            .args(paths)
            .arg("--json")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{paths:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        listed_ids(&serde_json::from_slice(&out.stdout).unwrap())
    };
    assert_eq!(listed(&["root"]), sorted(&[&at, &under]));
    assert_eq!(listed(&["root/a", "outside"]), sorted(&[&under, &outside]));
    // A link is followed to the directory it names.
    assert_eq!(listed(&["root/link"]), vec![outside.clone()]);
    assert_eq!(listed(&[]), sorted(&[&at, &under, &outside, &above]));
    for flags in [&["--prune", "root"][..], &["--check", &at, "root"]] {
        let out = env.cmd().arg("scans").args(flags).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{flags:?}");
    }
}

// P3-MCP-07: the analysis group.

/// `duscope ARGS`, each argument as given.
fn args(parts: &[&dyn AsRef<std::ffi::OsStr>]) -> Vec<OsString> {
    parts.iter().map(|part| part.as_ref().to_owned()).collect()
}

/// `--under=PATH`.
fn under(path: &Path) -> OsString {
    let mut flag = OsString::from("--under=");
    flag.push(path);
    flag
}

/// A statement that counts the files under the path's entry.
const COUNT: &str = "SELECT count(*) AS n FROM files WHERE id BETWEEN :root_id AND :root_last";

/// The analysis group on the fixture tree's `home`, saved as `id`, the
/// server's one root, as [`scan_group_cases`] has the scan group.
fn analysis_group_cases(env: &TestEnv, home: &Path, id: &str) -> Vec<Case> {
    let library = home.join("Library");
    let page = |args: &[OsString]| -> Value { serde_json::from_str(&cli_json(env, args)).unwrap() };
    let first = page(&line("caches --no-scan --min-size=0 --limit=2", home));
    let cursor = first["next_cursor"]
        .as_str()
        .unwrap_or_else(|| panic!("{first}"))
        .to_owned();
    let largest = page(&line("top --no-scan --files --limit=1", home));
    let file = PathBuf::from(largest["entries"][0]["path"].as_str().unwrap());
    let sql = format!("--sql={COUNT}");
    vec![
        ("duscope_caches", json!({}), line("caches --no-scan", home)),
        (
            "duscope_caches",
            json!({"scan_id": id, "path": "Library", "by_project": true, "loose": true,
                "min_size": "0", "size": "apparent", "fields": ["project_root", "allocated_bytes"],
                "limit": 3}),
            line(
                &format!(
                    "caches --no-scan --scan {id} --by-project --loose --min-size=0 \
                     --size=apparent --fields=project_root,allocated_bytes --limit=3"
                ),
                &library,
            ),
        ),
        (
            "duscope_caches",
            json!({"min_size": "0", "limit": 2, "cursor": cursor}),
            line(
                &format!("caches --no-scan --min-size=0 --limit=2 --cursor={cursor}"),
                home,
            ),
        ),
        (
            "duscope_explain",
            json!({"path": file}),
            line("explain", &file),
        ),
        (
            "duscope_explain",
            json!({"scan_id": &id[..6], "path": "Library"}),
            line(&format!("explain --scan {id}"), &library),
        ),
        (
            "duscope_query",
            json!({"sql": COUNT}),
            args(&[&"query", &"--no-scan", &"--scan", &id, &sql, &home]),
        ),
        (
            "duscope_query",
            json!({"sql": COUNT, "scan_id": "latest", "path": library}),
            args(&[&"query", &"--no-scan", &"--scan", &id, &sql, &library]),
        ),
        (
            "duscope_schema",
            json!({}),
            line(&format!("schema --scan {id}"), home),
        ),
        (
            "duscope_schema",
            json!({"scan_id": &id[..6], "path": "Library"}),
            line(&format!("schema --scan {id}"), &library),
        ),
        (
            "duscope_schema",
            json!({"envelope": "top"}),
            args(&[&"schema", &"--envelope=top"]),
        ),
        (
            "duscope_diff",
            json!({"old": id, "new": "latest"}),
            args(&[&"diff", &id, &"latest", &under(home)]),
        ),
        (
            "duscope_diff",
            json!({"old": &id[..6], "new": id, "path": "Library", "min_delta": "0", "depth": 2,
                "size": "apparent", "limit": 5}),
            args(&[
                &"diff",
                &id,
                &id,
                &under(&library),
                &"--min-delta=0",
                &"--depth=2",
                &"--size=apparent",
                &"--limit=5",
            ]),
        ),
    ]
}

#[test]
fn p3_mcp_07_each_tool_answers_with_its_verbs_json() {
    let env = TestEnv::new();
    let (home, id) = saved_home(&env);
    let cases = analysis_group_cases(&env, &home, &id);
    for tool in ANALYSIS_GROUP {
        assert!(
            cases.iter().any(|(case, _, _)| *case == tool),
            "{tool} has no case comparing it with its verb"
        );
    }
    let requests: Vec<Value> = cases
        .iter()
        .enumerate()
        .map(|(i, (tool, arguments, _))| call(i as u64 + 1, tool, arguments))
        .collect();
    let served = serve(&env, &roots(&[&home]), &requests);
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
        assert!(
            !text.contains("null") && !text.contains('?'),
            "{tool}: {text}"
        );
    }
    assert_eq!(saved_ids(&env), vec![id]);
}

#[test]
fn p3_mcp_07_query_and_schema_read_only_a_scan_inside_the_roots() {
    let env = TestEnv::new();
    let base = small_tree(&env);
    let root = base.join("root");
    let above = save(&env, &base);
    let inside = save(&env, &root);
    let calls = [
        call(1, "duscope_query", &json!({"sql": "SELECT 1 AS one"})),
        call(
            2,
            "duscope_query",
            &json!({"sql": "SELECT 1 AS one", "scan_id": "latest"}),
        ),
        call(3, "duscope_schema", &json!({})),
        call(
            4,
            "duscope_query",
            &json!({"sql": "SELECT 1 AS one", "scan_id": &above[..8]}),
        ),
        call(5, "duscope_schema", &json!({"scan_id": above})),
        call(
            6,
            "duscope_query",
            &json!({"sql": "SELECT 1 AS one", "path": "/"}),
        ),
    ];
    let served = serve(&env, &roots(&[&root]), &calls);
    // The scan of the root, though the scan above it is as new.
    for id in 1..=3 {
        let result = served.result(id);
        assert_eq!(result["isError"], false, "{result}");
        assert_eq!(result["structuredContent"]["scan"]["id"], inside.as_str());
    }
    // A scan above the root is not found, as one the roots hide.
    for id in 4..=5 {
        let error = &served.result(id)["structuredContent"]["error"];
        assert_eq!(error["code"], "scan_not_found", "{error}");
    }
    assert_eq!(
        served.result(6)["structuredContent"]["error"]["code"],
        "refused_path"
    );
    // With only a scan above the root, there is nothing to read.
    let served = serve(
        &env,
        &roots(&[&root.join("a")]),
        &[call(1, "duscope_query", &json!({"sql": "SELECT 1 AS one"}))],
    );
    let error = &served.result(1)["structuredContent"]["error"];
    assert_eq!(error["code"], "scan_not_found", "{error}");
}

#[test]
fn p3_mcp_07_a_hostile_statement_is_refused_through_the_tool_as_through_the_verb() {
    let env = TestEnv::new();
    let (home, id) = saved_home(&env);
    let file = env.index_dir().join(format!("scans/{id}.db"));
    let before = std::fs::read(&file).unwrap();
    let statements = [
        "DELETE FROM files",
        "DROP TABLE files",
        "CREATE TEMP TABLE t (x)",
        "ATTACH '/tmp/duscope-attach.db' AS other",
        "PRAGMA writable_schema = 1",
        "SELECT * FROM sqlite_master",
        "SELECT load_extension('/tmp/x')",
        "SELECT readfile('/etc/passwd')",
        "SELECT 1; DROP TABLE files",
        "VACUUM INTO '/tmp/duscope-vacuum.db'",
        "SELEC 1",
        "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c) SELECT count(*) FROM c",
    ];
    let requests: Vec<Value> = statements
        .iter()
        .enumerate()
        .map(|(i, sql)| call(i as u64 + 1, "duscope_query", &json!({"sql": sql})))
        .collect();
    let served = serve(&env, &roots(&[&home]), &requests);
    for (i, sql) in statements.iter().enumerate() {
        let result = served.result(i as u64 + 1);
        assert_eq!(result["isError"], true, "{sql}: {result}");
        let flag = format!("--sql={sql}");
        assert_eq!(
            structured_content(served.line(i as u64 + 1)),
            cli_json(
                &env,
                &args(&[&"query", &"--no-scan", &"--scan", &id, &flag, &home])
            ),
            "{sql}"
        );
    }
    assert_eq!(
        served.result(statements.len() as u64)["structuredContent"]["error"]["code"],
        "sql_timeout"
    );
    assert_eq!(std::fs::read(&file).unwrap(), before);
}

#[test]
fn p3_mcp_07_diff_compares_under_a_path_inside_the_roots() {
    let env = TestEnv::new();
    let base = small_tree(&env);
    let root = base.join("root");
    let old = save(&env, &base);
    std::fs::write(base.join("outside/secret"), vec![0u8; 2_000_000]).unwrap();
    std::fs::write(root.join("a/grown"), vec![0u8; 3_000_000]).unwrap();
    let new = save(&env, &base);
    let served = serve(
        &env,
        &roots(&[&root]),
        &[
            call(1, "duscope_diff", &json!({"old": old, "new": new})),
            call(
                2,
                "duscope_diff",
                &json!({"old": old, "new": new, "path": base}),
            ),
            call(3, "duscope_diff", &json!({"old": "0000", "new": new})),
            call(
                4,
                "duscope_diff",
                &json!({"old": env.index_dir().join(format!("scans/{old}.db")), "new": new}),
            ),
        ],
    );
    // The scans of the parent are compared at the root: the file grown
    // outside it is not seen.
    let result = served.result(1);
    assert_eq!(result["isError"], false, "{result}");
    let envelope = &result["structuredContent"];
    assert_eq!(envelope["query"]["path"], root.display().to_string());
    let paths: Vec<&str> = envelope["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["path"].as_str().unwrap())
        .collect();
    assert!(
        paths.iter().all(|path| Path::new(path).starts_with(&root)),
        "{paths:?}"
    );
    assert!(
        paths.contains(&root.join("a/grown").to_str().unwrap()),
        "{paths:?}"
    );
    assert_eq!(
        structured_content(served.line(1)),
        cli_json(&env, &args(&[&"diff", &old, &new, &under(&root)]))
    );
    for (id, code) in [
        (2, "refused_path"),
        (3, "scan_not_found"),
        (4, "refused_path"),
    ] {
        let error = &served.result(id)["structuredContent"]["error"];
        assert_eq!(error["code"], code, "{id}: {error}");
    }
}

#[test]
fn p3_mcp_07_explain_reads_the_path_as_it_is_without_a_saved_scan() {
    let env = TestEnv::new();
    let base = small_tree(&env);
    let root = base.join("root");
    let file = root.join("a/file");
    let served = serve(
        &env,
        &roots(&[&root]),
        &[call(1, "duscope_explain", &json!({"path": file}))],
    );
    let result = served.result(1);
    assert_eq!(result["isError"], false, "{result}");
    assert_eq!(
        result["structuredContent"]["result"]["path"],
        file.display().to_string()
    );
    assert_eq!(
        structured_content(served.line(1)),
        cli_json(&env, &line("explain", &file))
    );
    assert!(saved_ids(&env).is_empty());
}

// P3-MCP-05.

/// `serve` with the environment variables `vars` set for the server.
fn serve_with_env(
    env: &TestEnv,
    vars: &[(&str, &str)],
    args: &[OsString],
    requests: &[Value],
) -> Served {
    let mut command = env.cmd();
    command
        .arg("mcp")
        .args(args)
        .env("DUSCOPE_TEST_MCP_TOOLS", "1")
        .envs(vars.iter().copied())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
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

/// A tree of 40 directories of 50 files each under the work directory,
/// canonical.
fn wide_tree(env: &TestEnv) -> PathBuf {
    let root = env.work_dir().canonicalize().unwrap().join("wide");
    for dir in 0..40 {
        let dir = root.join(format!("d{dir}"));
        std::fs::create_dir_all(&dir).unwrap();
        for file in 0..50 {
            std::fs::write(dir.join(format!("f{file}")), b"x").unwrap();
        }
    }
    root
}

#[test]
fn p3_mcp_05_two_scans_of_one_root_serialise_and_the_second_reuses_the_first() {
    let env = TestEnv::new();
    let root = wide_tree(&env);
    let reuse = json!({"root": root, "max_age": "1h"});
    let served = serve(
        &env,
        &roots(&[&root]),
        &[
            call(1, "duscope_scan", &reuse),
            call(2, "duscope_scan", &reuse),
        ],
    );
    let first = served.result(1);
    let second = served.result(2);
    assert_eq!(first["isError"], false, "{first}");
    assert_eq!(second["isError"], false, "{second}");
    // The second ran once the first had saved its scan, and returned it.
    let id = first["structuredContent"]["scan"]["id"].as_str().unwrap();
    assert_eq!(second["structuredContent"]["scan"]["id"], id);
    assert_eq!(saved_ids(&env), [id]);
    let at = |id: u64| served.lines.iter().position(|line| line == served.line(id));
    assert!(at(1) < at(2), "{:#?}", served.lines);
}

#[test]
fn p3_mcp_05_a_scan_sends_its_progress_before_its_result() {
    let env = TestEnv::new();
    let root = wide_tree(&env);
    let mut scan = call(1, "duscope_scan", &json!({"root": root}));
    scan["params"]["_meta"]["progressToken"] = json!("scan-1");
    let served = serve_with_env(
        &env,
        &[("DUSCOPE_TEST_MCP_PROGRESS_MS", "1")],
        &roots(&[&root]),
        &[scan, call(2, "duscope_scans", &json!({}))],
    );
    let result = served.result(1);
    assert_eq!(result["isError"], false, "{result}");
    let counted = &result["structuredContent"]["scan"];
    let entries = counted["file_count"].as_u64().unwrap() + counted["dir_count"].as_u64().unwrap();
    assert_eq!(entries, 2_041);
    let messages: Vec<Value> = served
        .lines
        .iter()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let answered = messages.iter().position(|m| m["id"] == 1).unwrap();
    let mut last = 0;
    for (at, message) in messages.iter().enumerate() {
        if message["method"] != "notifications/progress" {
            continue;
        }
        assert!(at < answered, "progress after the result: {message}");
        let params = &message["params"];
        assert_eq!(params["progressToken"], "scan-1", "{message}");
        let progress = params["progress"].as_u64().unwrap();
        assert!(progress > last && progress <= entries, "{message}");
        last = progress;
    }
    assert!(last > 0, "no progress: {:#?}", served.lines);
    // The call that named no token got none, and progress never reaches
    // stderr.
    assert_eq!(
        messages
            .iter()
            .filter(|m| m["method"] == "notifications/progress")
            .filter(|m| m["params"]["progressToken"] != "scan-1")
            .count(),
        0
    );
    assert!(
        !served.stderr.contains("entries counted"),
        "{}",
        served.stderr
    );
}

#[test]
fn p3_mcp_05_a_call_past_its_deadline_is_deadline_exceeded() {
    let env = TestEnv::new();
    let root = wide_tree(&env);
    let served = serve_with_env(
        &env,
        &[("DUSCOPE_TEST_MCP_DEADLINE_MS", "0")],
        &roots(&[&root]),
        &[
            call(1, "duscope_scan", &json!({"root": root})),
            request(2, "tools/list", &json!({})),
        ],
    );
    let result = served.result(1);
    assert_eq!(result["isError"], true, "{result}");
    let envelope = &result["structuredContent"];
    assert_eq!(envelope["ok"], false);
    assert_eq!(
        envelope["error"],
        json!({
            "code": "deadline_exceeded",
            "message": "duscope_scan ran past its deadline of 0 ms",
            "hint": "Call duscope_scan again on a smaller directory, or run duscope scan in a \
                     terminal, where no deadline applies",
            "retryable": true,
            "fixable_by": "user",
        })
    );
    assert_eq!(
        result["content"][0]["text"],
        "duscope_scan ran past its deadline of 0 ms. Call duscope_scan again on a smaller \
         directory, or run duscope scan in a terminal, where no deadline applies"
    );
    assert!(served.result(2)["tools"].is_array());
    assert_eq!(served.output.status.code(), Some(0), "{}", served.stderr);
    // The deadline stopped the scan's walk, so it saved nothing; the server
    // exits only once the verb has returned.
    assert!(saved_ids(&env).is_empty(), "{}", served.stderr);
}

#[test]
fn p3_mcp_05_a_cancelled_call_gets_no_response_and_the_next_one_runs() {
    let env = TestEnv::new();
    let root = wide_tree(&env);
    let served = serve(
        &env,
        &roots(&[&root]),
        &[
            call(1, "duscope_scan", &json!({"root": root})),
            call(2, "duscope_scan", &json!({"root": root.join("d1")})),
            json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": 2}}),
            call(3, "duscope_scans", &json!({})),
            request(4, "tools/list", &json!({})),
        ],
    );
    let ids: Vec<Value> = served
        .lines
        .iter()
        .map(|line| serde_json::from_str::<Value>(line).unwrap()["id"].clone())
        .collect();
    let mut sorted_ids = ids.clone();
    sorted_ids.sort_by_key(Value::as_u64);
    assert_eq!(sorted_ids, [1, 3, 4], "{:#?}", served.lines);
    // The cancelled scan never ran: one scan saved, the one scans lists.
    let first = served.result(1)["structuredContent"]["scan"]["id"].clone();
    assert_eq!(saved_ids(&env), [first.as_str().unwrap()]);
    let listed = &served.result(3)["structuredContent"]["entries"];
    assert_eq!(listed.as_array().unwrap().len(), 1, "{listed}");
}

#[test]
fn p3_mcp_05_the_scans_a_dead_client_queued_never_run() {
    let env = TestEnv::new();
    let root = wide_tree(&env);
    let mut child = env
        .cmd()
        .arg("mcp")
        .args(roots(&[&root]))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // The client died with three scans sent: nothing reads the server's
    // output, and its input ends.
    drop(child.stdout.take());
    let mut stdin = child.stdin.take().unwrap();
    for (id, dir) in [(1, "d1"), (2, "d2"), (3, "d3")] {
        let scan = call(id, "duscope_scan", &json!({"root": root.join(dir)}));
        writeln!(stdin, "{scan}").unwrap();
    }
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    // The first scan ran and its answer could not be sent; the two queued
    // behind it never started.
    assert_eq!(saved_ids(&env).len(), 1, "{stderr}");
}

// P3-MCP-08.

/// How the panic hook ends a debug build under `DUSCOPE_TEST_PANIC=abort`:
/// the status a shell gives a process `SIGABRT` ended, without the signal.
const TEST_ABORT_STATUS: i32 = 134;

/// What a server ended by `DUSCOPE_TEST_PANIC=abort` wrote: one line a
/// message, the last line whole.
fn messages(stdout: &[u8]) -> Vec<Value> {
    let text = std::str::from_utf8(stdout).unwrap();
    assert!(text.ends_with('\n'), "a partial line: {text:?}");
    text.lines()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|e| panic!("{e}: {line}")))
        .collect()
}

/// The acceptance check, short of the abort itself. A debug build unwinds,
/// so `DUSCOPE_TEST_PANIC=abort` stands in for the release build's
/// `panic = "abort"`: the hook runs as it would there, then ends the process
/// with status 134 rather than `SIGABRT`, since on macOS an abort writes a
/// crash report into the account's own `~/Library/Logs/DiagnosticReports`.
/// What this proves: every request owed is answered, in order, with whole
/// lines, before the process ends. What it does not: that the release build
/// runs the hook before it aborts, which Rust's `panic = "abort"` does by
/// design (the hook, then the abort) and no test here runs.
#[test]
fn p3_mcp_08_a_panicking_verb_answers_every_call_before_the_server_ends() {
    let env = TestEnv::new();
    let base = small_tree(&env);
    let root = base.join("root");
    let after = base.join("panic-now");
    let mut child = env
        .cmd()
        .arg("mcp")
        .args(roots(&[&root]))
        .env("DUSCOPE_TEST_MCP_TOOLS", "panic")
        .env("DUSCOPE_TEST_PANIC", "abort")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let logged = std::thread::spawn(move || {
        let mut text = String::new();
        stderr.read_to_string(&mut text).unwrap();
        text
    });
    let mut stdin = child.stdin.take().unwrap();
    for request in [
        call(1, "test_panic", &json!({"after": after})),
        call(2, "duscope_scans", &json!({})),
        call(3, "test_top", &json!({"path": root})),
        request(4, "tools/list", &json!({})),
    ] {
        writeln!(stdin, "{request}").unwrap();
    }
    // The reader answers tools/list at once, and had queued the two calls
    // before it: the running verb may panic now.
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut first = String::new();
    stdout.read_line(&mut first).unwrap();
    std::fs::write(&after, b"").unwrap();
    let mut rest = Vec::new();
    stdout.read_to_end(&mut rest).unwrap();
    let status = child.wait().unwrap();
    drop(stdin);
    let stderr = logged.join().unwrap();
    assert_eq!(
        status.code(),
        Some(TEST_ABORT_STATUS),
        "{status:?}: {stderr}"
    );
    assert_eq!(status.signal(), None);
    let written = [first.into_bytes(), rest].concat();
    let messages = messages(&written);
    let ids: Vec<&Value> = messages.iter().map(|m| &m["id"]).collect();
    assert_eq!(ids, [4, 1, 2, 3], "{messages:#?}");
    assert!(messages[0]["result"]["tools"].is_array(), "{}", messages[0]);
    assert_eq!(
        messages[1]["error"],
        json!({
            "code": -32603,
            "message": "internal error: test_panic's verb panicked; the server is stopping",
        })
    );
    for (message, tool) in [(&messages[2], "duscope_scans"), (&messages[3], "test_top")] {
        assert_eq!(message["error"]["code"], -32603, "{message}");
        assert_eq!(
            message["error"]["message"],
            format!("internal error: the server is stopping after a panic: {tool} did not run")
        );
    }
    // The panic's own message still reaches stderr, after the answers.
    assert!(
        stderr.contains("test_panic's verb panicked, as its test asked"),
        "{stderr}"
    );
}

#[test]
fn p3_mcp_08_where_panics_unwind_the_server_answers_and_goes_on() {
    let env = TestEnv::new();
    let base = small_tree(&env);
    let root = base.join("root");
    let after = base.join("panic-now");
    std::fs::write(&after, b"").unwrap();
    let served = serve_with_env(
        &env,
        &[("DUSCOPE_TEST_MCP_TOOLS", "panic")],
        &roots(&[&root]),
        &[
            call(1, "test_panic", &json!({"after": after})),
            call(2, "duscope_scans", &json!({})),
        ],
    );
    assert_eq!(served.output.status.code(), Some(0), "{}", served.stderr);
    assert_eq!(
        served.response(1)["error"],
        json!({"code": -32603, "message": "internal error: test_panic's verb panicked"})
    );
    assert_eq!(served.result(2)["isError"], false);
    assert!(
        served
            .stderr
            .contains("test_panic's verb panicked, as its test asked"),
        "{}",
        served.stderr
    );
}
