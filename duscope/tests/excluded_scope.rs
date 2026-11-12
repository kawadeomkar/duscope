//! P5-CLI-02: `scan.excluded` within the path read (decision E). Every verb
//! names only the excluded directories inside the subtree its result was
//! read at; a list names the first 20 of them, in the scan's order, and the
//! warning `excluded_truncated` counts the rest; `summary` and `explain` name
//! them all. Over MCP, where a scan rooted above a root is read at the root,
//! a result names none outside it.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use duscope_testkit::{TestEnv, golden, replace_values};
use serde_json::{Value, json};

/// A valid `CACHEDIR.TAG`.
const TAG: &[u8] = b"Signature: 8a477f597d28d172789f06886806bc55\n";

/// `duscope ARGS`: its stdout as JSON, whatever the exit code.
fn run(env: &TestEnv, args: &[&OsStr]) -> Value {
    let out = env.cmd().args(args).output().unwrap();
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "{args:?}: {e}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

fn os(text: &str) -> &OsStr {
    OsStr::new(text)
}

/// `root/in/c00 … c24` and `root/out/d0 … d4`, each a cache that its
/// `CACHEDIR.TAG` marks, beside a file of its own; canonical.
fn tree(env: &TestEnv) -> PathBuf {
    let root = fs::canonicalize(env.tmp_dir()).unwrap().join("tree");
    let dirs = (0..25)
        .map(|i| format!("in/c{i:02}"))
        .chain((0..5).map(|i| format!("out/d{i}")));
    for dir in dirs {
        let dir = root.join(dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("CACHEDIR.TAG"), TAG).unwrap();
        fs::write(dir.join("blob"), vec![0u8; 5_000]).unwrap();
    }
    fs::write(root.join("in/kept"), vec![0u8; 9_000]).unwrap();
    fs::write(root.join("out/kept"), vec![0u8; 9_000]).unwrap();
    root
}

/// The paths `envelope`'s `scan.excluded` names.
fn excluded(envelope: &Value) -> Vec<String> {
    envelope["scan"]["excluded"]
        .as_array()
        .unwrap_or_else(|| panic!("{envelope}"))
        .iter()
        .map(|e| e["path"].as_str().unwrap().to_owned())
        .collect()
}

/// The paths of `names` under `dir`.
fn under(dir: &Path, names: impl IntoIterator<Item = String>) -> Vec<String> {
    names
        .into_iter()
        .map(|n| dir.join(n).display().to_string())
        .collect()
}

/// `excluded_truncated` among `warnings`: its count, and its hint.
fn truncated(warnings: &Value) -> Option<(u64, String)> {
    warnings
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["code"] == "excluded_truncated")
        .map(|w| {
            (
                w["count"].as_u64().unwrap(),
                w["hint"].as_str().unwrap().to_owned(),
            )
        })
}

