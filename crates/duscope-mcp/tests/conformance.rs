//! P3-MCP-06: the conformance suite. rmcp's client, the oracle D36 names
//! (rmcp 3.9.0, recorded in `schemas/mcp/MAPPING.md`), against the `duscope
//! mcp` binary itself, over its stdin and stdout, the way rmcp's own tests
//! drive a stdio server they start: both protocol revisions' handshakes and
//! the version refusals, discovery, `tools/list`, `tools/call` of every tool
//! the binary lists in both revisions, the errors a client meets, a
//! deadline, cancellation of a running and of a queued call, and a scan's
//! progress.
//!
//! Every line passes through a tap between the two, which checks, once the
//! session is over, that each line the binary wrote is a message rmcp's
//! model reads, and reads as what it answers (an `initialize` as an
//! `InitializeResult`, a `server/discover` as a `DiscoverResult`, a
//! `tools/list` as a `ListToolsResult`, a `tools/call` as a
//! `CallToolResult`, a 2025-11-25 `ping` as an empty result); that the
//! binary sends no request and no notification but progress; that every
//! request is answered once, unless the client cancelled it; that a
//! 2026-07-28 result says it is complete and names the server while a
//! 2025-11-25 result does neither; and that progress goes only to the
//! request that asked for it, before its response. The tap prints both
//! directions as they pass, which a failing test shows.
//!
//! The binary is the one the build put beside this test: `cargo build -p
//! duscope` first, as CI's `mcp-conformance` job does, or run the workspace's
//! tests. A debug build reads `DUSCOPE_TEST_MCP_PROGRESS_MS` and
//! `DUSCOPE_TEST_MCP_DEADLINE_MS` (DEV-118), which shorten the progress
//! interval and the deadline.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::ffi::OsString;
use std::fs::{self, File};
use std::future::Future;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::process::{Child, ExitStatus, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use duscope_testkit::TestEnv;
use rmcp::model::{
    CacheScope, CallToolRequest, CallToolRequestParams, CallToolResult, ClientCapabilities,
    ClientConfig, ClientRequest, DiscoverRequest, DiscoverRequestParams, DiscoverResult,
    EmptyResult, ErrorCode, Implementation, InitializeResult, ListToolsRequest, ListToolsResult,
    PingRequest, ProgressNotificationParam, ProtocolVersion, RequestId, RequestMetaObject,
    ServerJsonRpcMessage, ServerNotification, ServerPeerInfo, ServerResult, Tool,
};
use rmcp::service::{
    ClientInitializeError, ClientLifecycleMode, ClientServiceExt, MaybeSendFuture,
    NotificationContext, PeerRequestOptions, RequestHandle, RunningService, ServiceError,
};
use rmcp::{ClientHandler, RoleClient};
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};
use tokio::net::unix::pipe;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

/// How long a session may take before the test fails: far beyond the
/// longest, which waits out the sandbox's 5-second statement deadline.
const PATIENCE: Duration = Duration::from_secs(60);

/// How soon the reader must answer a request while a call runs: well
/// inside the 5 seconds the endless statement keeps the worker busy.
const AT_ONCE: Duration = Duration::from_secs(3);

/// The tools the binary lists, in the order it lists them.
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

/// The versions every refusal lists.
const SUPPORTED: [&str; 2] = ["2026-07-28", "2025-11-25"];

/// A statement that counts the files under the path's entry.
const COUNT: &str = "SELECT count(*) AS n FROM files WHERE id BETWEEN :root_id AND :root_last";

/// A statement the sandbox runs until its 5-second deadline, and whose
/// verb no cancellation stops: a call that keeps the worker busy.
const ENDLESS: &str =
    "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM n) SELECT max(x) FROM n";

/// Which way a line went.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Way {
    /// From the client to the binary.
    Sent,
    /// From the binary to the client.
    Received,
}

/// Every line of a session, in the order the tap passed it on.
#[derive(Clone, Default)]
struct Wire(Arc<Mutex<Vec<(Way, String)>>>);

impl Wire {
    fn push(&self, way: Way, line: String) {
        self.0.lock().unwrap().push((way, line));
    }

    fn take(&self) -> Vec<(Way, String)> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }
}

/// Copies lines from `from` to `to`, recording each, until `from` ends or
/// `to` is closed; then closes `to`, which the other side reads as the end
/// of its input.
fn relay(
    from: impl Read + Send + 'static,
    mut to: impl Write + Send + 'static,
    way: Way,
    wire: Wire,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let mut reader = BufReader::new(from);
        let mut line = Vec::new();
        loop {
            line.clear();
            match reader.read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            let text = String::from_utf8_lossy(&line).trim_end().to_owned();
            let arrow = if way == Way::Sent { '>' } else { '<' };
            let shown: String = text.chars().take(1_500).collect();
            eprintln!("{arrow} {shown}");
            wire.push(way, text);
            if to.write_all(&line).and_then(|()| to.flush()).is_err() {
                break;
            }
        }
    })
}

/// A `duscope mcp` the tap stands in front of.
struct Spawned {
    child: Child,
    /// The client's ends: what it reads, and what it writes.
    client_reads: OwnedFd,
    client_writes: OwnedFd,
    wire: Wire,
    relays: Vec<JoinHandle<()>>,
    stderr: PathBuf,
}

/// Starts `duscope mcp ARGS` under `env` with `vars` set, behind the tap.
fn spawn(env: &TestEnv, args: &[OsString], vars: &[(&str, &str)]) -> Spawned {
    static SERVERS: AtomicUsize = AtomicUsize::new(0);
    let n = SERVERS.fetch_add(1, Ordering::Relaxed);
    let stderr = env.root().join(format!("server-{n}.log"));
    let mut child = env
        .cmd()
        .arg("mcp")
        .args(args)
        .envs(vars.iter().copied())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(File::create(&stderr).unwrap())
        .spawn()
        .unwrap();
    let wire = Wire::default();
    let (client_reads, tap_writes) = std::io::pipe().unwrap();
    let (tap_reads, client_writes) = std::io::pipe().unwrap();
    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let relays = vec![
        relay(tap_reads, stdin, Way::Sent, wire.clone()),
        relay(stdout, tap_writes, Way::Received, wire.clone()),
    ];
    Spawned {
        child,
        client_reads: OwnedFd::from(client_reads),
        client_writes: OwnedFd::from(client_writes),
        wire,
        relays,
        stderr,
    }
}

/// `--roots ROOT`, then `extra`.
fn roots(root: &Path, extra: &[&str]) -> Vec<OsString> {
    let mut args = vec![OsString::from("--roots"), root.as_os_str().to_owned()];
    args.extend(extra.iter().map(OsString::from));
    args
}

/// What a session left: every line each way, the binary's log, and how it
/// ended.
struct Transcript {
    lines: Vec<(Way, String)>,
    stderr: String,
    status: ExitStatus,
}

/// A request the client sent, as the tap saw it.
struct Sent {
    id: Value,
    method: String,
    /// The version its `_meta` names, if any.
    version: Option<String>,
    progress_token: Option<Value>,
}

