//! P3-MCP-05: the worker through the server and the stdio transport, over
//! OS pipes, with fake verbs the tests hold and release: the FIFO queue
//! while the reader answers, cancellation of a queued and of a running call,
//! the deadline, and progress. rmcp's client (the conformance oracle)
//! observes a call's progress, and cancels a running call once it has seen
//! its progress. The binary's tests (`duscope/src/verbs/mcp/tests.rs`) cancel
//! a real walk this way, since duscope-walk lies outside this crate's
//! dependencies.

use std::future::Future;
use std::io::{self, BufRead, BufReader, Write};
use std::os::fd::OwnedFd;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use duscope_testkit::TestEnv;
use rmcp::model::{
    CallToolRequest, CallToolRequestParams, ClientCapabilities, ClientConfig, ClientRequest,
    Implementation, ProgressNotificationParam, ProtocolVersion, ServerResult,
};
use rmcp::service::{
    ClientLifecycleMode, ClientServiceExt, MaybeSendFuture, NotificationContext, PeerRequestOptions,
};
use rmcp::{ClientHandler, RoleClient};
use serde_json::{Map, Value, json};
use tokio::net::unix::pipe;

use super::*;
use crate::discover::PLACEHOLDER_INSTRUCTIONS;
use crate::roots::Roots;
use crate::server::Server;
use crate::tools::{Annotations, Invocation, Outcome, Registry, Runner, Tool};
use crate::transport::Transport;

/// How long a test waits for a message or a state before it fails.
const PATIENCE: Duration = Duration::from_secs(10);

/// A fake tool: `name` runs the fake verb `verb`, with the call's `label`.
struct TestTool {
    name: &'static str,
    verb: &'static str,
}

impl Tool for TestTool {
    fn name(&self) -> &'static str {
        self.name
    }

    fn description(&self) -> &'static str {
        "A fake verb (a test fixture)."
    }

    fn input_schema(&self) -> &'static str {
        r#"{"type":"object","properties":{"label":{"type":"string"}},"additionalProperties":false}"#
    }

    fn annotations(&self) -> Annotations {
        Annotations {
            read_only: true,
            destructive: false,
            idempotent: true,
        }
    }

    fn invocation(
        &self,
        arguments: &Map<String, Value>,
        _: &Context<'_>,
    ) -> Result<Invocation, Refusal> {
        let label = arguments.get("label").and_then(Value::as_str).unwrap_or("");
        Ok(Invocation {
            verb: self.verb,
            args: vec![label.into()],
        })
    }

    fn summary(&self, envelope: &Value) -> String {
        format!("ran {}", envelope["label"])
    }
}

/// The fake tools:
/// - `test_echo` returns at once;
/// - `test_block` counts one entry, then waits until the test opens the
///   latch, or its call is told to stop;
/// - `test_stubborn` waits for the latch alone, as a verb with no point at
///   which to stop;
/// - `test_count` counts an entry every 5 ms, 30 of them.
fn tools() -> Vec<TestTool> {
    [
        ("test_echo", "echo"),
        ("test_block", "block"),
        ("test_stubborn", "stubborn"),
        ("test_count", "count"),
    ]
    .into_iter()
    .map(|(name, verb)| TestTool { name, verb })
    .collect()
}

/// A latch the test opens; until then, the verbs that wait on it wait.
#[derive(Default)]
struct Latch {
    open: Mutex<bool>,
    opened: Condvar,
}

impl Latch {
    fn open(&self) {
        *self.open.lock().unwrap() = true;
        self.opened.notify_all();
    }

    /// Waits until the latch is open (`true`), or `stop` returns true
    /// (`false`), for `PATIENCE` at most.
    fn wait(&self, stop: impl Fn() -> bool) -> bool {
        let started = Instant::now();
        let mut open = self.open.lock().unwrap();
        while !*open && !stop() && started.elapsed() < PATIENCE {
            open = self
                .opened
                .wait_timeout(open, Duration::from_millis(1))
                .unwrap()
                .0;
        }
        *open
    }
}

/// The fake verbs, and what the test reads back from them.
#[derive(Clone)]
struct TestRunner {
    /// What the verbs did, in order: `start LABEL`, `end LABEL`,
    /// `stopped LABEL`.
    log: Arc<Mutex<Vec<String>>>,
    latch: Arc<Latch>,
}

