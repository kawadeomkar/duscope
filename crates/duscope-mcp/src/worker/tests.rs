//! P3-MCP-05: the worker through the server and the stdio transport, over
//! OS pipes, with fake verbs the tests hold and release: the FIFO queue
//! while the reader answers, cancellation of a queued and of a running call,
//! the deadline, progress, and a client whose output has gone. rmcp's
//! client (the conformance oracle) observes a call's progress, and cancels a
//! running call once it has seen its progress. The binary's tests
//! (`duscope/src/verbs/mcp/tests.rs`) cancel a real walk this way, since
//! duscope-walk lies outside this crate's dependencies.
//!
//! P3-MCP-08: what the panic hook answers before the process ends, on a
//! worker started here and an output the test reads, and a panicked verb
//! under unwinding, which the server survives.

use std::future::Future;
use std::io::{self, BufRead, BufReader, Cursor, Read, Write};
use std::os::fd::OwnedFd;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Condvar, Mutex, OnceLock, mpsc};
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
use crate::jsonrpc::Notification;
use crate::roots::Roots;
use crate::server::Server;
use crate::tools::{Annotations, Invocation, Outcome, Registry, Runner, Tool};
use crate::transport::{self, Handler, Outbox, Reader, Transport};

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
/// - `test_count` counts an entry every 5 ms, 30 of them;
/// - `test_commit` counts one entry and claims its call's point of no
///   return at once, then waits for the latch alone, as a verb that has
///   begun to move things; it ends `stopped` if its call was told to stop
///   all the same;
/// - `test_commit_late` counts one entry and waits for the latch alone, as
///   a plan is made, then claims the point of no return, and acts on
///   nothing (`stopped`) when its call was stopped first;
/// - `test_panic` panics;
/// - `test_commit_panic` claims its call's point of no return, then panics.
fn tools() -> Vec<TestTool> {
    [
        ("test_echo", "echo"),
        ("test_block", "block"),
        ("test_stubborn", "stubborn"),
        ("test_count", "count"),
        ("test_commit", "commit"),
        ("test_commit_late", "commit_late"),
        ("test_panic", "panic"),
        ("test_commit_panic", "commit_panic"),
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

    /// Waits until the latch is open, or `stop` returns true, for
    /// `PATIENCE` at most.
    fn wait(&self, stop: impl Fn() -> bool) -> Waited {
        let started = Instant::now();
        let mut open = self.open.lock().unwrap();
        loop {
            if *open {
                return Waited::Opened;
            }
            if stop() {
                return Waited::Stopped;
            }
            if started.elapsed() >= PATIENCE {
                return Waited::OutOfPatience;
            }
            open = self
                .opened
                .wait_timeout(open, Duration::from_millis(1))
                .unwrap()
                .0;
        }
    }
}

/// How a fake verb's wait ended, which it logs: `end`, `stopped` or
/// `timed out`. A verb told to stop logs `stopped` only when it saw its
/// flag, so a test that waits for `stopped` fails if the flag was never set,
/// rather than passing once the wait has run out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Waited {
    /// The test opened the latch, or the verb does not wait.
    Opened,
    /// The call's flag was set.
    Stopped,
    /// Neither, within `PATIENCE`.
    OutOfPatience,
}

impl Waited {
    fn word(self) -> &'static str {
        match self {
            Waited::Opened => "end",
            Waited::Stopped => "stopped",
            Waited::OutOfPatience => "timed out",
        }
    }
}

/// The fake verbs, and what the test reads back from them.
#[derive(Clone)]
struct TestRunner {
    /// What the verbs did, in order: `start LABEL`, then `end LABEL`,
    /// `stopped LABEL` or `timed out LABEL` ([`Waited`]); `committed LABEL`
    /// once a verb has claimed its call's point of no return.
    log: Arc<Mutex<Vec<String>>>,
    latch: Arc<Latch>,
    /// The signals of each call a verb ran for, in order.
    calls: Arc<Mutex<Vec<CallSignals>>>,
}