#[test]
// One saved scan, read by each kind of verb in turn.
#[allow(clippy::too_many_lines)]
fn p5_cli_02_a_list_names_the_first_20_excluded_directories_under_its_path() {
    let env = TestEnv::new();
    let root = tree(&env);
    let inside = root.join("in");
    let outside = root.join("out");
    let first_20 = under(&inside, (0..20).map(|i| format!("c{i:02}")));
    let all_in = under(&inside, (0..25).map(|i| format!("c{i:02}")));
    let all_out = under(&outside, (0..5).map(|i| format!("d{i}")));

    // A scan run now: the first 20 of 30, in the scan's order, and the
    // warning for the other 10, whose hint names summary of the saved scan.
    let scanned = run(
        &env,
        &[
            os("scan"),
            root.as_os_str(),
            os("--exclude-caches"),
            os("--save"),
            os("--json"),
        ],
    );
    let id = scanned["scan"]["id"].as_str().unwrap().to_owned();
    assert_eq!(scanned["scan"]["excluded_count"], 30);
    assert_eq!(excluded(&scanned), first_20);
    let (count, hint) = truncated(&scanned["warnings"]).unwrap();
    assert_eq!(count, 10);
    assert!(
        hint.contains("duscope summary") && hint.contains(&id),
        "{hint}"
    );

    // The saved scan read at its root lists the same 20.
    let list = |verb: &[&str], path: &Path| {
        let mut args: Vec<&OsStr> = verb.iter().map(|a| os(a)).collect();
        args.extend([
            os("--no-scan"),
            os("--exclude-caches"),
            os("--json"),
            path.as_os_str(),
        ]);
        run(&env, &args)
    };
    let top = list(&["top"], &root);
    assert_eq!(top["scan"]["id"], id.as_str());
    assert_eq!(excluded(&top), first_20);
    assert_eq!(truncated(&top["warnings"]).unwrap().0, 10);
    assert_eq!(top["scan"]["excluded_count"], 30, "the whole scan's");
    // At a subdirectory: only what is under it, and the cap again.
    let top_in = list(&["top"], &inside);
    assert_eq!(excluded(&top_in), first_20);
    assert_eq!(truncated(&top_in["warnings"]).unwrap().0, 5);
    let top_out = list(&["top"], &outside);
    assert_eq!(excluded(&top_out), all_out);
    assert_eq!(truncated(&top_out["warnings"]), None);
    // Each list verb (caches refuses --exclude-caches, whose directories
    // it lists: below, by the scan's id).
    for verb in [
        &["tree"][..],
        &["stale", "--older-than", "1d"],
        &["by-type"],
    ] {
        let envelope = list(verb, &outside);
        assert_eq!(excluded(&envelope), all_out, "{verb:?}");
        let envelope = list(verb, &root);
        assert_eq!(excluded(&envelope), first_20, "{verb:?}");
        assert_eq!(truncated(&envelope["warnings"]).unwrap().0, 10, "{verb:?}");
    }
    let caches = run(
        &env,
        &[
            os("caches"),
            os("--scan"),
            os(&id),
            os("--json"),
            outside.as_os_str(),
        ],
    );
    assert_eq!(excluded(&caches), all_out, "{caches}");
    // ndjson: the header carries the block, the trailer the warning.
    let out = env
        .cmd()
        .args(["top", "--no-scan", "--exclude-caches", "--format", "ndjson"])
        .arg(&inside)
        .output()
        .unwrap();
    let lines: Vec<Value> = out
        .stdout
        .split(|&b| b == b'\n')
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_slice(l).unwrap())
        .collect();
    assert_eq!(excluded(&lines[0]), first_20);
    assert_eq!(truncated(&lines.last().unwrap()["warnings"]).unwrap().0, 5);

    // summary and explain: every one under the path, uncapped.
    let summary = list(&["summary"], &root);
    let mut every = all_in.clone();
    every.extend(all_out.clone());
    assert_eq!(excluded(&summary), every);
    assert_eq!(truncated(&summary["warnings"]), None);
    assert_eq!(excluded(&list(&["summary"], &outside)), all_out);
    let explain = run(&env, &[os("explain"), inside.as_os_str(), os("--json")]);
    assert_eq!(explain["scan"]["id"], id.as_str());
    assert_eq!(excluded(&explain), all_in);
    assert_eq!(truncated(&explain["warnings"]), None);

    // explain --errors and query read the whole scan: a list, capped.
    let errors = run(
        &env,
        &[os("explain"), os("--errors"), os(&id), os("--json")],
    );
    assert_eq!(excluded(&errors), first_20);
    assert_eq!(truncated(&errors["warnings"]).unwrap().0, 10);
    let sql = run(
        &env,
        &[
            os("query"),
            os("--no-scan"),
            os("--exclude-caches"),
            os("--sql"),
            os("SELECT 1"),
            os("--json"),
            outside.as_os_str(),
        ],
    );
    assert_eq!(excluded(&sql), all_out);

    // diff of two saved scans under a path.
    let again = run(
        &env,
        &[
            os("scan"),
            root.as_os_str(),
            os("--exclude-caches"),
            os("--save"),
            os("--json"),
        ],
    );
    let again_id = again["scan"]["id"].as_str().unwrap().to_owned();
    let diff = run(
        &env,
        &[
            os("diff"),
            os(&id),
            os(&again_id),
            os("--under"),
            outside.as_os_str(),
            os("--json"),
        ],
    );
    assert_eq!(excluded(&diff), all_out);
    let diff = run(&env, &[os("diff"), os(&id), os(&again_id), os("--json")]);
    assert_eq!(excluded(&diff), first_20);
    assert_eq!(truncated(&diff["warnings"]).unwrap().0, 10);

    // clean: the excluded directories within its items.
    let item = outside.join("d0");
    let clean = run(
        &env,
        &[
            os("clean"),
            os("--scan"),
            os(&id),
            item.as_os_str(),
            os("--json"),
        ],
    );
    assert_eq!(excluded(&clean), [item.display().to_string()], "{clean}");
}