impl TestRunner {
    fn new() -> TestRunner {
        TestRunner {
            log: Arc::default(),
            latch: Arc::default(),
        }
    }

    fn note(&self, line: String) {
        self.log.lock().unwrap().push(line);
    }

    fn log(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }

    /// Waits until the log holds `line`.
    fn wait_for(&self, line: &str) {
        let started = Instant::now();
        while !self.log().iter().any(|l| l == line) {
            assert!(
                started.elapsed() < PATIENCE,
                "never logged {line}: {:?}",
                self.log()
            );
            thread::sleep(Duration::from_millis(1));
        }
    }
}

impl Runner for TestRunner {
    fn run(&self, invocation: &Invocation) -> Outcome {
        self.run_call(invocation, &CallSignals::default())
    }

    fn run_call(&self, invocation: &Invocation, call: &CallSignals) -> Outcome {
        let label = invocation.args[0].to_string_lossy().into_owned();
        self.note(format!("start {label}"));
        let stopped = call.cancel.load(Ordering::Relaxed);
        let ended = match invocation.verb {
            "block" => {
                call.entries.fetch_add(1, Ordering::Relaxed);
                self.latch.wait(|| call.cancel.load(Ordering::Relaxed))
            }
            "stubborn" => self.latch.wait(|| false),
            "count" => {
                for _ in 0..30 {
                    thread::sleep(Duration::from_millis(5));
                    call.entries.fetch_add(1, Ordering::Relaxed);
                }
                true
            }
            _ => !stopped,
        };
        self.note(format!("{} {label}", if ended { "end" } else { "stopped" }));
        Outcome {
            json: json!({"ok": ended, "label": label}).to_string(),
            ok: ended,
        }
    }

    fn refuse(&self, refusal: &Refusal) -> Outcome {
        let error = json!({"code": refusal.code, "message": refusal.message, "hint": refusal.hint});
        Outcome {
            json: json!({"ok": false, "error": error}).to_string(),
            ok: false,
        }
    }
}

/// A server offering the fake tools, run by `runner`, timed by `timing`.
fn server(env: &TestEnv, runner: &TestRunner, timing: Timing) -> Server {
    let mut registry = Registry::new(24_000, false);
    for tool in tools() {
        registry.register(Box::new(tool)).unwrap();
    }
    Server::new(ServerConfig::new(PLACEHOLDER_INSTRUCTIONS))
        .with_tools(Tools {
            registry,
            roots: Roots::new([env.work_dir()]).unwrap(),
            index_dir: env.index_dir(),
            runner: Box::new(runner.clone()),
        })
        .with_timing(timing)
}

/// Fast enough to see several reports in a test.
fn quick() -> Timing {
    Timing {
        progress_every: Duration::from_millis(20),
        deadline: None,
    }
}

/// A client on the other ends of the server's pipes, writing lines and
/// reading every message the server writes.
struct Client {
    input: Option<io::PipeWriter>,
    messages: mpsc::Receiver<Value>,
    transport: Option<Transport>,
}

impl Client {
    fn start(server: Server) -> Client {
        let (server_in, client_out) = io::pipe().unwrap();
        let (client_in, server_out) = io::pipe().unwrap();
        let transport = Transport::start(server_in, server_out, server).unwrap();
        let (sent, messages) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(client_in).lines() {
                let Ok(line) = line else { break };
                if sent.send(serde_json::from_str(&line).unwrap()).is_err() {
                    break;
                }
            }
        });
        Client {
            input: Some(client_out),
            messages,
            transport: Some(transport),
        }
    }

    fn send(&mut self, message: &Value) {
        writeln!(self.input.as_mut().unwrap(), "{message}").unwrap();
    }

    /// The next message the server writes.
    fn next(&self) -> Value {
        self.messages
            .recv_timeout(PATIENCE)
            .expect("the server wrote nothing")
    }

    /// The ids of the next `n` responses, in the order they came.
    fn ids(&self, n: usize) -> Vec<Value> {
        (0..n).map(|_| self.next()["id"].clone()).collect()
    }

    /// Closes the input, waits for the server to end, and returns what else
    /// it wrote.
    fn close(mut self) -> Vec<Value> {
        drop(self.input.take());
        self.transport.take().unwrap().wait().unwrap();
        self.messages.iter().collect()
    }
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

fn call(id: u64, tool: &str, label: &str) -> Value {
    request(
        id,
        "tools/call",
        &json!({"name": tool, "arguments": {"label": label}}),
    )
}