impl Transcript {
    /// The messages that went `way`, parsed, with their place in the
    /// session.
    fn messages(&self, way: Way) -> impl Iterator<Item = (usize, Value)> + '_ {
        self.lines
            .iter()
            .enumerate()
            .filter(move |(_, (w, _))| *w == way)
            .map(|(at, (_, line))| (at, serde_json::from_str(line).unwrap()))
    }

    /// The line answering request `id`, and its place in the session.
    fn response(&self, id: &RequestId) -> Option<(usize, &str)> {
        let id = serde_json::to_value(id).unwrap();
        self.lines
            .iter()
            .enumerate()
            .filter(|(_, (way, _))| *way == Way::Received)
            .find(|(_, (_, line))| {
                let message: Value = serde_json::from_str(line).unwrap();
                message.get("method").is_none() && message["id"] == id
            })
            .map(|(at, (_, line))| (at, line.as_str()))
    }

    /// Checks every line the binary wrote against the requests the client
    /// sent (see the module's documentation).
    fn check(&self) {
        let fail = |why: String| -> ! { panic!("{why}\nthe binary's log:\n{}", self.stderr) };
        let mut sent: Vec<Sent> = Vec::new();
        let mut cancelled: Vec<Value> = Vec::new();
        let mut answered: Vec<Value> = Vec::new();
        for (at, (way, line)) in self.lines.iter().enumerate() {
            let message: Value = serde_json::from_str(line)
                .unwrap_or_else(|e| fail(format!("line {at} is not JSON ({e}): {line}")));
            if *way == Way::Sent {
                match (message["method"].as_str(), message.get("id")) {
                    (Some(method), Some(id)) => {
                        let meta = &message["params"]["_meta"];
                        sent.push(Sent {
                            id: id.clone(),
                            method: method.to_owned(),
                            version: meta["io.modelcontextprotocol/protocolVersion"]
                                .as_str()
                                .map(str::to_owned),
                            progress_token: meta.get("progressToken").cloned(),
                        });
                    }
                    (Some("notifications/cancelled"), None) => {
                        cancelled.push(message["params"]["requestId"].clone());
                    }
                    _ => {}
                }
                continue;
            }
            if message["jsonrpc"] != "2.0" {
                fail(format!("line {at} is not JSON-RPC 2.0: {line}"));
            }
            if let Err(e) = serde_json::from_value::<ServerJsonRpcMessage>(message.clone()) {
                fail(format!("rmcp reads no message in line {at} ({e}): {line}"));
            }
            if let Some(method) = message["method"].as_str() {
                if message.get("id").is_some() {
                    fail(format!("the binary sent a request, {method}: {line}"));
                }
                let notification: ServerNotification = serde_json::from_value(message.clone())
                    .unwrap_or_else(|e| fail(format!("line {at} ({e}): {line}")));
                let ServerNotification::ProgressNotification(progress) = notification else {
                    fail(format!("a notification other than progress: {line}"));
                };
                let token = serde_json::to_value(&progress.params.progress_token).unwrap();
                let Some(owner) = sent
                    .iter()
                    .find(|s| s.progress_token.as_ref() == Some(&token))
                else {
                    fail(format!("progress for a token no request named: {line}"));
                };
                if answered.contains(&owner.id) {
                    fail(format!("progress after its request's response: {line}"));
                }
                continue;
            }
            let id = message["id"].clone();
            let Some(request) = sent.iter().find(|s| s.id == id) else {
                fail(format!("line {at} answers no request: {line}"));
            };
            if answered.contains(&id) {
                fail(format!("request {id} answered twice: {line}"));
            }
            answered.push(id);
            if let Some(error) = message.get("error") {
                if !error["code"].is_i64() || !error["message"].is_string() {
                    fail(format!("a malformed error: {line}"));
                }
                continue;
            }
            if let Err(why) = check_result(request, &message["result"]) {
                fail(format!("{why}: {line}"));
            }
        }
        for request in &sent {
            if !answered.contains(&request.id) && !cancelled.contains(&request.id) {
                fail(format!(
                    "request {} ({}) was never answered",
                    request.id, request.method
                ));
            }
        }
    }
}

/// Whether `result` answers `request` as its revision writes it and as
/// rmcp's model reads the method's result.
fn check_result(request: &Sent, result: &Value) -> Result<(), String> {
    match request.version.as_deref() {
        Some("2026-07-28") => {
            if result["resultType"] != "complete" {
                return Err("a 2026-07-28 result without resultType".to_owned());
            }
            let named = &result["_meta"]["io.modelcontextprotocol/serverInfo"];
            if named["name"] != "duscope" {
                return Err("a 2026-07-28 result naming no server".to_owned());
            }
        }
        Some(other) => return Err(format!("a result to a request naming {other}")),
        None => {
            if result.get("resultType").is_some() || result.get("_meta").is_some() {
                return Err("a 2025-11-25 result in 2026-07-28's shape".to_owned());
            }
        }
    }
    match request.method.as_str() {
        "initialize" => read::<InitializeResult>(result),
        "server/discover" => read::<DiscoverResult>(result),
        "tools/list" => read::<ListToolsResult>(result),
        "tools/call" => read::<CallToolResult>(result),
        "ping" if request.version.is_none() => read::<EmptyResult>(result),
        other => Err(format!("{other} has no result")),
    }
    .map_err(|e| format!("rmcp reads no {} result ({e})", request.method))
}

/// Whether `value` reads as a `T`.
fn read<T: DeserializeOwned>(value: &Value) -> Result<(), String> {
    serde_json::from_value::<T>(value.clone())
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Runs `client` on the client's ends of `spawned`, closes them, waits for
/// the binary to end, and checks the session's transcript.
fn run<T, F, Fut>(spawned: Spawned, client: F) -> (T, Transcript)
where
    F: FnOnce(pipe::Receiver, pipe::Sender) -> Fut,
    Fut: Future<Output = T>,
{
    let Spawned {
        mut child,
        client_reads,
        client_writes,
        wire,
        relays,
        stderr,
    } = spawned;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let outcome = runtime.block_on(async move {
        let reader = pipe::Receiver::from_owned_fd(client_reads).unwrap();
        let writer = pipe::Sender::from_owned_fd(client_writes).unwrap();
        tokio::time::timeout(PATIENCE, client(reader, writer)).await
    });
    drop(runtime);
    // The client's ends are closed: the binary reads the end of its input,
    // answers what it holds, and exits.
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if started.elapsed() > PATIENCE {
            child.kill().unwrap();
            panic!(
                "the binary did not exit once its input ended:\n{}",
                fs::read_to_string(&stderr).unwrap_or_default()
            );
        }
        thread::sleep(Duration::from_millis(10));
    };
    for relay in relays {
        relay.join().unwrap();
    }
    let transcript = Transcript {
        lines: wire.take(),
        stderr: fs::read_to_string(&stderr).unwrap_or_default(),
        status,
    };
    let Ok(outcome) = outcome else {
        panic!(
            "the session took longer than {PATIENCE:?}; the binary's log:\n{}",
            transcript.stderr
        );
    };
    transcript.check();
    assert!(
        transcript.status.success(),
        "the binary ended {}:\n{}",
        transcript.status,
        transcript.stderr
    );
    (outcome, transcript)
}

/// rmcp's client, passing each progress notification it receives to the
/// test.
#[derive(Clone)]
struct Client {
    config: ClientConfig,
    progress: UnboundedSender<ProgressNotificationParam>,
}

impl ClientHandler for Client {
    fn on_progress(
        &self,
        params: ProgressNotificationParam,
        _: NotificationContext<RoleClient>,
    ) -> impl Future<Output = ()> + MaybeSendFuture + '_ {
        let _ = self.progress.send(params);
        std::future::ready(())
    }

    fn get_info(&self) -> ClientConfig {
        self.config.clone()
    }
}

type Running = RunningService<RoleClient, Client>;

/// The client's identity, speaking `version` in an `initialize`.
fn config(version: ProtocolVersion) -> ClientConfig {
    ClientConfig::new(
        ClientCapabilities::default(),
        Implementation::new("duscope-conformance", "0"),
    )
    .with_protocol_version(version)
}

/// The 2026-07-28 lifecycle: `server/discover`, then a version in every
/// request's `_meta`.
fn discover() -> ClientLifecycleMode {
    ClientLifecycleMode::Discover {
        preferred_versions: vec![ProtocolVersion::V_2026_07_28],
    }
}

/// Opens rmcp's client on `spawned` in `lifecycle`, speaking `version` in an
/// `initialize`, runs `body` with it and the progress it receives, then
/// closes it.
fn session<T, F, Fut>(
    spawned: Spawned,
    lifecycle: ClientLifecycleMode,
    version: ProtocolVersion,
    body: F,
) -> (T, Transcript)
where
    F: FnOnce(Running, UnboundedReceiver<ProgressNotificationParam>) -> Fut,
    Fut: Future<Output = (T, Running)>,
{
    run(spawned, move |reader, writer| async move {
        let (progress, observed) = unbounded_channel();
        let client = Client {
            config: config(version),
            progress,
        }
        .serve_with_lifecycle((reader, writer), lifecycle)
        .await
        .unwrap_or_else(|e| panic!("rmcp could not open the session: {e}"));
        let (value, client) = body(client, observed).await;
        client.cancel().await.unwrap();
        value
    })
}

/// Has rmcp's client open `lifecycle` on `spawned`, speaking `version` in an
/// `initialize`: the server as it recorded it, or its error.
fn handshake(
    spawned: Spawned,
    lifecycle: ClientLifecycleMode,
    version: ProtocolVersion,
) -> (Result<ServerPeerInfo, ClientInitializeError>, Transcript) {
    run(spawned, move |reader, writer| async move {
        let (progress, _observed) = unbounded_channel();
        let opened = Client {
            config: config(version),
            progress,
        }
        .serve_with_lifecycle((reader, writer), lifecycle)
        .await;
        let client = opened?;
        let info = client.peer_info().map(|info| (*info).clone());
        client.cancel().await.unwrap();
        Ok(info.expect("rmcp recorded no server"))
    })
}

/// A `tools/call` of `tool` with `arguments`.
fn tool_call(tool: &'static str, arguments: &Value) -> ClientRequest {
    let mut params = CallToolRequestParams::new(tool);
    if let Some(arguments) = arguments.as_object() {
        params = params.with_arguments(arguments.clone());
    }
    ClientRequest::CallToolRequest(CallToolRequest::new(params))
}

/// Sends a `tools/call` of `tool` with `arguments`: its handle.
async fn start_call(
    client: &Running,
    tool: &'static str,
    arguments: &Value,
) -> RequestHandle<RoleClient> {
    client
        .send_cancellable_request(tool_call(tool, arguments), PeerRequestOptions::no_options())
        .await
        .unwrap()
}