#[test]
fn p5_cli_02_a_scan_and_its_saved_index_name_the_same_first_20() {
    let env = TestEnv::new();
    // Twenty under `a`, and `a-b` beside it: bytewise, `a-b` sorts before
    // `a/x00` (`-` before `/`), but the scan reaches it after `a`'s
    // subtree, and the first 20 are the scan's.
    let root = fs::canonicalize(env.tmp_dir()).unwrap().join("tree");
    let dirs = (0..20)
        .map(|i| format!("a/x{i:02}"))
        .chain(["a-b".to_owned()]);
    for dir in dirs {
        let dir = root.join(dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("CACHEDIR.TAG"), TAG).unwrap();
    }
    let scanned = run(
        &env,
        &[
            os("scan"),
            root.as_os_str(),
            os("--exclude-caches"),
            os("--save"),
            os("--json"),
        ],
    );
    let first_20 = under(&root.join("a"), (0..20).map(|i| format!("x{i:02}")));
    assert_eq!(excluded(&scanned), first_20);
    let top = run(
        &env,
        &[
            os("top"),
            os("--no-scan"),
            os("--exclude-caches"),
            os("--json"),
            root.as_os_str(),
        ],
    );
    assert_eq!(top["scan"]["id"], scanned["scan"]["id"]);
    assert_eq!(excluded(&top), first_20);
    assert_eq!(truncated(&top["warnings"]).unwrap().0, 1);
}

#[test]
fn p5_cli_02_summary_of_the_scan_root_is_unchanged() {
    let env = TestEnv::new();
    let root = fs::canonicalize(env.fixtures().path("home/Projects")).unwrap();
    let out = env
        .cmd()
        .args([
            "summary",
            "--json",
            "--threads",
            "4",
            "--no-save",
            "--exclude-caches",
        ])
        .arg(&root)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    let envelope: Value = serde_json::from_str(&stdout).unwrap();
    // More than a list names: summary names them all.
    assert!(envelope["scan"]["excluded"].as_array().unwrap().len() > 20);
    let mut text = duscope_testkit::normalize(&stdout, &root);
    replace_values(&mut text, "\"duration_ms\":", ',', "<ms>");
    golden(
        &format!("summary_json_excludes@{}.snap", std::env::consts::OS),
        &text,
    );
}

/// What a server wrote to stdout, a message a line.
fn serve(env: &TestEnv, root: &Path, requests: &[Value]) -> Vec<Value> {
    let mut child = env
        .cmd()
        .arg("mcp")
        .arg("--roots")
        .arg(root)
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
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn call(id: u64, tool: &str, arguments: &Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": {
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientCapabilities": {},
            },
            "name": tool,
            "arguments": arguments,
        },
    })
}

/// Every string in `value`.
fn strings<'v>(value: &'v Value, out: &mut Vec<&'v str>) {
    match value {
        Value::String(s) => out.push(s),
        Value::Array(items) => items.iter().for_each(|v| strings(v, out)),
        Value::Object(map) => {
            for (key, v) in map {
                out.push(key);
                strings(v, out);
            }
        }
        _ => {}
    }
}

#[test]
fn p5_cli_02_a_tool_names_no_excluded_directory_outside_the_roots() {
    let env = TestEnv::new();
    let root = tree(&env);
    let inside = root.join("in");
    let outside = root.join("out");
    let scanned = run(
        &env,
        &[
            os("scan"),
            root.as_os_str(),
            os("--exclude-caches"),
            os("--save"),
            os("--json"),
        ],
    );
    let id = scanned["scan"]["id"].as_str().unwrap().to_owned();
    // The scan holds excluded directories outside the root to be served.
    let whole = run(
        &env,
        &[
            os("summary"),
            os("--scan"),
            os(&id),
            os("--json"),
            root.as_os_str(),
        ],
    );
    assert!(
        excluded(&whole)
            .iter()
            .any(|p| Path::new(p).starts_with(&outside)),
        "{whole}"
    );
    let replies = serve(
        &env,
        &inside,
        &[
            call(1, "duscope_summary", &json!({"scan_id": id})),
            call(2, "duscope_explain", &json!({"scan_id": id})),
            call(3, "duscope_top", &json!({"scan_id": id})),
            call(
                4,
                "duscope_caches",
                &json!({"scan_id": id, "min_size": "0"}),
            ),
        ],
    );
    let outside_text = outside.display().to_string();
    for id in 1..=4 {
        let reply = replies
            .iter()
            .find(|r| r["id"] == id)
            .unwrap_or_else(|| panic!("no reply {id}: {replies:?}"));
        let result = &reply["result"];
        assert_eq!(result["isError"], false, "{reply}");
        let content = &result["structuredContent"];
        // The excluded directories inside the root, and no others.
        let listed = excluded(content);
        assert!(!listed.is_empty(), "{reply}");
        assert!(
            listed.iter().all(|p| Path::new(p).starts_with(&inside)),
            "{listed:?}"
        );
        // No string of the result names a path outside the root; the
        // scan's root, above it, is the one path that is not under it.
        let mut texts = Vec::new();
        strings(result, &mut texts);
        let leaked: Vec<&&str> = texts.iter().filter(|t| t.contains(&outside_text)).collect();
        assert!(leaked.is_empty(), "{leaked:?}");
    }
}