/// A call asking for progress under `token`.
fn call_with_progress(id: u64, tool: &str, token: &str) -> Value {
    let mut message = call(id, tool, token);
    message["params"]["_meta"]["progressToken"] = json!(token);
    message
}

fn cancel(id: u64) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "notifications/cancelled",
        "params": {"requestId": id, "reason": "the test cancelled it"},
    })
}

#[test]
fn p3_mcp_05_a_queued_call_runs_after_the_first() {
    let env = TestEnv::new();
    let runner = TestRunner::new();
    let mut client = Client::start(server(&env, &runner, quick()));
    client.send(&call(1, "test_block", "first"));
    client.send(&call(2, "test_echo", "second"));
    // The reader answers these while the first call runs and the second
    // waits.
    client.send(&request(3, "server/discover", &json!({})));
    client.send(&request(4, "tools/list", &json!({})));
    client.send(&request(5, "ping", &json!({})));
    assert_eq!(client.ids(3), [3, 4, 5]);
    runner.wait_for("start first");
    assert_eq!(runner.log(), ["start first"], "the second call waits");
    runner.latch.open();
    assert_eq!(client.ids(2), [1, 2]);
    assert_eq!(
        runner.log(),
        ["start first", "end first", "start second", "end second"]
    );
    assert!(client.close().is_empty());
}

#[test]
fn p3_mcp_05_answers_wait_for_the_calls_before_them() {
    let env = TestEnv::new();
    let runner = TestRunner::new();
    let mut client = Client::start(server(&env, &runner, quick()));
    client.send(&call(1, "test_block", "first"));
    // Neither ping, discovery nor tools/list: answered after the call.
    client.send(&request(2, "resources/list", &json!({})));
    client.send(&json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {}}));
    client.send(&request(4, "ping", &json!({})));
    assert_eq!(client.ids(1), [4]);
    runner.latch.open();
    let rest: Vec<Value> = (0..3).map(|_| client.next()).collect();
    assert_eq!(rest[0]["id"], 1);
    assert_eq!(rest[1]["id"], 2);
    assert_eq!(rest[1]["error"]["code"], -32601);
    assert_eq!(rest[2]["id"], 3);
    assert_eq!(rest[2]["error"]["code"], -32602, "no _meta: {}", rest[2]);
    assert!(client.close().is_empty());
}

#[test]
fn p3_mcp_05_a_cancelled_queued_call_never_runs_and_gets_no_response() {
    let env = TestEnv::new();
    let runner = TestRunner::new();
    let mut client = Client::start(server(&env, &runner, quick()));
    client.send(&call(1, "test_block", "first"));
    client.send(&call(2, "test_echo", "second"));
    client.send(&cancel(2));
    client.send(&call(3, "test_echo", "third"));
    client.send(&request(4, "ping", &json!({})));
    assert_eq!(client.ids(1), [4]);
    runner.latch.open();
    assert_eq!(client.ids(2), [1, 3]);
    assert!(client.close().is_empty(), "request 2 got an answer");
    assert_eq!(
        runner.log(),
        ["start first", "end first", "start third", "end third"]
    );
}

#[test]
fn p3_mcp_05_a_cancelled_running_call_is_told_to_stop_and_gets_no_response() {
    let env = TestEnv::new();
    let runner = TestRunner::new();
    let mut client = Client::start(server(&env, &runner, quick()));
    client.send(&call(1, "test_block", "first"));
    runner.wait_for("start first");
    client.send(&cancel(1));
    client.send(&call(2, "test_echo", "second"));
    // The latch never opens: the first call's verb stops on its flag.
    assert_eq!(client.ids(1), [2]);
    assert_eq!(
        runner.log(),
        ["start first", "stopped first", "start second", "end second"]
    );
    // A cancellation of a call already answered, or of none, is ignored.
    client.send(&cancel(2));
    client.send(&cancel(99));
    client.send(&request(3, "ping", &json!({})));
    assert_eq!(client.ids(1), [3]);
    assert!(client.close().is_empty(), "request 1 got an answer");
}