/// The result `handle` is answered with.
async fn result_of(handle: RequestHandle<RoleClient>) -> CallToolResult {
    match handle.await_response().await {
        Ok(ServerResult::CallToolResult(result)) => result,
        other => panic!("not a tool result: {other:?}"),
    }
}

/// Calls `tool` with `arguments`: the request's id and its result.
async fn call(
    client: &Running,
    tool: &'static str,
    arguments: &Value,
) -> (RequestId, CallToolResult) {
    let handle = start_call(client, tool, arguments).await;
    let id = handle.id.clone();
    (id, result_of(handle).await)
}

/// The one-line text block of `result`.
fn text(result: &CallToolResult) -> String {
    assert_eq!(result.content.len(), 1, "{result:?}");
    let block = serde_json::to_value(&result.content[0]).unwrap();
    assert_eq!(block["type"], "text", "{block}");
    let text = block["text"].as_str().unwrap().to_owned();
    assert!(!text.is_empty() && !text.contains('\n'), "{text:?}");
    text
}

/// The envelope of `result`, which must have succeeded.
fn envelope(result: &CallToolResult) -> &Value {
    assert_eq!(result.is_error, Some(false), "{result:?}");
    text(result);
    let envelope = result.structured_content.as_ref().unwrap();
    assert_eq!(envelope["ok"], true, "{envelope}");
    envelope
}

/// The error envelope's code of `result`, which must have failed, and whose
/// text must be the error's message and hint.
fn error_code(result: &CallToolResult) -> String {
    assert_eq!(result.is_error, Some(true), "{result:?}");
    let text = text(result);
    let envelope = result.structured_content.as_ref().unwrap();
    assert_eq!(envelope["ok"], false, "{envelope}");
    let error = &envelope["error"];
    assert!(
        text.starts_with(error["message"].as_str().unwrap()),
        "{text}"
    );
    assert!(text.ends_with(error["hint"].as_str().unwrap()), "{text}");
    error["code"].as_str().unwrap().to_owned()
}

/// The JSON-RPC error a request was refused with.
fn rpc_error<T: std::fmt::Debug>(outcome: Result<T, ServiceError>) -> rmcp::ErrorData {
    match outcome {
        Err(ServiceError::McpError(error)) => error,
        other => panic!("not refused with a JSON-RPC error: {other:?}"),
    }
}

/// `duscope ARGS --json` under `env`: what it printed, without the newline.
fn cli_json(env: &TestEnv, args: &[OsString]) -> String {
    let out = env.cmd().args(args).arg("--json").output().unwrap();
    String::from_utf8(out.stdout)
        .unwrap()
        .trim_end_matches('\n')
        .to_owned()
}

/// `structuredContent` as a line holds it, byte for byte.
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

/// A small tree under the work directory, canonical: a directory that is
/// the largest, a project with its dependencies, and a log file.
fn small_tree(env: &TestEnv) -> PathBuf {
    let root = env.work_dir().canonicalize().unwrap().join("tree");
    let files: [(&str, usize); 5] = [
        ("largest-dir/sub/blob", 300_000),
        ("largest-dir/notes.txt", 100_000),
        ("app/package.json", 20),
        ("app/node_modules/pkg/index.js", 20_000),
        ("old.log", 5_000),
    ];
    for (path, size) in files {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, vec![b'x'; size]).unwrap();
    }
    root
}

/// A tree of `dirs` directories of 50 files each under the work directory,
/// canonical: entries for a scan to count.
fn wide_tree(env: &TestEnv, dirs: usize) -> PathBuf {
    let root = env.work_dir().canonicalize().unwrap().join("wide");
    for dir in 0..dirs {
        let dir = root.join(format!("d{dir}"));
        fs::create_dir_all(&dir).unwrap();
        for file in 0..50 {
            fs::write(dir.join(format!("f{file}")), b"x").unwrap();
        }
    }
    root
}

/// The `instructions` the binary gives: the agent briefing, cut to 4,096
/// characters as a JavaScript string counts them.
fn instructions(env: &TestEnv) -> String {
    let out = env.cmd().arg("agent").output().unwrap();
    assert!(out.status.success(), "{out:?}");
    let briefing = String::from_utf8(out.stdout).unwrap();
    let mut units = 0;
    briefing
        .chars()
        .take_while(|c| {
            units += c.len_utf16();
            units <= 4_096
        })
        .collect()
}

/// Checks what rmcp recorded of the server in either revision.
fn assert_server(env: &TestEnv, info: &ServerPeerInfo) {
    assert_eq!(
        serde_json::to_value(&info.server_info).unwrap(),
        json!({"name": "duscope", "version": env!("CARGO_PKG_VERSION")})
    );
    assert_eq!(
        info.instructions.as_deref(),
        Some(instructions(env).as_str())
    );
    let tools = info
        .capabilities
        .tools
        .as_ref()
        .expect("no tools capability");
    assert_eq!(tools.list_changed, Some(false));
    assert!(info.capabilities.resources.is_none());
    assert!(info.capabilities.prompts.is_none());
}

// Handshakes and discovery.

#[test]
fn p3_mcp_06_rmcp_discovers_the_binary_on_2026_07_28() {
    let env = TestEnv::new();
    let root = small_tree(&env);
    let ((info, discovered), transcript) = session(
        spawn(&env, &roots(&root, &[]), &[]),
        discover(),
        ProtocolVersion::LATEST,
        |client, _| async move {
            let info = client.peer_info().map(|info| (*info).clone()).unwrap();
            // Past rmcp's cache of the startup's answer: the request again.
            let request =
                ClientRequest::DiscoverRequest(DiscoverRequest::new(DiscoverRequestParams {}));
            let discovered = match client.send_request(request).await.unwrap() {
                ServerResult::DiscoverResult(result) => result,
                other => panic!("not a discovery result: {other:?}"),
            };
            ((info, discovered), client)
        },
    );
    assert_eq!(info.protocol_version, ProtocolVersion::V_2026_07_28);
    assert_server(&env, &info);
    let supported: Vec<String> = discovered
        .supported_versions
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(supported, SUPPORTED);
    assert_eq!(discovered.ttl_ms, 3_600_000);
    assert_eq!(discovered.cache_scope, CacheScope::Public);
    assert_eq!(discovered.capabilities, info.capabilities);
    // No initialize: the 2026-07-28 lifecycle has none.
    assert!(
        transcript
            .messages(Way::Sent)
            .all(|(_, message)| message["method"] != "initialize"),
        "{}",
        transcript.stderr
    );
}

#[test]
fn p3_mcp_06_rmcp_initializes_the_binary_on_2025_11_25_and_pings_it() {
    let env = TestEnv::new();
    let root = small_tree(&env);
    let (info, transcript) = session(
        spawn(&env, &roots(&root, &[]), &[]),
        ClientLifecycleMode::Initialize,
        ProtocolVersion::V_2025_11_25,
        |client, _| async move {
            let info = client.peer_info().map(|info| (*info).clone()).unwrap();
            let pong = client
                .send_request(ClientRequest::PingRequest(PingRequest::default()))
                .await
                .unwrap();
            assert!(matches!(pong, ServerResult::EmptyResult(_)), "{pong:?}");
            (info, client)
        },
    );
    assert_eq!(info.protocol_version, ProtocolVersion::V_2025_11_25);
    assert_server(&env, &info);
    // The handshake closed with notifications/initialized.
    let methods: Vec<String> = transcript
        .messages(Way::Sent)
        .filter_map(|(_, message)| message["method"].as_str().map(str::to_owned))
        .collect();
    assert_eq!(
        methods[..3],
        ["initialize", "notifications/initialized", "ping"]
    );
}

#[test]
fn p3_mcp_06_rmcp_initializing_with_2026_07_28_is_served_2025_11_25() {
    let env = TestEnv::new();
    let root = small_tree(&env);
    let (info, _) = handshake(
        spawn(&env, &roots(&root, &[]), &[]),
        ClientLifecycleMode::Initialize,
        ProtocolVersion::V_2026_07_28,
    );
    assert_eq!(
        info.unwrap().protocol_version,
        ProtocolVersion::V_2025_11_25
    );
}

#[test]
fn p3_mcp_06_rmcp_probing_the_binary_settles_on_2026_07_28() {
    let env = TestEnv::new();
    let root = small_tree(&env);
    let (info, _) = handshake(
        spawn(&env, &roots(&root, &[]), &[]),
        ClientLifecycleMode::Auto {
            preferred_versions: vec![ProtocolVersion::V_2026_07_28],
            legacy_version: Some(ProtocolVersion::V_2025_11_25),
        },
        ProtocolVersion::V_2025_11_25,
    );
    assert_eq!(
        info.unwrap().protocol_version,
        ProtocolVersion::V_2026_07_28
    );
}

