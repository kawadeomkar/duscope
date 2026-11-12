//! P3-PKG-01: `server.json` and `docs/mcp.md` against what they describe. The
//! manifest validates against the MCP Registry's schema, copied into
//! `tests/mcp_registry/` because a test never reaches the network; each
//! registration recipe is the line its client's smoke script runs; the tool
//! list is what the server's `tools/list` answers.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use duscope_testkit::{TestEnv, duscope_bin};
use serde_json::Value;

const SERVER_NAME: &str = "io.github.kawadeomkar/duscope";

/// Each client's heading in `docs/mcp.md`, its smoke script and its command.
const CLIENTS: [(&str, &str, &str); 3] = [
    ("Claude Code", "claude-code", "claude"),
    ("Codex CLI", "codex", "codex"),
    ("Gemini CLI", "gemini", "gemini"),
];

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn read(path: impl AsRef<Path>) -> String {
    fs::read_to_string(repo().join(path)).unwrap()
}

fn manifest() -> Value {
    serde_json::from_str(&read("server.json")).unwrap()
}

/// The text of the section of `doc` under the heading `heading`, up to the
/// next heading of that level or above.
fn section<'a>(doc: &'a str, heading: &str) -> &'a str {
    let level = heading.bytes().take_while(|b| *b == b'#').count();
    let start = doc
        .find(&format!("\n{heading}\n"))
        .unwrap_or_else(|| panic!("no heading {heading:?}"))
        + heading.len()
        + 2;
    let body = &doc[start..];
    let end = body
        .match_indices("\n#")
        .find(|(at, _)| {
            let hashes = body[at + 1..].bytes().take_while(|b| *b == b'#').count();
            hashes <= level
        })
        .map_or(body.len(), |(at, _)| at);
    &body[..end]
}

/// The first fenced block of `text` in the language `language`.
fn fenced<'a>(text: &'a str, language: &str) -> &'a str {
    let open = format!("```{language}\n");
    let start = text
        .find(&open)
        .unwrap_or_else(|| panic!("no {language} block in {text}"))
        + open.len();
    let end = text[start..].find("\n```").unwrap();
    &text[start..start + end]
}

/// The registration line of a smoke script, as a reader of the docs would
/// type it: the temporary tree it registers becomes `~`.
fn smoke_registration(script: &str, command: &str) -> String {
    let source = read(format!("crates/duscope-mcp/tests/smoke/{script}.sh"));
    let prefix = format!("step {command} mcp add ");
    let line = source
        .lines()
        .find(|line| line.starts_with(&prefix))
        .unwrap_or_else(|| panic!("{script}.sh registers nothing"));
    let (command_line, _) = line.split_once(" ||").unwrap_or((line, ""));
    command_line
        .trim_start_matches("step ")
        .replace("\"$TREE\"", "~")
}

#[test]
fn p3_pkg_01_server_json_validates_against_the_registry_schema() {
    let schema: Value =
        serde_json::from_str(&read("duscope/tests/mcp_registry/server.schema.json")).unwrap();
    let validator = jsonschema::draft7::new(&schema).unwrap();
    let manifest = manifest();
    let errors: Vec<String> = validator
        .iter_errors(&manifest)
        .map(|e| format!("{}: {e}", e.instance_path()))
        .collect();
    assert!(errors.is_empty(), "{errors:#?}");
    assert_eq!(
        manifest["$schema"], schema["$id"],
        "server.json names another schema than the copy it is held to"
    );
}