#[test]
fn p3_mcp_05_a_call_past_its_deadline_is_answered_deadline_exceeded() {
    let env = TestEnv::new();
    let runner = TestRunner::new();
    let timing = Timing {
        deadline: Some(Duration::from_millis(100)),
        ..quick()
    };
    let mut client = Client::start(server(&env, &runner, timing));
    let sent = Instant::now();
    client.send(&call(1, "test_block", "first"));
    let response = client.next();
    let waited = sent.elapsed();
    assert_eq!(response["id"], 1);
    let result = &response["result"];
    assert_eq!(result["isError"], true, "{response}");
    assert_eq!(result["resultType"], "complete");
    let error = &result["structuredContent"]["error"];
    assert_eq!(error["code"], "deadline_exceeded");
    assert_eq!(
        error["message"],
        "test_block ran past its deadline of 100 ms"
    );
    assert_eq!(
        result["content"][0]["text"],
        "test_block ran past its deadline of 100 ms. Call test_block again on a smaller \
         directory, or run duscope block in a terminal, where no deadline applies"
    );
    assert!(waited >= Duration::from_millis(100), "{waited:?}");
    // The verb was told to stop, and the worker goes on.
    runner.wait_for("stopped first");
    client.send(&call(2, "test_echo", "second"));
    assert_eq!(client.next()["result"]["isError"], false);
    assert!(client.close().is_empty());
}

#[test]
fn p3_mcp_05_the_deadline_answers_a_verb_that_cannot_stop_and_the_next_call_waits_for_it() {
    let env = TestEnv::new();
    let runner = TestRunner::new();
    let timing = Timing {
        deadline: Some(Duration::from_millis(50)),
        ..quick()
    };
    let mut client = Client::start(server(&env, &runner, timing));
    client.send(&call(1, "test_stubborn", "first"));
    client.send(&call(2, "test_echo", "second"));
    let first = client.next();
    assert_eq!(first["id"], 1);
    assert_eq!(
        first["result"]["structuredContent"]["error"]["code"],
        "deadline_exceeded"
    );
    // Answered, but its verb still runs: one verb at a time.
    thread::sleep(Duration::from_millis(100));
    assert_eq!(runner.log(), ["start first"]);
    runner.latch.open();
    // The second call starts with its own deadline, and is answered.
    let second = client.next();
    assert_eq!(second["id"], 2);
    assert_eq!(second["result"]["isError"], false, "{second}");
    assert_eq!(
        runner.log(),
        ["start first", "end first", "start second", "end second"]
    );
    assert!(client.close().is_empty());
}

#[test]
fn p3_mcp_05_each_tool_has_its_deadline() {
    for tool in crate::tools::shipped() {
        let expected = if tool.name() == "duscope_scan" {
            Duration::from_secs(600)
        } else {
            Duration::from_secs(30)
        };
        assert_eq!(tool.deadline(), expected, "{}", tool.name());
    }
    assert_eq!(spoken(SCAN_DEADLINE), "10 minutes");
    assert_eq!(spoken(DEADLINE), "30 seconds");
    assert_eq!(spoken(Duration::from_secs(1)), "1 second");
    assert_eq!(spoken(Duration::from_millis(1500)), "1500 ms");
}

#[test]
fn p3_mcp_05_progress_goes_out_before_the_response_and_only_when_asked_for() {
    let env = TestEnv::new();
    let runner = TestRunner::new();
    let mut client = Client::start(server(&env, &runner, quick()));
    client.send(&call_with_progress(1, "test_count", "counting"));
    let mut messages = Vec::new();
    loop {
        let message = client.next();
        let answered = message.get("id").is_some();
        messages.push(message);
        if answered {
            break;
        }
    }
    let (response, notifications) = messages.split_last().unwrap();
    assert_eq!(response["id"], 1);
    assert!(notifications.len() >= 2, "{messages:#?}");
    let mut last = 0;
    for notification in notifications {
        assert_eq!(notification["method"], "notifications/progress");
        let params = &notification["params"];
        assert_eq!(params["progressToken"], "counting");
        let progress = params["progress"].as_u64().unwrap();
        assert!(progress > last && progress <= 30, "{notification}");
        assert_eq!(params["message"], format!("{progress} entries counted"));
        last = progress;
    }
    // Without a token, no progress.
    client.send(&call(2, "test_count", "quiet"));
    assert_eq!(client.next()["id"], 2);
    // Nothing follows a response about its request.
    assert!(client.close().is_empty());
}

/// rmcp's client, recording every progress notification it receives.
struct Observer {
    config: ClientConfig,
    progress: tokio::sync::mpsc::UnboundedSender<ProgressNotificationParam>,
}