#[test]
fn p3_mcp_06_rmcp_retries_a_version_the_binary_refuses_from_its_list() {
    let env = TestEnv::new();
    let root = small_tree(&env);
    let future: ProtocolVersion = serde_json::from_value(json!("2099-01-01")).unwrap();
    let (info, transcript) = handshake(
        spawn(&env, &roots(&root, &[]), &[]),
        ClientLifecycleMode::Discover {
            preferred_versions: vec![future, ProtocolVersion::V_2026_07_28],
        },
        ProtocolVersion::LATEST,
    );
    assert_eq!(
        info.unwrap().protocol_version,
        ProtocolVersion::V_2026_07_28
    );
    let refusal = transcript
        .messages(Way::Received)
        .find_map(|(_, message)| message.get("error").cloned())
        .expect("no refusal");
    assert_eq!(refusal["code"], -32_022, "{refusal}");
    assert_eq!(
        refusal["data"],
        json!({"supported": SUPPORTED, "requested": "2099-01-01"})
    );
}

#[test]
fn p3_mcp_06_the_binary_refuses_a_2024_11_05_client_with_its_list() {
    let env = TestEnv::new();
    let root = small_tree(&env);
    let (refused, _) = handshake(
        spawn(&env, &roots(&root, &[]), &[]),
        ClientLifecycleMode::Initialize,
        ProtocolVersion::V_2024_11_05,
    );
    let ClientInitializeError::JsonRpcError(error) = refused.unwrap_err() else {
        panic!("not refused with a JSON-RPC error");
    };
    assert_eq!(error.code, ErrorCode::INVALID_PARAMS);
    assert_eq!(
        error.data,
        Some(json!({"supported": SUPPORTED, "requested": "2024-11-05"}))
    );
}

// tools/list.

/// Checks `tool` as the binary lists it, declaring `max_chars`.
fn assert_tool(tool: &Tool, max_chars: u64) {
    let name = tool.name.as_ref();
    assert!(
        name.len() <= 63
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'),
        "{name}"
    );
    assert!(
        tool.description.as_deref().is_some_and(|d| !d.is_empty()),
        "{name}"
    );
    // Each schema as its hand-written file has it; an output schema only
    // where the tool has the file.
    let fixture = |which: &str| -> Option<Map<String, Value>> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../schemas/mcp")
            .join(format!("{name}.{which}.json"));
        let text = fs::read_to_string(path).ok()?;
        Some(serde_json::from_str(&text).unwrap())
    };
    assert_eq!(
        Some((*tool.input_schema).clone()),
        fixture("input"),
        "{name}"
    );
    assert_eq!(
        tool.output_schema.as_deref().cloned(),
        fixture("output"),
        "{name}"
    );
    let hints = tool.annotations.as_ref().unwrap();
    let scan = name == "duscope_scan";
    assert_eq!(hints.read_only_hint, Some(!scan), "{name}");
    assert_eq!(hints.destructive_hint, Some(false), "{name}");
    assert_eq!(hints.idempotent_hint, Some(!scan), "{name}");
    assert_eq!(hints.open_world_hint, Some(false), "{name}");
    let meta = &tool.meta.as_ref().unwrap().0;
    assert_eq!(
        meta.get("anthropic/maxResultSizeChars"),
        Some(&json!(max_chars)),
        "{name}"
    );
    assert!(
        meta.get("anthropic/requiresUserInteraction").is_none(),
        "{name}"
    );
}

#[test]
fn p3_mcp_06_tools_list_through_rmcp_names_every_tool_with_its_schema_and_hints() {
    let env = TestEnv::new();
    let root = small_tree(&env);
    let args = roots(&root, &["--max-chars", "5000"]);
    let (modern, _) = session(
        spawn(&env, &args, &[]),
        discover(),
        ProtocolVersion::LATEST,
        |client, _| async move { (client.list_tools(None).await.unwrap(), client) },
    );
    let names: Vec<&str> = modern.tools.iter().map(|tool| tool.name.as_ref()).collect();
    assert_eq!(names, SHIPPED);
    assert_eq!(modern.next_cursor, None);
    assert_eq!(modern.ttl_ms, Some(3_600_000));
    assert_eq!(modern.cache_scope, Some(CacheScope::Public));
    for tool in &modern.tools {
        assert_tool(tool, 5_000);
    }
    // The 2025-11-25 revision lists the same tools, without how long the
    // list holds.
    let (legacy, _) = session(
        spawn(&env, &args, &[]),
        ClientLifecycleMode::Initialize,
        ProtocolVersion::V_2025_11_25,
        |client, _| async move { (client.list_tools(None).await.unwrap(), client) },
    );
    assert_eq!(legacy.tools, modern.tools);
    assert_eq!((legacy.ttl_ms, legacy.cache_scope), (None, None));
}

#[test]
fn p3_mcp_06_tools_list_is_the_same_in_every_answer() {
    let env = TestEnv::new();
    let root = small_tree(&env);
    let list = |env: &TestEnv| {
        let (id, transcript) = session(
            spawn(env, &roots(&root, &[]), &[]),
            discover(),
            ProtocolVersion::LATEST,
            |client, _| async move {
                let request = ClientRequest::ListToolsRequest(ListToolsRequest::default());
                let handle = client
                    .send_cancellable_request(request, PeerRequestOptions::no_options())
                    .await
                    .unwrap();
                let id = handle.id.clone();
                handle.await_response().await.unwrap();
                (id, client)
            },
        );
        transcript.response(&id).unwrap().1.to_owned()
    };
    // Byte for byte, from two processes: a client's prompt cache holds.
    assert_eq!(list(&env), list(&env));
}

// tools/call.

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

/// Every tool on `root`, saved as `id`, the server's one root, beside the
/// command line that prints the same envelope.
fn every_tool(root: &Path, id: &str) -> Vec<Case> {
    let file = root.join("largest-dir/notes.txt");
    let mut under = OsString::from("--under=");
    under.push(root);
    vec![
        // A scan younger than max_age is returned: the saved one.
        (
            "duscope_scan",
            json!({"root": root, "max_age": "1h"}),
            line("scan --save --reuse=1h", root),
        ),
        ("duscope_scans", json!({}), line("scans", root)),
        (
            "duscope_summary",
            json!({}),
            line("summary --no-scan", root),
        ),
        (
            "duscope_top",
            json!({"limit": 3}),
            line("top --no-scan --limit=3", root),
        ),
        (
            "duscope_by_type",
            json!({}),
            line("by-type --no-scan", root),
        ),
        ("duscope_stale", json!({}), line("stale --no-scan", root)),
        (
            "duscope_caches",
            json!({"min_size": "0"}),
            line("caches --no-scan --min-size=0", root),
        ),
        (
            "duscope_explain",
            json!({"path": file}),
            line("explain", &file),
        ),
        (
            "duscope_query",
            json!({"sql": COUNT}),
            vec![
                "query".into(),
                "--no-scan".into(),
                "--scan".into(),
                id.into(),
                format!("--sql={COUNT}").into(),
                root.as_os_str().to_owned(),
            ],
        ),
        (
            "duscope_schema",
            json!({}),
            line(&format!("schema --scan {id}"), root),
        ),
        (
            "duscope_diff",
            json!({"old": id, "new": "latest"}),
            vec!["diff".into(), id.into(), "latest".into(), under],
        ),
    ]
}

/// rmcp's client, speaking `lifecycle`, scans the small tree, then calls
/// every tool the binary lists: each answers with its verb's `--json`, byte
/// for byte, and a one-line text block.
fn calls_every_tool(lifecycle: ClientLifecycleMode, version: ProtocolVersion) {
    let env = TestEnv::new();
    let root = small_tree(&env);
    let scan_root = root.clone();
    let ((listed, id, called), transcript) = session(
        spawn(&env, &roots(&root, &[]), &[]),
        lifecycle,
        version,
        |client, _| async move {
            let listed: Vec<String> = client
                .list_all_tools()
                .await
                .unwrap()
                .into_iter()
                .map(|tool| tool.name.into_owned())
                .collect();
            let (_, scanned) = call(&client, "duscope_scan", &json!({"root": scan_root})).await;
            let id = envelope(&scanned)["scan"]["id"]
                .as_str()
                .unwrap()
                .to_owned();
            let mut called = Vec::new();
            for (tool, arguments, args) in every_tool(&scan_root, &id) {
                let (request, result) = call(&client, tool, &arguments).await;
                envelope(&result);
                called.push((tool, request, result, args));
            }
            ((listed, id, called), client)
        },
    );
    for name in &listed {
        assert!(
            called.iter().any(|(tool, ..)| tool == name),
            "{name} is listed and was not called"
        );
    }
    for (tool, request, result, args) in &called {
        let printed = cli_json(&env, args);
        let (_, line) = transcript.response(request).unwrap();
        assert_eq!(structured_content(line), printed, "{tool}");
        assert_eq!(
            result.structured_content.as_ref().unwrap(),
            &serde_json::from_str::<Value>(&printed).unwrap(),
            "{tool}"
        );
    }
    // One scan saved, by the first call; the second found it.
    let scans: Value = serde_json::from_str(&cli_json(&env, &line("scans", &root))).unwrap();
    assert_eq!(scans["entries"].as_array().unwrap().len(), 1, "{scans}");
    assert_eq!(scans["entries"][0]["id"], id);
}