impl TestRunner {
    fn new() -> TestRunner {
        TestRunner {
            log: Arc::default(),
            latch: Arc::default(),
            calls: Arc::default(),
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
        self.calls.lock().unwrap().push(call.clone());
        self.note(format!("start {label}"));
        let stopped = call.cancel.load(Ordering::Relaxed);
        let waited = match invocation.verb {
            "block" => {
                call.entries.fetch_add(1, Ordering::Relaxed);
                self.latch.wait(|| call.cancel.load(Ordering::Relaxed))
            }
            "stubborn" => self.latch.wait(|| false),
            "commit" => {
                call.entries.fetch_add(1, Ordering::Relaxed);
                if call.commit() {
                    self.note(format!("committed {label}"));
                    match self.latch.wait(|| false) {
                        // A committed call is never told to stop.
                        Waited::Opened if call.cancel.load(Ordering::Relaxed) => Waited::Stopped,
                        other => other,
                    }
                } else {
                    Waited::Stopped
                }
            }
            "commit_late" => {
                call.entries.fetch_add(1, Ordering::Relaxed);
                match self.latch.wait(|| false) {
                    Waited::Opened if call.commit() => {
                        self.note(format!("committed {label}"));
                        Waited::Opened
                    }
                    Waited::Opened => Waited::Stopped,
                    other => other,
                }
            }
            "panic" => panic!("the verb {label} panicked"),
            "commit_panic" => {
                assert!(call.commit());
                self.note(format!("committed {label}"));
                panic!("the verb {label} panicked past its point of no return");
            }
            "count" => {
                for _ in 0..30 {
                    thread::sleep(Duration::from_millis(5));
                    call.entries.fetch_add(1, Ordering::Relaxed);
                }
                Waited::Opened
            }
            _ if stopped => Waited::Stopped,
            _ => Waited::Opened,
        };
        self.note(format!("{} {label}", waited.word()));
        let ended = waited == Waited::Opened;
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

/// The fake tools, run by `runner`.
fn fake_tools(env: &TestEnv, runner: &TestRunner) -> Tools {
    let mut registry = Registry::new(24_000, false);
    for tool in tools() {
        registry.register(Box::new(tool)).unwrap();
    }
    Tools {
        registry,
        roots: Roots::new([env.work_dir()]).unwrap(),
        index_dir: env.index_dir(),
        runner: Box::new(runner.clone()),
    }
}

/// A server offering the fake tools, run by `runner`, timed by `timing`.
fn server(env: &TestEnv, runner: &TestRunner, timing: Timing) -> Server {
    Server::new(ServerConfig::new(PLACEHOLDER_INSTRUCTIONS))
        .with_tools(fake_tools(env, runner))
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

/// The output of a client that has gone: every write fails, as one to a
/// pipe whose reader has closed does.
struct Gone;

impl Write for Gone {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::ErrorKind::BrokenPipe.into())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Starts `server` writing to a client that has gone: the input it reads,
/// and the running transport.
fn start_gone(server: Server) -> (io::PipeWriter, Transport) {
    let (server_in, client_out) = io::pipe().unwrap();
    let transport = Transport::start(server_in, Gone, server).unwrap();
    (client_out, transport)
}

fn send(input: &mut io::PipeWriter, message: &Value) {
    writeln!(input, "{message}").unwrap();
}

#[test]
fn p3_mcp_05_once_a_write_fails_the_queued_calls_never_start() {
    let env = TestEnv::new();
    let runner = TestRunner::new();
    let (mut input, transport) = start_gone(server(&env, &runner, quick()));
    send(&mut input, &call(1, "test_block", "first"));
    send(&mut input, &call(2, "test_echo", "second"));
    send(&mut input, &call(3, "test_echo", "third"));
    runner.wait_for("start first");
    // The client died: its input ends with two calls queued, and the first
    // call's answer cannot reach it.
    drop(input);
    runner.latch.open();
    let ended = transport.wait().unwrap_err();
    assert_eq!(ended.kind(), io::ErrorKind::BrokenPipe);
    assert_eq!(
        runner.log(),
        ["start first", "end first"],
        "a queued call ran"
    );
}

#[test]
fn p3_mcp_05_a_call_whose_progress_cannot_be_sent_is_cancelled_and_the_queue_dropped() {
    let env = TestEnv::new();
    let runner = TestRunner::new();
    let (mut input, transport) = start_gone(server(&env, &runner, quick()));
    send(&mut input, &call_with_progress(1, "test_block", "first"));
    send(&mut input, &call(2, "test_echo", "second"));
    // Its first progress cannot be sent, so the call is told to stop: the
    // latch never opens.
    runner.wait_for("stopped first");
    drop(input);
    let ended = transport.wait().unwrap_err();
    assert_eq!(ended.kind(), io::ErrorKind::BrokenPipe);
    assert_eq!(
        runner.log(),
        ["start first", "stopped first"],
        "the queued call ran"
    );
}

/// A handler that keeps each request's responder for the input to answer
/// through, and how the input ended.
struct Keeping {
    responders: mpsc::Sender<Responder>,
    ended: Option<Result<(), io::ErrorKind>>,
}

impl Handler for Keeping {
    fn request(&mut self, _: Request, responder: Responder) {
        self.responders.send(responder).unwrap();
    }

    fn notification(&mut self, _: Notification) {}

    fn end(&mut self, ended: &io::Result<()>) {
        self.ended = Some(ended.as_ref().copied().map_err(io::Error::kind));
    }
}

/// An input of one request that ends just as the request's answer, sent
/// from elsewhere while the reader waits for more, fails to go out: the
/// reader read the request and found every write sound, and next meets the
/// end of its input.
struct EndsAfterTheAnswer {
    request: Cursor<Vec<u8>>,
    responders: mpsc::Receiver<Responder>,
}

impl Read for EndsAfterTheAnswer {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let read = self.request.read(buf)?;
        if read == 0 {
            let responder = self.responders.try_recv().unwrap();
            assert!(responder.respond(Ok(json!({}))).is_err());
        }
        Ok(read)
    }
}

#[test]
fn p3_mcp_05_an_input_that_ends_after_a_failed_write_ends_as_the_failure() {
    let (responders, kept) = mpsc::channel();
    let mut handler = Keeping {
        responders,
        ended: None,
    };
    let input = EndsAfterTheAnswer {
        request: Cursor::new(format!("{}\n", request(1, "ping", &json!({}))).into_bytes()),
        responders: kept,
    };
    let ended = transport::run(
        Reader::new(BufReader::new(input)),
        &Outbox::new(Gone),
        &mut handler,
    );
    assert_eq!(ended.unwrap_err().kind(), io::ErrorKind::BrokenPipe);
    // The handler is not asked to answer what it still owes, which could
    // never reach the client.
    assert_eq!(handler.ended, Some(Err(io::ErrorKind::BrokenPipe)));
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

// P5-MCP-01: a call's point of no return. A verb that moves things claims
// it first; the deadline, a cancellation and a gone client claim the stop.

/// Whether the server has written anything for `wait`: false when nothing
/// came in that time.
fn wrote_within(client: &Client, wait: Duration) -> bool {
    client.messages.recv_timeout(wait).is_ok()
}

#[test]
fn p5_mcp_01_a_call_stopped_before_its_point_of_no_return_acts_on_nothing() {
    // Past its deadline: answered deadline_exceeded, and its verb, which
    // reaches its point of no return only after that, finds the call
    // stopped and acts on nothing.
    let env = TestEnv::new();
    let runner = TestRunner::new();
    let timing = Timing {
        deadline: Some(Duration::from_millis(50)),
        ..quick()
    };
    let mut client = Client::start(server(&env, &runner, timing));
    client.send(&call(1, "test_commit_late", "first"));
    let first = client.next();
    assert_eq!(first["id"], 1);
    assert_eq!(
        first["result"]["structuredContent"]["error"]["code"], "deadline_exceeded",
        "{first}"
    );
    runner.latch.open();
    // The next call runs once the first call's verb has returned.
    client.send(&call(2, "test_echo", "second"));
    assert_eq!(client.next()["id"], 2);
    assert_eq!(
        runner.log(),
        ["start first", "stopped first", "start second", "end second"]
    );
    assert!(client.close().is_empty());

    // Cancelled: never answered, and its verb acts on nothing.
    let runner = TestRunner::new();
    let mut client = Client::start(server(&env, &runner, quick()));
    client.send(&call(1, "test_commit_late", "first"));
    runner.wait_for("start first");
    client.send(&cancel(1));
    // The reader takes the cancellation before it answers the ping.
    client.send(&request(2, "ping", &json!({})));
    assert_eq!(client.ids(1), [2]);
    runner.latch.open();
    client.send(&call(3, "test_echo", "second"));
    assert_eq!(client.ids(1), [3]);
    assert_eq!(
        runner.log(),
        ["start first", "stopped first", "start second", "end second"]
    );
    assert!(client.close().is_empty(), "request 1 got an answer");
}

#[test]
fn p5_mcp_01_a_committed_call_past_its_deadline_is_answered_with_its_result() {
    let env = TestEnv::new();
    let runner = TestRunner::new();
    let timing = Timing {
        deadline: Some(Duration::from_millis(50)),
        ..quick()
    };
    let mut client = Client::start(server(&env, &runner, timing));
    client.send(&call(1, "test_commit", "first"));
    runner.wait_for("committed first");
    // Well past the deadline, and nothing has answered the call.
    assert!(
        !wrote_within(&client, Duration::from_millis(200)),
        "the committed call was answered before its verb returned"
    );
    runner.latch.open();
    let response = client.next();
    assert_eq!(response["id"], 1);
    let result = &response["result"];
    assert_eq!(result["isError"], false, "{response}");
    assert_eq!(result["structuredContent"]["label"], "first");
    assert_eq!(result["content"][0]["text"], "ran \"first\"");
    assert_eq!(
        runner.log(),
        ["start first", "committed first", "end first"]
    );
    // The next call has its own deadline.
    client.send(&call(2, "test_echo", "second"));
    assert_eq!(client.next()["result"]["isError"], false);
    assert!(client.close().is_empty());
}

#[test]
fn p5_mcp_01_a_cancellation_after_the_point_of_no_return_still_gets_the_result() {
    let env = TestEnv::new();
    let runner = TestRunner::new();
    let mut client = Client::start(server(&env, &runner, quick()));
    client.send(&call(1, "test_commit", "first"));
    runner.wait_for("committed first");
    client.send(&cancel(1));
    client.send(&request(2, "ping", &json!({})));
    assert_eq!(client.ids(1), [2]);
    runner.latch.open();
    let response = client.next();
    assert_eq!(response["id"], 1, "{response}");
    assert_eq!(response["result"]["isError"], false, "{response}");
    assert_eq!(response["result"]["structuredContent"]["label"], "first");
    assert_eq!(
        runner.log(),
        ["start first", "committed first", "end first"]
    );
    assert!(client.close().is_empty());
}

#[test]
fn p5_mcp_01_a_gone_client_stops_a_call_before_its_point_of_no_return_not_after() {
    // How long the tests let a progress that cannot be sent go out: ten of
    // its 20 ms intervals.
    let gone_by = Duration::from_millis(200);

    // Before: the first progress that cannot be sent stops the call, and
    // its verb, reaching its point of no return later, acts on nothing.
    let env = TestEnv::new();
    let runner = TestRunner::new();
    let (mut input, transport) = start_gone(server(&env, &runner, quick()));
    send(
        &mut input,
        &call_with_progress(1, "test_commit_late", "first"),
    );
    runner.wait_for("start first");
    thread::sleep(gone_by);
    drop(input);
    runner.latch.open();
    assert_eq!(
        transport.wait().unwrap_err().kind(),
        io::ErrorKind::BrokenPipe
    );
    assert_eq!(runner.log(), ["start first", "stopped first"]);

    // After: neither the progress that cannot be sent nor the input's end
    // stops a committed call, whose verb runs to its end before the server
    // ends, its answer failing as the client has gone.
    let runner = TestRunner::new();
    let (mut input, transport) = start_gone(server(&env, &runner, quick()));
    send(&mut input, &call_with_progress(1, "test_commit", "first"));
    runner.wait_for("committed first");
    thread::sleep(gone_by);
    drop(input);
    let opener = {
        let runner = runner.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            runner.latch.open();
        })
    };
    assert_eq!(
        transport.wait().unwrap_err().kind(),
        io::ErrorKind::BrokenPipe
    );
    opener.join().unwrap();
    assert_eq!(
        runner.log(),
        ["start first", "committed first", "end first"]
    );
}

#[test]
fn p5_mcp_01_the_input_ending_stops_every_call_before_its_point_of_no_return() {
    // The input ends while a call that would move things waits before its
    // point of no return, a second waits in the queue behind it, and a read
    // behind that: the client may have gone, so neither of the first two may
    // commit. All three still run and are answered in order; the read, which
    // has no point of no return, as before.
    let env = TestEnv::new();
    let runner = TestRunner::new();
    let mut client = Client::start(server(&env, &runner, quick()));
    client.send(&call(1, "test_commit_late", "first"));
    client.send(&call(2, "test_commit_late", "second"));
    client.send(&call(3, "test_echo", "third"));
    runner.wait_for("start first");
    drop(client.input.take());
    // The latch opens only once the reader has met the end of the input.
    let first = runner.calls.lock().unwrap()[0].clone();
    let started = Instant::now();
    while first.stage.load(Ordering::Acquire) != STOPPED {
        assert!(
            started.elapsed() < PATIENCE,
            "the end of the input left the running call free to commit: {:?}",
            runner.log()
        );
        thread::sleep(Duration::from_millis(1));
    }
    // Its point of no return is gone, not the call: nothing tells a verb
    // that reads to stop.
    assert!(!first.cancel.load(Ordering::Relaxed));
    runner.latch.open();
    let answers = client.close();
    let ids: Vec<&Value> = answers.iter().map(|answer| &answer["id"]).collect();
    assert_eq!(ids, [1, 2, 3], "{answers:?}");
    for answer in &answers[..2] {
        assert_eq!(answer["result"]["isError"], true, "{answer}");
    }
    assert_eq!(answers[2]["result"]["isError"], false, "{}", answers[2]);
    assert_eq!(
        runner.log(),
        [
            "start first",
            "stopped first",
            "start second",
            "stopped second",
            "start third",
            "end third"
        ]
    );
}

// P3-MCP-08: the panic hook answers before the process ends. The hook is
// the binary's; here the worker's part of it is called on a worker of the
// test's own, since a test process runs many servers at once and the hook
// answers for the last one started.

/// What an output has been given, which the test reads back.
#[derive(Clone, Default)]
struct Recorded(Arc<Mutex<Vec<u8>>>);

impl Write for Recorded {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Recorded {
    fn bytes(&self) -> Vec<u8> {
        self.0.lock().unwrap().clone()
    }

    /// Every message written, each a whole line.
    fn messages(&self) -> Vec<Value> {
        let bytes = self.bytes();
        let text = String::from_utf8(bytes).unwrap();
        assert!(
            text.is_empty() || text.ends_with('\n'),
            "a partial line: {text:?}"
        );
        text.lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}

fn id(n: u64) -> Id {
    Id::from_value(&json!(n)).unwrap()
}

/// The tool call `id`, of `tool` with `label`, as the reader hands it on.
fn tool_request(n: u64, tool: &str, label: &str) -> Request {
    let params = json!({"name": tool, "arguments": {"label": label}});
    Request {
        id: id(n),
        method: "tools/call".to_owned(),
        params: params.as_object().cloned(),
    }
}

/// A worker running the fake tools, and responders that answer on `output`.
fn worker(
    env: &TestEnv,
    runner: &TestRunner,
    output: &Recorded,
) -> (Worker, impl Fn(u64) -> Responder) {
    let worker = Worker::start(
        Arc::new(fake_tools(env, runner)),
        ServerConfig::new(PLACEHOLDER_INSTRUCTIONS),
        quick(),
    )
    .unwrap();
    let outbox = Outbox::new(output.clone());
    (worker, move |n| Responder::new(id(n), outbox.clone()))
}

#[test]
fn p3_mcp_08_the_hook_answers_the_running_call_then_the_queue_in_order() {
    let env = TestEnv::new();
    let runner = TestRunner::new();
    let output = Recorded::default();
    let (worker, responder) = worker(&env, &runner, &output);
    worker.call(
        tool_request(1, "test_block", "first"),
        Revision::PerRequest,
        responder(1),
    );
    runner.wait_for("start first");
    worker.call(
        tool_request(2, "test_echo", "second"),
        Revision::PerRequest,
        responder(2),
    );
    // A request the reader answered, waiting for the calls before it.
    let answered = RawValue::from_string(r#"{"answered":3}"#.to_owned()).unwrap();
    worker.answer(responder(3), Ok(answered));
    worker.call(
        tool_request(4, "test_echo", "fourth"),
        Revision::Session,
        responder(4),
    );
    worker.call(
        tool_request(5, "test_echo", "cancelled"),
        Revision::PerRequest,
        responder(5),
    );
    assert!(worker.cancel(&id(5)));
    worker.shared.answer_before_abort();
    let messages = output.messages();
    let ids: Vec<&Value> = messages.iter().map(|m| &m["id"]).collect();
    assert_eq!(ids, [1, 2, 3, 4], "{messages:#?}");
    // The hook ran on the test's thread, not the call's: the server, not
    // the verb, panicked.
    assert_eq!(
        messages[0]["error"],
        json!({
            "code": -32603,
            "message": "internal error: the server panicked while test_block ran; the server \
                        is stopping",
        })
    );
    for (message, tool) in [(&messages[1], "test_echo"), (&messages[3], "test_echo")] {
        assert_eq!(message["error"]["code"], -32603, "{message}");
        assert_eq!(
            message["error"]["message"],
            format!("internal error: the server is stopping after a panic: {tool} did not run")
        );
    }
    assert_eq!(messages[2]["result"], json!({"answered": 3}));
    // The running call was stopped, so its verb acts on nothing; nothing
    // queued ever runs, and nothing more is written.
    runner.wait_for("stopped first");
    worker.finish();
    assert_eq!(runner.log(), ["start first", "stopped first"]);
    assert_eq!(output.messages().len(), 4);
}

#[test]
fn p3_mcp_08_a_call_past_its_point_of_no_return_is_told_some_may_be_done() {
    let env = TestEnv::new();
    let runner = TestRunner::new();
    let output = Recorded::default();
    let (worker, responder) = worker(&env, &runner, &output);
    worker.call(
        tool_request(1, "test_commit", "first"),
        Revision::PerRequest,
        responder(1),
    );
    runner.wait_for("committed first");
    worker.shared.answer_before_abort();
    let messages = output.messages();
    assert_eq!(messages.len(), 1, "{messages:#?}");
    assert_eq!(messages[0]["id"], 1);
    assert_eq!(
        messages[0]["error"]["message"],
        "internal error: the server panicked while test_commit ran past its point of no return: \
         some of what the call asked for may be done; the server is stopping"
    );
    // A committed call is never stopped: its verb runs to its end, and its
    // answer is not sent twice.
    runner.latch.open();
    runner.wait_for("end first");
    worker.finish();
    assert_eq!(
        runner.log(),
        ["start first", "committed first", "end first"]
    );
    assert_eq!(output.messages().len(), 1);
}

#[test]
fn p3_mcp_08_a_call_answered_already_is_not_answered_again() {
    // Answered at its deadline while its verb, which cannot stop, still
    // runs: the hook has nothing to send for it.
    let env = TestEnv::new();
    let runner = TestRunner::new();
    let output = Recorded::default();
    let worker = Worker::start(
        Arc::new(fake_tools(&env, &runner)),
        ServerConfig::new(PLACEHOLDER_INSTRUCTIONS),
        Timing {
            deadline: Some(Duration::from_millis(20)),
            ..quick()
        },
    )
    .unwrap();
    let outbox = Outbox::new(output.clone());
    worker.call(
        tool_request(1, "test_stubborn", "first"),
        Revision::PerRequest,
        Responder::new(id(1), outbox),
    );
    let started = Instant::now();
    while output.messages().is_empty() {
        assert!(started.elapsed() < PATIENCE, "never answered");
        thread::sleep(Duration::from_millis(1));
    }
    assert!(worker.shared.state().running.is_some());
    worker.shared.answer_before_abort();
    runner.latch.open();
    worker.finish();
    let messages = output.messages();
    assert_eq!(messages.len(), 1, "{messages:#?}");
    assert_eq!(
        messages[0]["result"]["structuredContent"]["error"]["code"],
        "deadline_exceeded"
    );
    assert_eq!(runner.log(), ["start first", "end first"]);
}

/// An output whose first write answers another request through the hook's
/// path, on the thread that holds the output's lock, as a panic hook on
/// that thread would: what that answer met.
struct Reentrant {
    recorded: Recorded,
    outbox: Arc<OnceLock<Outbox>>,
    met: Arc<Mutex<Option<(io::ErrorKind, Duration)>>>,
}

impl Write for Reentrant {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut met = self.met.lock().unwrap();
        if met.is_none() {
            let outbox = self.outbox.get().unwrap().clone();
            let started = Instant::now();
            let sent =
                Responder::new(id(2), outbox).respond_before_abort(Err(RpcError::internal("x")));
            *met = Some((sent.unwrap_err().kind(), started.elapsed()));
        }
        self.recorded.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// An output that breaks partway through a line, as a writer that panics
/// would leave it.
struct Breaks(Recorded);

impl Write for Breaks {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(&buf[..buf.len() / 2])?;
        panic!("the output broke mid-line");
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn p3_mcp_08_the_hook_writes_nothing_while_the_output_is_held_or_broken() {
    // Held by the thread that would answer: it gives up within its bound,
    // and the line being written goes out whole, alone.
    let recorded = Recorded::default();
    let outbox = Arc::new(OnceLock::new());
    let met = Arc::new(Mutex::new(None));
    let output = Outbox::new(Reentrant {
        recorded: recorded.clone(),
        outbox: Arc::clone(&outbox),
        met: Arc::clone(&met),
    });
    outbox.set(output.clone()).unwrap();
    Responder::new(id(1), output)
        .respond(Ok(json!({})))
        .unwrap();
    let (kind, waited) = met.lock().unwrap().unwrap();
    assert_eq!(kind, io::ErrorKind::WouldBlock);
    assert!(waited >= Duration::from_millis(100), "{waited:?}");
    assert!(waited < Duration::from_secs(5), "{waited:?}");
    let messages = recorded.messages();
    assert_eq!(messages.len(), 1, "{messages:#?}");
    assert_eq!(messages[0]["id"], 1);

    // Broken by a write that panicked: nothing is added after its part of a
    // line.
    let recorded = Recorded::default();
    let output = Outbox::new(Breaks(recorded.clone()));
    let writer = output.clone();
    let panicked = thread::spawn(move || writer.notify("notifications/x", None)).join();
    assert!(panicked.is_err());
    let partial = recorded.bytes();
    assert!(!partial.is_empty());
    let sent = Responder::new(id(1), output).respond_before_abort(Err(RpcError::internal("x")));
    assert!(sent.is_err());
    assert_eq!(recorded.bytes(), partial);
}

#[test]
fn p3_mcp_08_under_unwinding_a_panicked_verb_is_answered_and_the_server_goes_on() {
    // No hook here: where panics unwind, the worker answers the call itself
    // and runs the next.
    let env = TestEnv::new();
    let runner = TestRunner::new();
    let mut client = Client::start(server(&env, &runner, quick()));
    client.send(&call(1, "test_panic", "first"));
    client.send(&call(2, "test_echo", "second"));
    client.send(&call(3, "test_commit_panic", "third"));
    let first = client.next();
    assert_eq!(first["id"], 1);
    assert_eq!(
        first["error"],
        json!({"code": -32603, "message": "internal error: test_panic's verb panicked"})
    );
    let second = client.next();
    assert_eq!(second["id"], 2);
    assert_eq!(second["result"]["isError"], false, "{second}");
    let third = client.next();
    assert_eq!(third["id"], 3);
    assert_eq!(
        third["error"]["message"],
        "internal error: test_commit_panic's verb panicked past its point of no return: some of \
         what the call asked for may be done"
    );
    assert!(client.close().is_empty());
    assert_eq!(
        runner.log(),
        [
            "start first",
            "start second",
            "end second",
            "start third",
            "committed third"
        ]
    );
}

// P5-SEC-05: the queue's limits, and the deadline over a call's argument
// checks.

/// The busy error, as every refused request carries it.
fn assert_busy(message: &Value, id: u64) {
    assert_eq!(message["id"], id, "{message}");
    assert_eq!(message["error"]["code"], jsonrpc::SERVER_BUSY, "{message}");
    let data = &message["error"]["data"];
    assert_eq!(
        data["max_queued_requests"], MAX_QUEUED_REQUESTS,
        "{message}"
    );
    assert_eq!(data["max_queued_bytes"], MAX_QUEUED_BYTES, "{message}");
}

#[test]
fn p5_sec_05_the_busy_code_lies_outside_what_mcp_and_json_rpc_reserve() {
    assert!(
        !(-32768..=-32000).contains(&jsonrpc::SERVER_BUSY),
        "MCP 2026-07-28: new codes lie outside the JSON-RPC reserved range"
    );
}

#[test]
fn p5_sec_05_a_request_past_the_count_limit_is_answered_busy_and_never_runs() {
    let env = TestEnv::new();
    let runner = TestRunner::new();
    let mut client = Client::start(server(&env, &runner, quick()));
    client.send(&call(1, "test_block", "first"));
    runner.wait_for("start first");
    let queued = u64::try_from(MAX_QUEUED_REQUESTS).unwrap();
    for id in 2..2 + queued {
        client.send(&call(id, "test_echo", &format!("queued {id}")));
    }
    // Sent in the order of the ids: the reader has read the queued ones
    // when it answers these, ahead of everything waiting.
    for id in 2 + queued..2 + queued + 3 {
        client.send(&call(id, "test_echo", "over"));
    }
    for id in 2 + queued..2 + queued + 3 {
        assert_busy(&client.next(), id);
    }
    // The reader still answers what it answers at once.
    client.send(&request(9000, "ping", &json!({})));
    assert_eq!(client.ids(1), [9000]);
    assert_eq!(runner.log(), ["start first"]);
    runner.latch.open();
    let answered = client.ids(MAX_QUEUED_REQUESTS + 1);
    let expected: Vec<Value> = (1..2 + queued).map(|id| json!(id)).collect();
    assert_eq!(answered, expected, "the ones it kept run in order");
    assert!(
        !runner.log().iter().any(|line| line == "start over"),
        "{:?}",
        runner.log()
    );
    assert!(client.close().is_empty());
}

#[test]
fn p5_sec_05_a_request_past_the_byte_limit_is_answered_busy() {
    let env = TestEnv::new();
    let runner = TestRunner::new();
    let mut client = Client::start(server(&env, &runner, quick()));
    client.send(&call(1, "test_block", "first"));
    runner.wait_for("start first");
    // Measured parsed: each is about 240 KB of text and over half the limit
    // in memory, so the first waits and the second does not.
    let objects = vec![json!({"a": 0}); 30_000];
    let heavy = |id| {
        request(
            id,
            "tools/call",
            &json!({"name": "test_echo", "arguments": {"label": objects}}),
        )
    };
    let held = heavy(2)["params"].as_object().map_or(0, held_by_fields);
    assert!(
        held > MAX_QUEUED_BYTES / 2 && held <= MAX_QUEUED_BYTES,
        "{held}"
    );
    assert!(heavy(2).to_string().len() < crate::transport::MAX_MESSAGE_BYTES);
    client.send(&heavy(2));
    client.send(&heavy(3));
    assert_busy(&client.next(), 3);
    runner.latch.open();
    assert_eq!(client.ids(2), [1, 2]);
    assert!(client.close().is_empty());
}

/// A worker whose call `first` is blocked and running, and responders that
/// answer on `output`.
fn blocked_worker(
    env: &TestEnv,
    runner: &TestRunner,
    output: &Recorded,
) -> (Worker, impl Fn(u64) -> Responder) {
    let (worker, responder) = worker(env, runner, output);
    worker.call(
        tool_request(1, "test_block", "first"),
        Revision::PerRequest,
        responder(1),
    );
    runner.wait_for("start first");
    (worker, responder)
}

/// What the requests waiting hold: how many, and the bytes.
fn queued(worker: &Worker) -> (usize, usize) {
    let state = worker.shared.state();
    (state.queue.len(), state.queued_bytes)
}

#[test]
fn p5_sec_05_the_queue_never_holds_more_than_its_limits() {
    let env = TestEnv::new();
    let runner = TestRunner::new();
    let output = Recorded::default();
    let (worker, responder) = blocked_worker(&env, &runner, &output);
    let label = "x".repeat(1024 * 1024);
    let sent: u64 = 100;
    for n in 2..2 + sent {
        worker.call(
            tool_request(n, "test_echo", &label),
            Revision::PerRequest,
            responder(n),
        );
        let (held, bytes) = queued(&worker);
        assert!(held <= MAX_QUEUED_REQUESTS, "{held} requests");
        assert!(bytes <= MAX_QUEUED_BYTES, "{bytes} bytes");
    }
    // 1 MiB of label each: the bytes are the limit here, a few below 32.
    let (held, bytes) = queued(&worker);
    assert!((29..=32).contains(&held), "{held} calls, {bytes} bytes");
    let refused = usize::try_from(sent).unwrap() - held;
    let messages = output.messages();
    assert_eq!(messages.len(), refused);
    for message in &messages {
        assert_eq!(message["error"]["code"], jsonrpc::SERVER_BUSY, "{message}");
    }
    runner.latch.open();
    worker.finish();
    assert_eq!(output.messages().len(), usize::try_from(sent).unwrap() + 1);
}

#[test]
fn p5_sec_05_the_count_and_the_bytes_come_back_as_requests_leave() {
    let env = TestEnv::new();
    let runner = TestRunner::new();
    let output = Recorded::default();
    let (worker, responder) = blocked_worker(&env, &runner, &output);
    for n in 2..6 {
        worker.call(
            tool_request(n, "test_echo", &"y".repeat(1024)),
            Revision::PerRequest,
            responder(n),
        );
    }
    let (held, four) = queued(&worker);
    assert_eq!(held, 4);
    assert!(four > 4 * 1024, "{four} bytes");
    // A cancelled one gives back what it held.
    assert!(worker.cancel(&id(3)));
    let (held, three) = queued(&worker);
    assert_eq!(held, 3);
    assert!(three < four, "{three} of {four} bytes");
    runner.latch.open();
    // Answered: the running call and three of the queued ones.
    let started = Instant::now();
    while output.messages().len() < 4 {
        assert!(started.elapsed() < PATIENCE, "{:?}", output.messages());
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(queued(&worker), (0, 0));
    worker.finish();
}

#[test]
fn p5_sec_05_a_full_queue_still_hears_cancellations() {
    let env = TestEnv::new();
    let runner = TestRunner::new();
    let mut client = Client::start(server(&env, &runner, quick()));
    client.send(&call(1, "test_block", "first"));
    runner.wait_for("start first");
    let queued = u64::try_from(MAX_QUEUED_REQUESTS).unwrap();
    for id in 2..2 + queued {
        client.send(&call(id, "test_echo", "queued"));
    }
    client.send(&call(2 + queued, "test_echo", "over"));
    assert_busy(&client.next(), 2 + queued);
    // Cancelling one that waits makes room for one more.
    client.send(&cancel(2));
    client.send(&request(9000, "ping", &json!({})));
    assert_eq!(client.ids(1), [9000], "the cancellation was read in turn");
    client.send(&call(100_000, "test_echo", "room"));
    // Cancelling the running call is heard behind a full queue.
    client.send(&cancel(1));
    runner.wait_for("stopped first");
    let answered = client.ids(MAX_QUEUED_REQUESTS);
    assert_eq!(answered.last(), Some(&json!(100_000)), "{answered:?}");
    assert!(client.close().is_empty());
}

#[test]
fn p5_sec_05_an_answer_past_the_limit_is_answered_busy() {
    let env = TestEnv::new();
    let runner = TestRunner::new();
    let mut client = Client::start(server(&env, &runner, quick()));
    client.send(&call(1, "test_block", "first"));
    runner.wait_for("start first");
    let queued = u64::try_from(MAX_QUEUED_REQUESTS).unwrap();
    for id in 2..2 + queued {
        // Neither ping, discovery nor tools/list: each waits as an answer.
        client.send(&request(id, "resources/list", &json!({})));
    }
    client.send(&request(2 + queued, "resources/list", &json!({})));
    assert_busy(&client.next(), 2 + queued);
    runner.latch.open();
    let rest: Vec<Value> = (0..=MAX_QUEUED_REQUESTS).map(|_| client.next()).collect();
    assert_eq!(rest[0]["id"], 1);
    assert_eq!(rest[1]["error"]["code"], -32601, "{}", rest[1]);
    assert!(client.close().is_empty());
}

#[test]
fn p5_sec_05_the_measure_counts_what_a_parsed_message_holds() {
    // A long string costs its length.
    let label = "x".repeat(1024 * 1024);
    let string: Value = serde_json::from_str(&format!("[\"{label}\"]")).unwrap();
    let held = held_bytes(&string);
    assert!(
        (1024 * 1024..2 * 1024 * 1024).contains(&held),
        "{held} bytes"
    );
    // Small objects cost far more than the text: `{"a":0},` is 8 bytes of
    // JSON, and a node, a key and a slot in memory.
    let count: usize = 10_000;
    let text = format!("[{}{{\"a\":0}}]", "{\"a\":0},".repeat(count - 1));
    let objects: Value = serde_json::from_str(&text).unwrap();
    let held = held_bytes(&objects);
    assert!(held >= count * 600, "{held} bytes for {count} objects");
    assert!(
        held >= text.len() * 60,
        "{held} bytes for {} of text",
        text.len()
    );
    // Nothing at all costs nothing beyond the slot.
    assert_eq!(held_bytes(&Value::Null), 0);
    assert_eq!(held_bytes(&json!({})), 0);
}

#[test]
fn p5_sec_05_a_request_the_worker_takes_at_once_is_never_busy() {
    let env = TestEnv::new();
    let runner = TestRunner::new();
    let mut client = Client::start(server(&env, &runner, quick()));
    // Parsed, this message holds more than the queue allows; nothing runs
    // or waits, so it is taken, refused by its arguments alone.
    let count = MAX_QUEUED_BYTES / 600 + 1;
    let objects = vec![json!({"a": 0}); count];
    let message = request(
        1,
        "tools/call",
        &json!({"name": "test_echo", "arguments": {"label": objects}}),
    );
    assert!(
        message["params"].as_object().map_or(0, held_by_fields) > MAX_QUEUED_BYTES,
        "the message is meant to be past the limit"
    );
    client.send(&message);
    let answered = client.next();
    assert_eq!(answered["id"], 1);
    assert_eq!(answered["result"]["isError"], true, "{answered}");
    assert!(client.close().is_empty());
}

/// A tool whose argument checks wait for a latch, as binding a path to the
/// roots waits on a slow file system.
struct SlowChecks {
    latch: Arc<Latch>,
    runner: TestRunner,
}

impl Tool for SlowChecks {
    fn name(&self) -> &'static str {
        "test_slow_checks"
    }

    fn description(&self) -> &'static str {
        "A tool whose checks wait (a test fixture)."
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

    fn invocation(&self, _: &Map<String, Value>, _: &Context<'_>) -> Result<Invocation, Refusal> {
        self.runner.note("checking".to_owned());
        self.latch.wait(|| false);
        self.runner.note("checked".to_owned());
        Ok(Invocation {
            verb: "echo",
            args: vec!["slow".into()],
        })
    }

    fn summary(&self, _: &Value) -> String {
        "ran".to_owned()
    }
}

#[test]
fn p5_sec_05_the_deadline_covers_the_argument_checks() {
    let env = TestEnv::new();
    let runner = TestRunner::new();
    let latch = Arc::new(Latch::default());
    let mut tools = fake_tools(&env, &runner);
    tools
        .registry
        .register(Box::new(SlowChecks {
            latch: Arc::clone(&latch),
            runner: runner.clone(),
        }))
        .unwrap();
    let server = Server::new(ServerConfig::new(PLACEHOLDER_INSTRUCTIONS))
        .with_tools(tools)
        .with_timing(Timing {
            progress_every: Duration::from_millis(20),
            deadline: Some(Duration::from_millis(50)),
        });
    let mut client = Client::start(server);
    client.send(&request(
        1,
        "tools/call",
        &json!({"name": "test_slow_checks", "arguments": {}}),
    ));
    runner.wait_for("checking");
    // Answered while its checks still wait.
    let answered = client.next();
    assert_eq!(answered["id"], 1);
    assert_eq!(answered["result"]["isError"], true, "{answered}");
    let error = &answered["result"]["structuredContent"]["error"];
    assert_eq!(error["code"], "deadline_exceeded", "{answered}");
    assert!(
        error["hint"]
            .as_str()
            .unwrap()
            .contains("run duscope in a terminal"),
        "the verb is not known yet: {answered}"
    );
    assert_eq!(runner.log(), ["checking"], "the checks are still running");
    // The next call waits for them, and the stopped call's verb finds it
    // stopped.
    client.send(&call(2, "test_echo", "second"));
    thread::sleep(Duration::from_millis(50));
    assert_eq!(runner.log(), ["checking"], "the second call waits");
    latch.open();
    runner.wait_for("end second");
    assert_eq!(
        runner.log(),
        [
            "checking",
            "checked",
            "start slow",
            "stopped slow",
            "start second",
            "end second"
        ]
    );
    assert_eq!(client.next()["id"], 2);
    assert!(client.close().is_empty());
}

#[test]
fn p5_sec_05_a_call_cancelled_during_its_checks_finds_it_stopped_and_gets_no_answer() {
    let env = TestEnv::new();
    let runner = TestRunner::new();
    let latch = Arc::new(Latch::default());
    let mut tools = fake_tools(&env, &runner);
    tools
        .registry
        .register(Box::new(SlowChecks {
            latch: Arc::clone(&latch),
            runner: runner.clone(),
        }))
        .unwrap();
    let server = Server::new(ServerConfig::new(PLACEHOLDER_INSTRUCTIONS))
        .with_tools(tools)
        .with_timing(quick());
    let mut client = Client::start(server);
    client.send(&request(
        1,
        "tools/call",
        &json!({"name": "test_slow_checks", "arguments": {}}),
    ));
    runner.wait_for("checking");
    client.send(&cancel(1));
    client.send(&request(9, "ping", &json!({})));
    assert_eq!(client.ids(1), [9]);
    latch.open();
    runner.wait_for("checked");
    client.send(&call(2, "test_echo", "second"));
    assert_eq!(client.ids(1), [2], "the cancelled call is not answered");
    assert_eq!(
        runner.log(),
        [
            "checking",
            "checked",
            "start slow",
            "stopped slow",
            "start second",
            "end second"
        ]
    );
    assert!(client.close().is_empty());
}