impl ClientHandler for Observer {
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

/// Starts `server` on fresh pipes, runs `session` with rmcp's client on the
/// other ends, speaking 2026-07-28, and its progress notifications; then
/// closes the client and waits for the server to end.
fn rmcp_session<F, Fut>(server: Server, session: F)
where
    F: FnOnce(
        rmcp::service::RunningService<RoleClient, Observer>,
        tokio::sync::mpsc::UnboundedReceiver<ProgressNotificationParam>,
    ) -> Fut,
    Fut: Future<Output = rmcp::service::RunningService<RoleClient, Observer>>,
{
    let (server_in, client_out) = io::pipe().unwrap();
    let (client_in, server_out) = io::pipe().unwrap();
    let running_server = Transport::start(server_in, server_out, server).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async move {
        let reader = pipe::Receiver::from_owned_fd(OwnedFd::from(client_in)).unwrap();
        let writer = pipe::Sender::from_owned_fd(OwnedFd::from(client_out)).unwrap();
        let (progress, received) = tokio::sync::mpsc::unbounded_channel();
        let handler = Observer {
            config: ClientConfig::new(
                ClientCapabilities::default(),
                Implementation::new("duscope-test", "0"),
            )
            .with_protocol_version(ProtocolVersion::LATEST),
            progress,
        };
        let client = handler
            .serve_with_lifecycle(
                (reader, writer),
                ClientLifecycleMode::Discover {
                    preferred_versions: vec![ProtocolVersion::V_2026_07_28],
                },
            )
            .await
            .unwrap();
        let client = session(client, received).await;
        client.cancel().await.unwrap();
    });
    drop(runtime);
    running_server.wait().unwrap();
}

fn tool_call(name: &'static str) -> ClientRequest {
    ClientRequest::CallToolRequest(CallToolRequest::new(CallToolRequestParams::new(name)))
}

#[test]
fn p3_mcp_05_rmcp_observes_the_progress_of_a_call() {
    let env = TestEnv::new();
    let runner = TestRunner::new();
    rmcp_session(
        server(&env, &runner, quick()),
        |client, mut observed| async move {
            let handle = client
                .send_cancellable_request(tool_call("test_count"), PeerRequestOptions::no_options())
                .await
                .unwrap();
            let token = handle.progress_token.clone();
            let response = handle.await_response().await.unwrap();
            let ServerResult::CallToolResult(result) = response else {
                panic!("not a tool result: {response:?}");
            };
            assert_eq!(result.is_error, Some(false));
            let mut seen = Vec::new();
            while let Ok(progress) = observed.try_recv() {
                seen.push(progress);
            }
            assert!(seen.len() >= 2, "{seen:?}");
            let mut last = 0.0;
            for progress in &seen {
                assert_eq!(progress.progress_token, token);
                assert!(progress.progress > last, "{seen:?}");
                assert!(progress.progress <= 30.0, "{seen:?}");
                assert_eq!(progress.total, None);
                last = progress.progress;
            }
            // Nothing more about a request once it is answered.
            tokio::time::sleep(Duration::from_millis(100)).await;
            assert!(observed.try_recv().is_err());
            client
        },
    );
}

#[test]
fn p3_mcp_05_rmcp_cancels_a_running_call_once_it_has_seen_its_progress() {
    let env = TestEnv::new();
    let runner = TestRunner::new();
    let log = runner.clone();
    rmcp_session(
        server(&env, &runner, quick()),
        |client, mut observed| async move {
            let handle = client
                .send_cancellable_request(tool_call("test_block"), PeerRequestOptions::no_options())
                .await
                .unwrap();
            let token = handle.progress_token.clone();
            let progress = tokio::time::timeout(PATIENCE, observed.recv())
                .await
                .expect("no progress within the patience")
                .unwrap();
            assert_eq!(progress.progress_token, token);
            assert!(
                (progress.progress - 1.0).abs() < f64::EPSILON,
                "{progress:?}"
            );
            handle
                .cancel(Some("the test cancelled the call".to_owned()))
                .await
                .unwrap();
            // The latch never opens: the next call runs once the first
            // call's verb has stopped on its flag.
            let echoed = client
                .call_tool(CallToolRequestParams::new("test_echo"))
                .await
                .unwrap();
            assert_eq!(echoed.is_error, Some(false));
            assert_eq!(log.log(), ["start ", "stopped ", "start ", "end "]);
            client
        },
    );
}