#[test]
fn p3_mcp_06_rmcp_calls_every_tool_on_2026_07_28() {
    calls_every_tool(discover(), ProtocolVersion::LATEST);
}

#[test]
fn p3_mcp_06_rmcp_calls_every_tool_on_2025_11_25() {
    calls_every_tool(
        ClientLifecycleMode::Initialize,
        ProtocolVersion::V_2025_11_25,
    );
}

// Errors.

#[test]
fn p3_mcp_06_errors_reach_rmcp_as_the_protocol_has_them() {
    let env = TestEnv::new();
    let root = small_tree(&env);
    session(
        spawn(&env, &roots(&root, &[]), &[]),
        discover(),
        ProtocolVersion::LATEST,
        |client, _| async move {
            // A tool's failure is its result, with the error envelope and
            // what to do next as its text.
            let (_, unsaved) = call(&client, "duscope_summary", &json!({})).await;
            assert_eq!(error_code(&unsaved), "scan_not_found");
            let (_, outside) = call(&client, "duscope_top", &json!({"path": "/"})).await;
            assert_eq!(error_code(&outside), "refused_path");
            for refused in [
                json!({"limit": 0}),
                json!({"limit": "3"}),
                json!({"nope": 1}),
            ] {
                let (_, result) = call(&client, "duscope_top", &refused).await;
                assert_eq!(error_code(&result), "usage", "{refused}");
            }
            // A tool there is none of, a method there is none of, and a
            // version the binary does not serve are protocol errors.
            let unknown = client
                .call_tool(CallToolRequestParams::new("duscope_nothing"))
                .await;
            assert_eq!(rpc_error(unknown).code, ErrorCode::INVALID_PARAMS);
            let resources = client.list_resources(None).await;
            assert_eq!(rpc_error(resources).code, ErrorCode::METHOD_NOT_FOUND);
            // 2026-07-28 has no ping: the binary refuses it as rmcp's own
            // server does (DEV-139).
            let ping = client
                .send_request_with_option(
                    ClientRequest::PingRequest(PingRequest::default()),
                    PeerRequestOptions::no_options(),
                )
                .await
                .unwrap()
                .await_response()
                .await;
            assert_eq!(rpc_error(ping).code, ErrorCode::METHOD_NOT_FOUND);
            let mut future = RequestMetaObject::new();
            future.set_protocol_version(serde_json::from_value(json!("2099-01-01")).unwrap());
            let request =
                ClientRequest::DiscoverRequest(DiscoverRequest::new(DiscoverRequestParams {}));
            let refused = client
                .send_request_with_option(
                    request,
                    PeerRequestOptions::no_options().with_meta(future),
                )
                .await
                .unwrap()
                .await_response()
                .await;
            let refused = rpc_error(refused);
            assert_eq!(refused.code, ErrorCode::UNSUPPORTED_PROTOCOL_VERSION);
            assert_eq!(
                refused.data,
                Some(json!({"supported": SUPPORTED, "requested": "2099-01-01"}))
            );
            // None of it ended the session.
            assert_eq!(
                client.list_tools(None).await.unwrap().tools.len(),
                SHIPPED.len()
            );
            ((), client)
        },
    );
}

#[test]
fn p3_mcp_06_a_call_past_its_deadline_reaches_rmcp_as_deadline_exceeded() {
    let env = TestEnv::new();
    let root = small_tree(&env);
    session(
        spawn(
            &env,
            &roots(&root, &[]),
            &[("DUSCOPE_TEST_MCP_DEADLINE_MS", "0")],
        ),
        discover(),
        ProtocolVersion::LATEST,
        |client, _| async move {
            let (_, scanned) = call(&client, "duscope_scan", &json!({})).await;
            assert_eq!(error_code(&scanned), "deadline_exceeded");
            assert!(scanned.structured_content.as_ref().unwrap()["error"]["retryable"] == true);
            assert_eq!(
                client.list_tools(None).await.unwrap().tools.len(),
                SHIPPED.len()
            );
            ((), client)
        },
    );
}

// Cancellation and concurrency.

#[test]
fn p3_mcp_06_rmcp_cancels_a_running_call_which_is_never_answered() {
    let env = TestEnv::new();
    let root = small_tree(&env);
    let (cancelled, transcript) = session(
        spawn(&env, &roots(&root, &["--log-level", "debug"]), &[]),
        discover(),
        ProtocolVersion::LATEST,
        |client, _| async move {
            let (_, scanned) = call(&client, "duscope_scan", &json!({})).await;
            envelope(&scanned);
            let endless = start_call(&client, "duscope_query", &json!({"sql": ENDLESS})).await;
            let cancelled = endless.id.clone();
            // The reader answers while the statement runs, so the call has
            // started by the time it is cancelled.
            let asked = Instant::now();
            assert_eq!(
                client.list_tools(None).await.unwrap().tools.len(),
                SHIPPED.len()
            );
            assert!(asked.elapsed() < AT_ONCE, "{:?}", asked.elapsed());
            tokio::time::sleep(Duration::from_millis(200)).await;
            endless
                .cancel(Some("the conformance suite cancelled it".to_owned()))
                .await
                .unwrap();
            // The next call runs once the statement's verb has returned.
            let (_, listed) = call(&client, "duscope_scans", &json!({})).await;
            envelope(&listed);
            (cancelled, client)
        },
    );
    assert!(transcript.response(&cancelled).is_none());
    assert!(
        transcript
            .stderr
            .contains(&format!("request {cancelled} is told to stop")),
        "{}",
        transcript.stderr
    );
}

#[test]
fn p3_mcp_06_rmcp_cancels_a_queued_call_which_never_runs() {
    let env = TestEnv::new();
    let root = small_tree(&env);
    let queued_root = root.join("largest-dir");
    let ((first, queued, last, probes), transcript) = session(
        spawn(&env, &roots(&root, &["--log-level", "debug"]), &[]),
        discover(),
        ProtocolVersion::LATEST,
        |client, _| async move {
            let (_, scanned) = call(&client, "duscope_scan", &json!({})).await;
            envelope(&scanned);
            let endless = start_call(&client, "duscope_query", &json!({"sql": ENDLESS})).await;
            let scan = start_call(&client, "duscope_scan", &json!({"root": queued_root})).await;
            let scans = start_call(&client, "duscope_scans", &json!({})).await;
            let (first, queued, last) = (endless.id.clone(), scan.id.clone(), scans.id.clone());
            // Discovery and tools/list are answered at once, ahead of the
            // calls queued behind the running one.
            let asked = Instant::now();
            let mut probes = Vec::new();
            for request in [
                ClientRequest::DiscoverRequest(DiscoverRequest::new(DiscoverRequestParams {})),
                ClientRequest::ListToolsRequest(ListToolsRequest::default()),
            ] {
                let handle = client
                    .send_cancellable_request(request, PeerRequestOptions::no_options())
                    .await
                    .unwrap();
                probes.push(handle.id.clone());
                handle.await_response().await.unwrap();
            }
            assert!(asked.elapsed() < AT_ONCE, "{:?}", asked.elapsed());
            scan.cancel(Some("the conformance suite cancelled it".to_owned()))
                .await
                .unwrap();
            // The statement runs into the sandbox's deadline.
            assert_eq!(error_code(&result_of(endless).await), "sql_timeout");
            // The queued scan never ran: the one scan saved is the first.
            let listed = result_of(scans).await;
            assert_eq!(envelope(&listed)["entries"].as_array().unwrap().len(), 1);
            ((first, queued, last, probes), client)
        },
    );
    assert!(transcript.response(&queued).is_none());
    assert!(
        transcript
            .stderr
            .contains(&format!("request {queued} leaves the queue")),
        "{}",
        transcript.stderr
    );
    // In the order the binary answered: the probes, then the calls in the
    // order they came.
    let at = |id: &RequestId| transcript.response(id).unwrap().0;
    for probe in &probes {
        assert!(at(probe) < at(&first), "{probe}");
    }
    assert!(at(&first) < at(&last));
}

// Progress.