/// The release commit moves the manifest with the crates: the registry
/// resolves the package by this version on crates.io.
#[test]
fn p3_pkg_01_server_json_names_the_crate_and_its_version() {
    let manifest = manifest();
    let version = env!("CARGO_PKG_VERSION");
    assert_eq!(manifest["name"], SERVER_NAME);
    assert_eq!(manifest["version"], version);
    let packages = manifest["packages"].as_array().unwrap();
    assert_eq!(packages.len(), 1);
    let package = &packages[0];
    assert_eq!(package["registryType"], "cargo");
    assert_eq!(package["identifier"], env!("CARGO_PKG_NAME"));
    assert_eq!(package["version"], version);
    assert_eq!(package["transport"]["type"], "stdio");
    let arguments = package["packageArguments"].as_array().unwrap();
    assert_eq!(arguments[0]["type"], "positional");
    assert_eq!(arguments[0]["value"], "mcp");
    assert_eq!(arguments[1]["type"], "named");
    assert_eq!(arguments[1]["name"], "--roots");
    assert_eq!(arguments[1]["isRequired"], true);
    assert_eq!(arguments.len(), 2);
}

/// The registry proves a cargo package's owner by this token in the README
/// crates.io renders, which strips HTML comments.
#[test]
fn p3_pkg_01_the_crates_readme_carries_the_registry_name() {
    let readme = read("duscope/README.md");
    let token = format!("mcp-name: {SERVER_NAME}");
    let line = readme
        .lines()
        .find(|line| line.contains(&token))
        .unwrap_or_else(|| panic!("no {token:?} in {readme}"));
    assert!(!line.contains("<!--"), "{line}");
}

#[test]
fn p3_pkg_01_each_recipe_is_the_line_its_smoke_script_runs() {
    let docs = read("docs/mcp.md");
    let readme = read("README.md");
    for (heading, script, command) in CLIENTS {
        let registration = smoke_registration(script, command);
        assert!(registration.ends_with("--roots ~"), "{registration}");
        let recipe = fenced(section(&docs, &format!("### {heading}")), "sh");
        assert_eq!(recipe, registration, "docs/mcp.md, {heading}");
        assert!(
            readme.lines().any(|line| line == registration),
            "the README does not hold {registration:?}"
        );
    }
}

/// `- `name`: description`, one line a tool, in the order the server lists
/// them.
fn listed_tools() -> String {
    let env = TestEnv::new();
    let mut child = env
        .command(duscope_bin())
        .arg("mcp")
        .arg("--roots")
        .arg(env.tmp_dir())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let request = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}"#;
    let mut stdin = child.stdin.take().unwrap();
    writeln!(stdin, "{request}").unwrap();
    drop(stdin);
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "{out:?}");
    let answer: Value = serde_json::from_slice(&out.stdout).unwrap();
    answer["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| {
            format!(
                "- `{}`: {}\n",
                tool["name"].as_str().unwrap(),
                tool["description"].as_str().unwrap()
            )
        })
        .collect()
}

#[test]
fn p3_pkg_01_the_tool_list_is_what_the_server_lists() {
    let docs = read("docs/mcp.md");
    let written = section(&docs, "## The tools");
    let listed = listed_tools();
    assert!(
        written.contains(&listed),
        "the list in docs/mcp.md should read:\n{listed}"
    );
}

/// The recipe's command is run: what it prints is the hook's whole answer.
#[test]
fn p3_pkg_01_the_hook_recipe_asks_before_duscope_clean() {
    let docs = read("docs/mcp.md");
    let settings: Value = serde_json::from_str(fenced(
        section(&docs, "## Ask before `duscope clean`"),
        "json",
    ))
    .unwrap();
    let group = &settings["hooks"]["PreToolUse"][0];
    assert_eq!(group["matcher"], "Bash");
    let hook = &group["hooks"][0];
    assert_eq!(hook["type"], "command");
    assert_eq!(hook["if"], "Bash(duscope clean *)");
    let env = TestEnv::new();
    let out = env
        .command("sh")
        .arg("-c")
        .arg(hook["command"].as_str().unwrap())
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let answer: Value = serde_json::from_slice(&out.stdout).unwrap();
    let decision = &answer["hookSpecificOutput"];
    assert_eq!(decision["hookEventName"], "PreToolUse");
    assert_eq!(decision["permissionDecision"], "ask");
    assert!(decision["permissionDecisionReason"].is_string());
}