#[test]
fn p3_mcp_06_rmcp_observes_the_progress_of_a_scan() {
    let env = TestEnv::new();
    let root = wide_tree(&env, 100);
    let scan_root = root.clone();
    let ((token, result, seen), transcript) = session(
        spawn(
            &env,
            &roots(&root, &[]),
            &[("DUSCOPE_TEST_MCP_PROGRESS_MS", "1")],
        ),
        discover(),
        ProtocolVersion::LATEST,
        |client, mut observed| async move {
            let handle = start_call(&client, "duscope_scan", &json!({"root": scan_root})).await;
            let token = handle.progress_token.clone();
            let result = result_of(handle).await;
            let mut seen = Vec::new();
            while let Ok(progress) = observed.try_recv() {
                seen.push(progress);
            }
            ((token, result, seen), client)
        },
    );
    let scanned = &envelope(&result)["scan"];
    let entries = scanned["file_count"].as_u64().unwrap() + scanned["dir_count"].as_u64().unwrap();
    assert_eq!(entries, 5_101);
    assert!(!seen.is_empty(), "no progress");
    let mut last = 0.0;
    for progress in &seen {
        assert_eq!(progress.progress_token, token, "{progress:?}");
        assert!(progress.progress > last, "{seen:?}");
        assert!(
            progress.progress <= f64::from(u32::try_from(entries).unwrap()),
            "{seen:?}"
        );
        assert_eq!(progress.total, None, "{progress:?}");
        last = progress.progress;
    }
    // The tap saw each one on the wire before the response.
    let notified = transcript
        .messages(Way::Received)
        .filter(|(_, message)| message["method"] == "notifications/progress")
        .count();
    assert_eq!(notified, seen.len());
}

// The oracle.

#[test]
fn p3_mcp_06_mapping_records_the_rmcp_oracle_cargo_pins() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let read = |path: &str| fs::read_to_string(workspace.join(path)).unwrap();
    let mapping = read("schemas/mcp/MAPPING.md");
    let recorded = mapping
        .lines()
        .find_map(|line| line.strip_prefix("Oracle: rmcp "))
        .and_then(|rest| rest.split([' ', ',']).next())
        .map(|version| version.trim_end_matches('.').to_owned())
        .filter(|version| !version.is_empty())
        .expect("MAPPING.md has no `Oracle: rmcp X.Y.Z` line");
    // The pin in the workspace's dependencies: exact.
    let manifest = read("Cargo.toml");
    let pin = manifest
        .lines()
        .find(|line| line.starts_with("rmcp = "))
        .expect("Cargo.toml pins no rmcp");
    assert!(pin.contains(&format!("version = \"={recorded}\"")), "{pin}");
    // The version the lock file resolves, which the suite ran.
    let lock = read("Cargo.lock");
    let locked = lock
        .split("[[package]]")
        .find(|package| package.contains("\nname = \"rmcp\"\n"))
        .and_then(|package| {
            package
                .lines()
                .find_map(|line| line.strip_prefix("version = \""))
                .map(|v| v.trim_end_matches('"').to_owned())
        })
        .expect("Cargo.lock holds no rmcp");
    assert_eq!(locked, recorded);
}

// The tap.

#[test]
fn p3_mcp_06_the_tap_refuses_what_the_protocol_does_not_allow() {
    use Way::{Received as R, Sent as S};
    use std::os::unix::process::ExitStatusExt;
    let modern = |id: u64, method: &str, extra: &str| {
        format!(
            r#"{{"jsonrpc":"2.0","id":{id},"method":"{method}","params":{{"_meta":{{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{{}},"progressToken":{id}}}{extra}}}}}"#
        )
    };
    let named =
        r#""_meta":{"io.modelcontextprotocol/serverInfo":{"name":"duscope","version":"0"}}"#;
    let listed = format!(
        r#"{{"jsonrpc":"2.0","id":1,"result":{{"tools":[],"resultType":"complete",{named}}}}}"#
    );
    let called = format!(
        r#"{{"jsonrpc":"2.0","id":2,"result":{{"content":[{{"type":"text","text":"t"}}],"structuredContent":{{}},"isError":false,"resultType":"complete",{named}}}}}"#
    );
    let list = modern(1, "tools/list", "");
    let call = modern(2, "tools/call", r#","name":"duscope_top""#);
    let progress = r#"{"jsonrpc":"2.0","method":"notifications/progress","params":{"progressToken":2,"progress":1}}"#;
    let legacy_list = r#"{"jsonrpc":"2.0","id":3,"method":"tools/list"}"#;
    let cancel = r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":2}}"#;
    let check = |lines: &[(Way, &str)]| {
        let transcript = Transcript {
            lines: lines
                .iter()
                .map(|(way, line)| (*way, (*line).to_owned()))
                .collect(),
            stderr: String::new(),
            status: ExitStatus::from_raw(0),
        };
        std::panic::catch_unwind(|| transcript.check()).is_ok()
    };
    // A session that keeps every rule.
    assert!(check(&[
        (S, &list),
        (R, &listed),
        (S, &call),
        (R, progress),
        (R, &called)
    ]));
    assert!(check(&[(S, &call), (S, cancel)]));
    assert!(check(&[
        (S, legacy_list),
        (R, r#"{"jsonrpc":"2.0","id":3,"result":{"tools":[]}}"#)
    ]));
    // Each broken.
    let broken: [&[(Way, &str)]; 10] = [
        &[(S, &list), (R, "not json")],
        &[
            (S, &list),
            (R, &listed.replace(r#""resultType":"complete","#, "")),
        ],
        &[(S, &list), (R, &listed.replace("duscope", "other"))],
        &[
            (S, legacy_list),
            (R, &listed.replace(r#""id":1"#, r#""id":3"#)),
        ],
        &[
            (S, &list),
            (R, &listed.replace(r#""tools":[]"#, r#""tools":"none""#)),
        ],
        &[
            (S, &call),
            (
                R,
                &called.replace(
                    r#""content":[{"type":"text","text":"t"}]"#,
                    r#""content":"t""#,
                ),
            ),
        ],
        &[(S, &list), (R, &listed), (R, &listed)],
        &[(S, &call), (R, &called), (R, progress)],
        &[
            (S, &list),
            (R, r#"{"jsonrpc":"2.0","id":9,"method":"ping"}"#),
            (R, &listed),
        ],
        &[(S, &list), (S, &call), (R, &listed)],
    ];
    for (n, lines) in broken.iter().enumerate() {
        assert!(!check(lines), "case {n} passed the tap");
    }
}

// P5-MCP-01: `duscope_delete`, through rmcp's client.

/// Both protocol revisions' sessions: the lifecycle and the version an
/// `initialize` speaks.
fn revisions() -> [(ClientLifecycleMode, ProtocolVersion); 2] {
    [
        (discover(), ProtocolVersion::LATEST),
        (
            ClientLifecycleMode::Initialize,
            ProtocolVersion::V_2025_11_25,
        ),
    ]
}

/// A tree under the work directory, canonical, with a saved scan: two files
/// and a directory, one file of which no call ever names.
fn delete_tree(env: &TestEnv) -> PathBuf {
    let root = env.work_dir().canonicalize().unwrap().join("trash-tree");
    fs::create_dir_all(root.join("dir")).unwrap();
    fs::write(root.join("keep.bin"), vec![1u8; 3_000]).unwrap();
    fs::write(root.join("one.bin"), vec![2u8; 4_000]).unwrap();
    fs::write(root.join("dir/two.bin"), vec![3u8; 5_000]).unwrap();
    let out = env
        .cmd()
        .args(["scan", "--save", "--json"])
        .arg(&root)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    root
}

/// `explain --json` of `path`, through the binary: its handle and
/// `freeable_now_bytes`.
fn explained(env: &TestEnv, path: &Path) -> (String, u64) {
    let out = env
        .cmd()
        .args(["explain", "--json"])
        .arg(path)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let value: Value = serde_json::from_slice(&out.stdout).unwrap();
    (
        value["result"]["handle"].as_str().unwrap().to_owned(),
        value["result"]["freeable_now_bytes"].as_u64().unwrap(),
    )
}

/// The arguments of one item.
fn item(handle: &str, expect_bytes: u64) -> Value {
    json!({"handle": handle, "expect_bytes": expect_bytes})
}

/// What a test expects a dry or refused run to leave: the three files where
/// they were and nothing in the Trash.
fn assert_untouched(env: &TestEnv, root: &Path) {
    for path in ["keep.bin", "one.bin", "dir/two.bin"] {
        assert!(root.join(path).exists(), "{path} moved");
    }
    assert!(
        !env.xdg_data_home().join("Trash/files").exists()
            || fs::read_dir(env.xdg_data_home().join("Trash/files"))
                .unwrap()
                .next()
                .is_none(),
        "something reached the Trash"
    );
}

#[test]
fn p5_mcp_01_the_tool_is_offered_only_under_allow_delete() {
    for (lifecycle, version) in revisions() {
        let env = TestEnv::new();
        let root = delete_tree(&env);
        let (handle, frees) = explained(&env, &root.join("one.bin"));
        // Without the flag: not listed, and a call is an unknown tool.
        let ((names, refused), _) = session(
            spawn(&env, &roots(&root, &[]), &[]),
            lifecycle.clone(),
            version.clone(),
            |client, _| async move {
                let names: Vec<String> = client
                    .list_all_tools()
                    .await
                    .unwrap()
                    .into_iter()
                    .map(|tool| tool.name.into_owned())
                    .collect();
                let refused = client
                    .send_cancellable_request(
                        tool_call("duscope_delete", &json!({"items": [item(&handle, frees)]})),
                        PeerRequestOptions::no_options(),
                    )
                    .await
                    .unwrap()
                    .await_response()
                    .await;
                ((names, refused), client)
            },
        );
        assert_eq!(names, SHIPPED, "{version}");
        let error = rpc_error(refused);
        assert_eq!(error.code, ErrorCode::INVALID_PARAMS, "{error:?}");
        assert!(error.message.contains("duscope_delete"), "{error:?}");
        assert_untouched(&env, &root);
        // With it: listed, sorted among the read-only tools, whose hints
        // do not change, and marked for the client's prompt.
        let (listed, _) = session(
            spawn(&env, &roots(&root, &["--allow-delete"]), &[]),
            lifecycle.clone(),
            version.clone(),
            |client, _| async move { (client.list_all_tools().await.unwrap(), client) },
        );
        let names: Vec<&str> = listed.iter().map(|tool| tool.name.as_ref()).collect();
        let mut expected: Vec<&str> = SHIPPED.to_vec();
        expected.push("duscope_delete");
        expected.sort_unstable();
        assert_eq!(names, expected, "{version}");
        for tool in &listed {
            if tool.name == "duscope_delete" {
                let hints = tool.annotations.as_ref().unwrap();
                assert_eq!(hints.read_only_hint, Some(false));
                assert_eq!(hints.destructive_hint, Some(true));
                assert_eq!(hints.idempotent_hint, Some(false));
                assert_eq!(hints.open_world_hint, Some(false));
                let meta = &tool.meta.as_ref().unwrap().0;
                assert_eq!(
                    meta.get("anthropic/requiresUserInteraction"),
                    Some(&json!(true))
                );
                let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../schemas/mcp/duscope_delete.input.json");
                assert_eq!(
                    Value::Object((*tool.input_schema).clone()),
                    serde_json::from_str::<Value>(&fs::read_to_string(fixture).unwrap()).unwrap()
                );
            } else {
                assert_tool(tool, 24_000);
            }
        }
    }
}

#[test]
fn p5_mcp_01_a_call_without_a_handle_is_refused() {
    for (lifecycle, version) in revisions() {
        let env = TestEnv::new();
        let root = delete_tree(&env);
        let (results, _) = session(
            spawn(&env, &roots(&root, &["--allow-delete"]), &[]),
            lifecycle,
            version,
            |client, _| async move {
                let (_, no_items) = call(&client, "duscope_delete", &json!({"items": []})).await;
                let (_, no_handle) = call(
                    &client,
                    "duscope_delete",
                    &json!({"items": [{"expect_bytes": 4096}]}),
                )
                .await;
                let (_, no_bytes) = call(
                    &client,
                    "duscope_delete",
                    &json!({"items": [{"handle": "aGFuZGxl"}]}),
                )
                .await;
                let (_, no_arguments) = call(&client, "duscope_delete", &json!({})).await;
                ([no_items, no_handle, no_bytes, no_arguments], client)
            },
        );
        for result in &results {
            assert_eq!(error_code(result), "usage", "{result:?}");
        }
        assert_untouched(&env, &root);
    }
}

#[test]
fn p5_mcp_01_an_expired_or_forged_handle_is_refused() {
    use duscope_core::handle::{Handle, HandlePayload, Key};
    let env = TestEnv::new();
    let root = delete_tree(&env);
    let (good, frees) = explained(&env, &root.join("one.bin"));
    let key = Key::open(&env.index_dir()).unwrap();
    let payload = Handle::verify(&key, &good, duscope_core::query::clock::now()).unwrap();
    let expired = Handle::issue(
        &key,
        &HandlePayload {
            expires_at: 1,
            ..payload
        },
    );
    // The last character of the MAC, whose low bits may be padding, spared.
    let mut forged = good.clone().into_bytes();
    let at = forged.len() - 2;
    forged[at] = if forged[at] == b'A' { b'B' } else { b'A' };
    let forged = String::from_utf8(forged).unwrap();
    for (lifecycle, version) in revisions() {
        let (expired, forged) = (expired.clone(), forged.clone());
        let ((late, wrong), _) = session(
            spawn(&env, &roots(&root, &["--allow-delete"]), &[]),
            lifecycle,
            version,
            |client, _| async move {
                let (_, late) = call(
                    &client,
                    "duscope_delete",
                    &json!({"items": [item(&expired, frees)], "dry_run": false}),
                )
                .await;
                let (_, wrong) = call(
                    &client,
                    "duscope_delete",
                    &json!({"items": [item(&forged, frees)], "dry_run": false}),
                )
                .await;
                ((late, wrong), client)
            },
        );
        assert_eq!(error_code(&late), "handle_expired", "{late:?}");
        assert_eq!(error_code(&wrong), "handle_invalid", "{wrong:?}");
    }
    assert_untouched(&env, &root);
}

#[test]
fn p5_mcp_01_a_path_outside_the_roots_is_refused() {
    let env = TestEnv::new();
    let base = env.work_dir().canonicalize().unwrap();
    let root = base.join("served");
    let inside = root.join("inside.bin");
    let outside = base.join("outside.bin");
    fs::create_dir_all(&root).unwrap();
    fs::write(&inside, vec![4u8; 1_000]).unwrap();
    fs::write(&outside, vec![5u8; 1_000]).unwrap();
    let scanned = env
        .cmd()
        .args(["scan", "--save", "--json"])
        .arg(&base)
        .output()
        .unwrap();
    assert!(scanned.status.success(), "{scanned:?}");
    // Handles the CLI issues with the same key: a path outside the server's
    // roots, and one of the roots itself.
    let (out_handle, _) = explained(&env, &outside);
    let (root_handle, _) = explained(&env, &root);
    for (lifecycle, version) in revisions() {
        let (out_handle, root_handle) = (out_handle.clone(), root_handle.clone());
        let ((out, top), _) = session(
            spawn(&env, &roots(&root, &["--allow-delete"]), &[]),
            lifecycle,
            version,
            |client, _| async move {
                let (_, out) = call(
                    &client,
                    "duscope_delete",
                    &json!({"items": [item(&out_handle, 1_000)], "dry_run": false}),
                )
                .await;
                let (_, top) = call(
                    &client,
                    "duscope_delete",
                    &json!({"items": [item(&root_handle, 1_000)], "dry_run": false}),
                )
                .await;
                ((out, top), client)
            },
        );
        assert_eq!(error_code(&out), "refused_path", "{out:?}");
        assert_eq!(error_code(&top), "refused_path", "{top:?}");
    }
    assert!(inside.exists() && outside.exists() && root.is_dir());
    assert!(
        !env.xdg_data_home().join("Trash/files").exists(),
        "something reached the Trash"
    );
}

#[test]
fn p5_mcp_01_dry_run_is_the_default_and_moves_nothing() {
    for (lifecycle, version) in revisions() {
        let env = TestEnv::new();
        let root = delete_tree(&env);
        let (handle, frees) = explained(&env, &root.join("one.bin"));
        let items = json!([item(&handle, frees)]);
        let args = |dry_run: Option<bool>| match dry_run {
            None => json!({ "items": items }),
            Some(flag) => json!({"items": items, "dry_run": flag}),
        };
        let (default_arguments, explicit_arguments) = (args(None), args(Some(true)));
        let (results, transcript) = session(
            spawn(&env, &roots(&root, &["--allow-delete"]), &[]),
            lifecycle,
            version,
            |client, _| async move {
                let (first, default) = call(&client, "duscope_delete", &default_arguments).await;
                let (second, explicit) = call(&client, "duscope_delete", &explicit_arguments).await;
                ((first, second, default, explicit), client)
            },
        );
        let (first, second, default, explicit) = results;
        // Byte for byte, the verb's own `--json`, whichever way the
        // default is reached.
        let printed = cli_json(
            &env,
            &[
                "clean".into(),
                format!("--from={handle}").into(),
                format!("--expect-item-bytes={frees}").into(),
                "--dry-run".into(),
            ],
        );
        for request in [&first, &second] {
            let (_, line) = transcript.response(request).unwrap();
            assert_eq!(structured_content(line), printed);
        }
        for result in [&default, &explicit] {
            let value = envelope(result);
            let plan = &value["result"]["plan"];
            assert_eq!(plan["items"][0]["action"], "trash", "{value}");
            assert_eq!(plan["items"][0]["outcome"], Value::Null, "{value}");
            assert_eq!(plan["confirmation_required"], true, "{value}");
            assert_eq!(value["result"]["trashed_count"], 0, "{value}");
            assert_eq!(value["next"], json!([]), "{value}");
            assert_eq!(value["query"]["yes"], false, "{value}");
            assert!(
                text(result).starts_with("Dry run: 1 item would"),
                "{result:?}"
            );
        }
        assert_untouched(&env, &root);
    }
}

#[test]
fn p5_mcp_01_an_expect_bytes_mismatch_refuses_the_item_and_moves_nothing() {
    for (lifecycle, version) in revisions() {
        let env = TestEnv::new();
        let root = delete_tree(&env);
        let (one, one_frees) = explained(&env, &root.join("one.bin"));
        let (two, two_frees) = explained(&env, &root.join("dir"));
        // The second off by far more than 1%, the first exact; a dry run
        // and a real one alike.
        let items = json!([item(&one, one_frees), item(&two, two_frees * 2 + 8192)]);
        let (arguments, real) = (
            json!({ "items": items }),
            json!({"items": items, "dry_run": false}),
        );
        let ((dry, wet), _) = session(
            spawn(&env, &roots(&root, &["--allow-delete"]), &[]),
            lifecycle,
            version,
            |client, _| async move {
                let (_, dry) = call(&client, "duscope_delete", &arguments).await;
                let (_, wet) = call(&client, "duscope_delete", &real).await;
                ((dry, wet), client)
            },
        );
        for result in [&dry, &wet] {
            assert_eq!(error_code(result), "expectation_mismatch", "{result:?}");
            let value = result.structured_content.as_ref().unwrap();
            let items = &value["result"]["plan"]["items"];
            assert_eq!(items[0]["action"], "trash", "{value}");
            assert_eq!(items[1]["action"], "refused", "{value}");
            assert_eq!(items[1]["reason"], "expectation_mismatch", "{value}");
            assert_eq!(value["result"]["trashed_count"], 0, "{value}");
            assert!(
                value["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains("not within 1%"),
                "{value}"
            );
        }
        assert_untouched(&env, &root);
    }
}

/// Has rmcp's client, speaking `lifecycle`, move a file and a directory of a
/// fresh tree to the test's own Trash, and checks what moved.
#[cfg(target_os = "linux")]
fn really_moves(lifecycle: ClientLifecycleMode, version: ProtocolVersion) {
    let env = TestEnv::new();
    let root = delete_tree(&env);
    let file = root.join("one.bin");
    let dir = root.join("dir");
    let (file_handle, file_frees) = explained(&env, &file);
    let (dir_handle, dir_frees) = explained(&env, &dir);
    let arguments = json!({
        "items": [item(&file_handle, file_frees), item(&dir_handle, dir_frees)],
        "dry_run": false,
    });
    let (result, _) = session(
        spawn(&env, &roots(&root, &["--allow-delete"]), &[]),
        lifecycle,
        version,
        |client, _| async move {
            let (_, result) = call(&client, "duscope_delete", &arguments).await;
            (result, client)
        },
    );
    let value = envelope(&result);
    assert_eq!(value["query"]["permanent"], false, "{value}");
    assert_eq!(value["result"]["plan"]["method"], "trash", "{value}");
    assert_eq!(value["result"]["trashed_count"], 2, "{value}");
    assert_eq!(value["result"]["freed_bytes"], 0, "{value}");
    assert_eq!(value["result"]["deleted_count"], 0, "{value}");
    let moved = text(&result);
    assert!(moved.starts_with("Moved 2 items to the Trash"), "{moved}");
    assert_eq!(value["warnings"][0]["code"], "index_untouched", "{value}");
    // Only the two named items moved, each to the test's own Trash.
    assert!(root.join("keep.bin").exists());
    assert!(!file.exists() && !dir.exists());
    let trash = env.xdg_data_home().join("Trash/files");
    assert_eq!(fs::read(trash.join("one.bin")).unwrap(), vec![2u8; 4_000]);
    assert_eq!(
        fs::read(trash.join("dir/two.bin")).unwrap(),
        vec![3u8; 5_000]
    );
    assert_eq!(
        fs::read_dir(&trash).unwrap().count(),
        2,
        "only the two named"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn p5_mcp_01_linux_a_real_run_moves_only_the_named_items_to_the_trash() {
    for (lifecycle, version) in revisions() {
        really_moves(lifecycle, version);
    }
}

// macOS: an item on the startup volume would reach the account's own
// Trash, so the test environment refuses it (`DUSCOPE_TEST_TRASH`), and a
// real run of the tool shows that refusal; a real move runs on a disk image,
// as `clean`'s platform test does.

#[cfg(target_os = "macos")]
#[test]
fn p5_mcp_01_macos_the_test_environment_moves_nothing_on_the_startup_volume() {
    for (lifecycle, version) in revisions() {
        let env = TestEnv::new();
        let root = delete_tree(&env);
        let (handle, frees) = explained(&env, &root.join("one.bin"));
        let (result, _) = session(
            spawn(&env, &roots(&root, &["--allow-delete"]), &[]),
            lifecycle,
            version,
            |client, _| async move {
                let (_, result) = call(
                    &client,
                    "duscope_delete",
                    &json!({"items": [item(&handle, frees)], "dry_run": false}),
                )
                .await;
                (result, client)
            },
        );
        assert_eq!(error_code(&result), "trash_unavailable", "{result:?}");
        let value = result.structured_content.as_ref().unwrap();
        assert_eq!(
            value["result"]["plan"]["items"][0]["outcome"], "failed",
            "{value}"
        );
        assert_eq!(value["result"]["trashed_count"], 0, "{value}");
        assert_untouched(&env, &root);
    }
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "attaches a disk image; DUSCOPE_TEST_PLATFORM=1"]
fn p5_mcp_01_macos_a_real_run_on_an_image_moves_only_the_named_items_to_the_trash() {
    let env = TestEnv::new();
    if std::env::var_os("DUSCOPE_TEST_PLATFORM").is_none_or(|v| v != "1") {
        eprintln!("skipped: set DUSCOPE_TEST_PLATFORM=1");
        return;
    }
    let work = env.tmp_dir().canonicalize().unwrap().join("image");
    let mount = work.join("mnt");
    fs::create_dir_all(&mount).unwrap();
    let image = work.join("image.dmg");
    env.hdiutil(&[
        &"create",
        &"-ov",
        &"-size",
        &"16m",
        &"-fs",
        &"APFS",
        &"-volname",
        &"DUSCOPEMCP",
        &image,
    ]);
    env.hdiutil(&[&"attach", &"-nobrowse", &"-mountpoint", &mount, &image]);
    let detach = Detach(&env, mount.clone());
    let (file, dir) = (mount.join("a.bin"), mount.join("dir"));
    let keep = mount.join("keep.bin");
    fs::create_dir_all(&dir).unwrap();
    fs::write(&file, vec![7u8; 3_000]).unwrap();
    fs::write(&keep, vec![1u8; 3_000]).unwrap();
    fs::write(dir.join("b.bin"), vec![9u8; 5_000]).unwrap();
    let scanned = env
        .cmd()
        .args(["scan", "--save", "--json"])
        .arg(&mount)
        .output()
        .unwrap();
    assert!(scanned.status.success(), "{scanned:?}");
    let (file_handle, file_frees) = explained(&env, &file);
    let (dir_handle, dir_frees) = explained(&env, &dir);
    let arguments = json!({
        "items": [item(&file_handle, file_frees), item(&dir_handle, dir_frees)],
        "dry_run": false,
    });
    let (result, _) = session(
        spawn(&env, &roots(&mount, &["--allow-delete"]), &[]),
        discover(),
        ProtocolVersion::LATEST,
        |client, _| async move {
            let (_, result) = call(&client, "duscope_delete", &arguments).await;
            (result, client)
        },
    );
    let value = envelope(&result);
    assert_eq!(value["result"]["trashed_count"], 2, "{value}");
    assert!(!file.exists() && !dir.exists() && keep.exists());
    // The volume's own Trash, this user's directory in it.
    let uid = String::from_utf8(
        std::process::Command::new("id")
            .arg("-u")
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let trash = mount.join(".Trashes").join(uid.trim());
    assert_eq!(fs::read(trash.join("a.bin")).unwrap(), vec![7u8; 3_000]);
    assert_eq!(fs::read(trash.join("dir/b.bin")).unwrap(), vec![9u8; 5_000]);
    drop(detach);
}

/// Detaches the disk image mounted at its path when dropped.
#[cfg(target_os = "macos")]
struct Detach<'a>(&'a TestEnv, PathBuf);

#[cfg(target_os = "macos")]
impl Drop for Detach<'_> {
    fn drop(&mut self) {
        let _ = self
            .0
            .command("hdiutil")
            .args(["detach", "-force"])
            .arg(&self.1)
            .output();
    }
}
