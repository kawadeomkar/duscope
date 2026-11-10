//! P3-MCP-01: the stdio transport, fed malformed, oversized and interleaved
//! input in memory, and on a real process's standard streams with every log
//! record at debug level on.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::collections::BTreeSet;
use std::io::{self, BufRead, BufReader, Cursor, Read, Write};
use std::process::Stdio;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use duscope_mcp::jsonrpc::{Notification, Request, RpcError};
use duscope_mcp::transport::{
    self, Handler, MAX_MESSAGE_BYTES, Outbox, READER_THREAD, Reader, Responder, Transport,
};
use duscope_testkit::TestEnv;
use serde_json::{Value, json};

/// A shared byte buffer standing in for stdout.
#[derive(Clone, Default)]
struct Buffer(Arc<Mutex<Vec<u8>>>);

impl Write for Buffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Buffer {
    fn bytes(&self) -> Vec<u8> {
        self.0.lock().unwrap().clone()
    }

    /// Every line written, each checked to end with a newline and to hold
    /// one JSON-RPC message, parsed.
    fn messages(&self) -> Vec<Value> {
        let bytes = self.bytes();
        assert!(
            bytes.is_empty() || bytes.ends_with(b"\n"),
            "the output ends mid-line"
        );
        bytes
            .split(|&b| b == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| {
                let message: Value = serde_json::from_slice(line).unwrap_or_else(|e| {
                    panic!("not one message ({e}): {}", String::from_utf8_lossy(line))
                });
                assert_eq!(message["jsonrpc"], "2.0", "{message}");
                message
            })
            .collect()
    }
}

/// Answers `echo` with its params, leaves `drop` unanswered and `abandon`
/// abandoned, and anything else with method not found; keeps the methods of
/// the notifications and the threads requests arrived on.
#[derive(Clone, Default)]
struct Echo {
    notifications: Arc<Mutex<Vec<String>>>,
    threads: Arc<Mutex<Vec<Option<String>>>>,
}

impl Handler for Echo {
    fn request(&mut self, request: Request, responder: Responder) {
        self.threads
            .lock()
            .unwrap()
            .push(thread::current().name().map(str::to_owned));
        match request.method.as_str() {
            "echo" => responder
                .respond(Ok(Value::Object(request.params.unwrap_or_default())))
                .unwrap(),
            "drop" => drop(responder),
            "abandon" => responder.abandon(),
            method => responder
                .respond(Err(RpcError::method_not_found(method)))
                .unwrap(),
        }
    }

    fn notification(&mut self, notification: Notification) {
        self.notifications.lock().unwrap().push(notification.method);
    }
}

/// `input` through the transport with `Echo`: the messages written and the
/// notifications handled.
fn serve(input: &[u8]) -> (Vec<Value>, Vec<String>) {
    let out = Buffer::default();
    let mut echo = Echo::default();
    transport::run(Reader::new(input), &Outbox::new(out.clone()), &mut echo).unwrap();
    let notifications = echo.notifications.lock().unwrap().clone();
    (out.messages(), notifications)
}

fn request(id: &Value, method: &str, params: &Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string()
}

/// The code of an error response, after checking its shape: a message, and
/// the id it carries, or no `id` member at all when `id` is `None`.
fn error_code(message: &Value, id: Option<&Value>) -> i64 {
    let error = &message["error"];
    assert!(
        error["message"].as_str().is_some_and(|m| !m.is_empty()),
        "{message}"
    );
    assert!(message.get("result").is_none(), "{message}");
    assert_eq!(message.get("id"), id, "{message}");
    error["code"].as_i64().unwrap()
}

#[test]
fn p3_mcp_01_malformed_lines_get_their_error_code_and_the_next_line_is_read() {
    let deep = format!("{}{}", "[".repeat(200), "]".repeat(200));
    let cases: &[(&[u8], i64, Option<Value>)] = &[
        // Not JSON, or not UTF-8: -32700, no id.
        (b"{", -32700, None),
        (b"not json", -32700, None),
        (b"\xff\xfe{}", -32700, None),
        (
            br#"{"jsonrpc":"2.0","id":"a","method":"echo\xff"}"#,
            -32700,
            None,
        ),
        (
            br#"{"jsonrpc":"2.0","id":1,"method":"echo"} {}"#,
            -32700,
            None,
        ),
        (deep.as_bytes(), -32700, None),
        // JSON, but not one request or notification: -32600.
        (b"42", -32600, None),
        (b"\"text\"", -32600, None),
        (b"null", -32600, None),
        (
            br#"{"jsonrpc":"2.0","id":null,"method":"echo"}"#,
            -32600,
            None,
        ),
        (
            br#"{"jsonrpc":"2.0","id":1.5,"method":"echo"}"#,
            -32600,
            None,
        ),
        (
            br#"{"jsonrpc":"2.0","id":{},"method":"echo"}"#,
            -32600,
            None,
        ),
        (
            br#"{"jsonrpc":"2.0","id":true,"method":"echo"}"#,
            -32600,
            None,
        ),
        (br#"{"id":1,"method":"echo"}"#, -32600, Some(json!(1))),
        (
            br#"{"jsonrpc":"1.0","id":"a","method":"echo"}"#,
            -32600,
            Some(json!("a")),
        ),
        (
            br#"{"jsonrpc":2,"id":"b","method":"echo"}"#,
            -32600,
            Some(json!("b")),
        ),
        (
            br#"{"jsonrpc":"2.0","id":2,"method":7}"#,
            -32600,
            Some(json!(2)),
        ),
        (br#"{"jsonrpc":"2.0","id":3}"#, -32600, Some(json!(3))),
        (
            br#"{"jsonrpc":"2.0","id":4,"method":"echo","params":"x"}"#,
            -32600,
            Some(json!(4)),
        ),
        (
            br#"{"jsonrpc":"2.0","method":"echo","params":5}"#,
            -32600,
            None,
        ),
        // Positional parameters, which no MCP method takes: -32602.
        (
            br#"{"jsonrpc":"2.0","id":5,"method":"echo","params":[1]}"#,
            -32602,
            Some(json!(5)),
        ),
        // A method the handler does not have: -32601.
        (
            br#"{"jsonrpc":"2.0","id":6,"method":"nope"}"#,
            -32601,
            Some(json!(6)),
        ),
    ];
    for (line, code, id) in cases {
        let mut input = line.to_vec();
        input.push(b'\n');
        input.extend_from_slice(request(&json!(99), "echo", &json!({"after": true})).as_bytes());
        input.push(b'\n');
        let (messages, notifications) = serve(&input);
        let shown = String::from_utf8_lossy(line);
        assert_eq!(messages.len(), 2, "{shown}: {messages:?}");
        assert_eq!(error_code(&messages[0], id.as_ref()), *code, "{shown}");
        assert_eq!(
            messages[1],
            json!({"jsonrpc": "2.0", "id": 99, "result": {"after": true}})
        );
        assert!(notifications.is_empty(), "{shown}");
    }
}

#[test]
fn p3_mcp_01_batches_are_refused_whole() {
    let batch = format!(
        "[{},{}]\n[]\n",
        request(&json!(1), "echo", &json!({})),
        request(&json!(2), "echo", &json!({}))
    );
    let (messages, _) = serve(batch.as_bytes());
    assert_eq!(messages.len(), 2, "{messages:?}");
    for message in &messages {
        assert_eq!(error_code(message, None), -32600);
        assert!(
            message["error"]["message"]
                .as_str()
                .unwrap()
                .contains("batch"),
            "{message}"
        );
    }
}

/// A request for `echo` exactly `size` bytes long, its id `id`.
fn request_of_size(id: u64, size: usize) -> Vec<u8> {
    let empty = request(&json!(id), "echo", &json!({"pad": ""}));
    let line = request(
        &json!(id),
        "echo",
        &json!({"pad": "x".repeat(size - empty.len())}),
    );
    assert_eq!(line.len(), size);
    line.into_bytes()
}

#[test]
fn p3_mcp_01_a_message_over_1_mib_is_refused_and_the_next_one_answered() {
    assert_eq!(MAX_MESSAGE_BYTES, 1024 * 1024);
    let mut input = request_of_size(1, MAX_MESSAGE_BYTES + 1);
    input.push(b'\n');
    input.extend_from_slice(&request_of_size(2, MAX_MESSAGE_BYTES));
    input.push(b'\n');
    input.extend_from_slice(request(&json!(3), "echo", &json!({})).as_bytes());
    input.push(b'\n');
    // A last line past the limit without its newline is refused too.
    input.extend_from_slice(&request_of_size(4, MAX_MESSAGE_BYTES + 2));
    let (messages, _) = serve(&input);
    assert_eq!(messages.len(), 4);
    for refused in [&messages[0], &messages[3]] {
        assert_eq!(error_code(refused, None), -32600);
        assert_eq!(
            refused["error"]["data"],
            json!({"max_message_bytes": 1_048_576})
        );
    }
    // Exactly 1 MiB is read.
    assert_eq!(messages[1]["id"], 2);
    assert_eq!(
        messages[1]["result"]["pad"].as_str().unwrap().len(),
        MAX_MESSAGE_BYTES - request(&json!(2), "echo", &json!({"pad": ""})).len()
    );
    assert_eq!(
        messages[2],
        json!({"jsonrpc": "2.0", "id": 3, "result": {}})
    );
}

#[test]
fn p3_mcp_01_interleaved_requests_and_notifications_are_answered_in_order() {
    let lines = [
        request(&json!(1), "echo", &json!({"n": 1})),
        r#"{"jsonrpc":"2.0","method":"notifications/one"}"#.to_owned(),
        String::new(),
        "  \t ".to_owned(),
        r#"{"jsonrpc":"2.0","id":"two","method":"echo","params":null}"#.to_owned(),
        "{oops".to_owned(),
        // A response from the client: logged, never answered.
        r#"{"jsonrpc":"2.0","id":1,"result":{}}"#.to_owned(),
        r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"x"}}"#.to_owned(),
        r#"{"jsonrpc":"2.0","method":"notifications/two","params":{"k":"v"}}"#.to_owned(),
        // Positional params, which no MCP method takes: a notification is
        // still never answered.
        r#"{"jsonrpc":"2.0","method":"notifications/three","params":[1]}"#.to_owned(),
        request(&json!(3), "missing", &json!({})),
        request(&json!(4), "echo", &json!({"n": 4})),
    ];
    let mut input = lines.join("\n");
    input.push('\n');
    let (messages, notifications) = serve(input.as_bytes());
    assert_eq!(notifications, ["notifications/one", "notifications/two"]);
    let answered: Vec<Value> = messages
        .iter()
        .map(|m| m.get("id").cloned().unwrap_or(Value::Null))
        .collect();
    assert_eq!(
        answered,
        [json!(1), json!("two"), Value::Null, json!(3), json!(4)]
    );
    assert_eq!(messages[0]["result"], json!({"n": 1}));
    assert_eq!(messages[1]["result"], json!({}));
    assert_eq!(error_code(&messages[2], None), -32700);
    assert_eq!(error_code(&messages[3], Some(&json!(3))), -32601);
    assert_eq!(messages[4]["result"], json!({"n": 4}));
}

/// Hands out its input at most `step` bytes a read.
struct Trickle<'a> {
    rest: &'a [u8],
    step: usize,
}

impl Read for Trickle<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.step.min(buf.len()).min(self.rest.len());
        buf[..n].copy_from_slice(&self.rest[..n]);
        self.rest = &self.rest[n..];
        Ok(n)
    }
}

#[test]
fn p3_mcp_01_where_the_reads_split_the_input_changes_nothing() {
    let mut input = Vec::new();
    for line in [
        request(&json!(1), "echo", &json!({"text": "two\nlines é"})),
        r#"{"jsonrpc":"2.0","method":"notifications/x"}"#.to_owned(),
        "[1,2]".to_owned(),
        request(&json!(2), "echo", &json!({"pad": "y".repeat(300)})),
        "x".repeat(200),
        request(&json!(3), "echo", &json!({})),
    ] {
        input.extend_from_slice(line.as_bytes());
        input.extend_from_slice(b"\r\n");
    }
    let whole = Buffer::default();
    transport::run(
        Reader::with_limit(&input[..], 256),
        &Outbox::new(whole.clone()),
        &mut Echo::default(),
    )
    .unwrap();
    let messages = whole.messages();
    assert_eq!(messages.len(), 5, "{messages:?}");
    // The newline inside a string goes out escaped: one message, one line.
    assert_eq!(messages[0]["result"]["text"], "two\nlines é");
    assert_eq!(
        error_code(&messages[2], None),
        -32600,
        "over the limit of 256"
    );
    for step in [1, 2, 3, 7, 64, 255, 256, 257] {
        let out = Buffer::default();
        let trickle = BufReader::with_capacity(step, Trickle { rest: &input, step });
        transport::run(
            Reader::with_limit(trickle, 256),
            &Outbox::new(out.clone()),
            &mut Echo::default(),
        )
        .unwrap();
        assert_eq!(out.bytes(), whole.bytes(), "reads of {step} bytes");
    }
}

#[test]
fn p3_mcp_01_ids_come_back_as_they_were_sent() {
    let ids = [
        json!("abc"),
        json!(""),
        json!("1"),
        json!(0),
        json!(-1),
        json!(i64::MIN),
        json!(i64::MAX),
        json!(u64::MAX),
    ];
    let mut input = String::new();
    for id in &ids {
        input.push_str(&request(id, "echo", &json!({})));
        input.push('\n');
    }
    let out = Buffer::default();
    transport::run(
        Reader::new(input.as_bytes()),
        &Outbox::new(out.clone()),
        &mut Echo::default(),
    )
    .unwrap();
    let text = String::from_utf8(out.bytes()).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), ids.len());
    for (line, id) in lines.iter().zip(&ids) {
        assert_eq!(
            *line,
            format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{{}}}}"#)
        );
    }
}

/// Takes at most seven bytes a write, so a line written without the outbox's
/// lock would come apart among the others.
#[derive(Clone, Default)]
struct Choppy(Buffer);

impl Write for Choppy {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let n = bytes.len().min(7);
        self.0.write(&bytes[..n])?;
        thread::yield_now();
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Answers each request from a thread of its own.
struct Threaded(Arc<Mutex<Vec<thread::JoinHandle<()>>>>);

impl Handler for Threaded {
    fn request(&mut self, request: Request, responder: Responder) {
        self.0.lock().unwrap().push(thread::spawn(move || {
            responder
                .respond(Ok(Value::Object(request.params.unwrap_or_default())))
                .unwrap();
        }));
    }

    fn notification(&mut self, _: Notification) {}
}

#[test]
fn p3_mcp_01_messages_written_from_many_threads_never_interleave() {
    let mut input = String::new();
    for id in 0..32 {
        input.push_str(&request(
            &json!(id),
            "echo",
            &json!({"pad": "z".repeat(3000 + id)}),
        ));
        input.push('\n');
    }
    let out = Choppy::default();
    let outbox = Outbox::new(out.clone());
    let spawned = Arc::new(Mutex::new(Vec::new()));
    // Notifications from another thread at the same time.
    let notifier = {
        let outbox = outbox.clone();
        thread::spawn(move || {
            for n in 0..32 {
                outbox
                    .notify(
                        "notifications/progress",
                        Some(&json!({"progress": n, "pad": "p".repeat(2000)})),
                    )
                    .unwrap();
            }
        })
    };
    transport::run(
        Reader::new(input.as_bytes()),
        &outbox,
        &mut Threaded(Arc::clone(&spawned)),
    )
    .unwrap();
    for handle in spawned.lock().unwrap().drain(..) {
        handle.join().unwrap();
    }
    notifier.join().unwrap();
    let messages = out.0.messages();
    assert_eq!(messages.len(), 64);
    let ids: BTreeSet<u64> = messages.iter().filter_map(|m| m["id"].as_u64()).collect();
    assert_eq!(ids, (0..32).collect());
    for message in messages.iter().filter(|m| m.get("id").is_some()) {
        let id = usize::try_from(message["id"].as_u64().unwrap()).unwrap();
        assert_eq!(message["result"]["pad"].as_str().unwrap().len(), 3000 + id);
    }
    let progress = messages
        .iter()
        .filter(|m| m["method"] == "notifications/progress")
        .count();
    assert_eq!(progress, 32);
}

#[test]
fn p3_mcp_01_a_request_left_unanswered_gets_an_internal_error_and_an_abandoned_one_nothing() {
    let input = format!(
        "{}\n{}\n{}\n",
        request(&json!(1), "drop", &json!({})),
        request(&json!(2), "abandon", &json!({})),
        request(&json!(3), "echo", &json!({}))
    );
    let (messages, _) = serve(input.as_bytes());
    assert_eq!(messages.len(), 2, "{messages:?}");
    assert_eq!(error_code(&messages[0], Some(&json!(1))), -32603);
    assert_eq!(messages[1]["id"], 3);
}

/// A client that has gone away.
struct Closed;

impl Write for Closed {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::ErrorKind::BrokenPipe.into())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Counts the requests, dropping each responder, which answers for it.
#[derive(Default)]
struct Counting(usize);

impl Handler for Counting {
    fn request(&mut self, _: Request, responder: Responder) {
        self.0 += 1;
        drop(responder);
    }

    fn notification(&mut self, _: Notification) {}
}

#[test]
fn p3_mcp_01_a_failed_write_stops_the_reader() {
    let first = request(&json!(1), "echo", &json!({}));
    let input = format!("{first}\n{}\n", request(&json!(2), "echo", &json!({})));
    let mut cursor = Cursor::new(input.as_bytes());
    let mut counting = Counting::default();
    let ended = transport::run(
        Reader::new(&mut cursor),
        &Outbox::new(Closed),
        &mut counting,
    );
    assert_eq!(ended.unwrap_err().kind(), io::ErrorKind::BrokenPipe);
    // The first answer failed, so the second request was never read.
    assert_eq!(counting.0, 1);
    assert_eq!(cursor.position(), u64::try_from(first.len() + 1).unwrap());
}

/// A stream whose write panics, partway through a line.
struct Panicking;

impl Write for Panicking {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        panic!("the stream broke mid-line");
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn p3_mcp_01_a_panic_mid_write_fails_every_later_write() {
    let outbox = Outbox::new(Panicking);
    let writer = outbox.clone();
    let panicked = thread::spawn(move || writer.notify("notifications/x", None)).join();
    assert!(panicked.is_err());
    // The stream may hold part of a line: nothing more is written to it (a
    // write would panic again).
    let after = outbox.notify("notifications/x", None).unwrap_err();
    assert_eq!(after.kind(), io::ErrorKind::Other);
}

#[test]
fn p3_mcp_01_the_transport_reads_on_a_thread_of_its_own() {
    let input = format!(
        "{}\n{}\n",
        request(&json!(1), "echo", &json!({})),
        r#"{"jsonrpc":"2.0","method":"notifications/x"}"#
    );
    let out = Buffer::default();
    let echo = Echo::default();
    let transport =
        Transport::start(Cursor::new(input.into_bytes()), out.clone(), echo.clone()).unwrap();
    transport
        .outbox()
        .notify("notifications/hello", None)
        .unwrap();
    transport.wait().unwrap();
    assert_eq!(
        *echo.threads.lock().unwrap(),
        [Some(READER_THREAD.to_owned())]
    );
    assert_eq!(*echo.notifications.lock().unwrap(), ["notifications/x"]);
    let messages = out.messages();
    assert_eq!(messages.len(), 2);
    assert!(messages.contains(&json!({"jsonrpc": "2.0", "method": "notifications/hello"})));
    assert!(messages.contains(&json!({"jsonrpc": "2.0", "id": 1, "result": {}})));
}

/// The child test that serves stdio, and the variable that has it run.
const CHILD: &str = "p3_mcp_01_stdio_child_process";
const CHILD_VAR: &str = "DUSCOPE_TEST_MCP_STDIO_CHILD";

/// The line the child test writes just before it starts the transport: the
/// test harness's own output comes before it, the transport's after.
const STARTS: &str = "-- the transport starts here --\n";

#[test]
fn p3_mcp_01_stdout_is_byte_clean_at_debug_level() {
    let env = TestEnv::new();
    let mut child = env
        .command(std::env::current_exe().unwrap())
        .args(["--exact", CHILD, "--ignored", "--nocapture", "--quiet"])
        .args(["--test-threads", "1"])
        .env(CHILD_VAR, "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let (lines, received) = mpsc::channel();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let reading = thread::spawn(move || {
        loop {
            let mut line = Vec::new();
            if stdout.read_until(b'\n', &mut line).unwrap() == 0 {
                break;
            }
            lines.send(line).unwrap();
        }
    });
    let next = || {
        let line = received
            .recv_timeout(Duration::from_secs(60))
            .expect("no line on stdout in 60 s");
        String::from_utf8(line).unwrap()
    };
    let mut harness = Vec::new();
    loop {
        let line = next();
        if line == STARTS {
            break;
        }
        harness.push(line);
        assert!(harness.len() < 10, "no start line: {harness:?}");
    }
    let mut stdin = child.stdin.take().unwrap();
    let mut send = |line: &str| {
        stdin.write_all(line.as_bytes()).unwrap();
        stdin.write_all(b"\n").unwrap();
        stdin.flush().unwrap();
    };
    // Each answer arrives while stdin stays open: every message is flushed as
    // it is written.
    send(&request(&json!(1), "echo", &json!({"n": 1})));
    assert_eq!(
        next(),
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"n\":1}}\n"
    );
    send(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
    send("{oops");
    let refused: Value = serde_json::from_str(&next()).unwrap();
    assert_eq!(error_code(&refused, None), -32700);
    // A method name cannot forge a log line: its newline is logged escaped.
    send(&request(&json!("two"), "nope\nWARN forged", &json!({})));
    let missing: Value = serde_json::from_str(&next()).unwrap();
    assert_eq!(error_code(&missing, Some(&json!("two"))), -32601);
    // End of input: the server exits, and wrote nothing more.
    drop(stdin);
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    let status = child.wait().unwrap();
    reading.join().unwrap();
    assert!(status.success(), "{status}: {stderr}");
    let rest: Vec<String> = received
        .try_iter()
        .map(|l| String::from_utf8_lossy(&l).into_owned())
        .collect();
    assert!(rest.is_empty(), "more on stdout: {rest:?}");
    // The debug records went to stderr.
    for record in [
        "DEBUG duscope_mcp::transport: received request 1 echo",
        "DEBUG duscope_mcp::transport: sent the result for request 1",
        "DEBUG duscope_mcp::transport: received notification notifications/initialized",
        "WARN duscope_mcp::transport: refused a message: parse error",
        "DEBUG duscope_mcp::transport: received request \"two\" nope\\nWARN forged",
        "DEBUG duscope_mcp::transport: sent an error for request \"two\": method not found: nope\\nWARN forged",
        "DEBUG duscope_mcp::transport: the input ended",
    ] {
        assert!(
            stderr.contains(record),
            "no `{record}` in stderr:\n{stderr}"
        );
    }
    assert!(
        !stderr.lines().any(|line| line.starts_with("WARN forged")),
        "{stderr}"
    );
}

/// Logs every record at debug level and above to stderr, as `duscope mcp
/// --log-level debug` does.
struct Stderr;

impl log::Log for Stderr {
    fn enabled(&self, _: &log::Metadata<'_>) -> bool {
        true
    }

    fn log(&self, record: &log::Record<'_>) {
        let line = format!(
            "{} {}: {}\n",
            record.level(),
            record.target(),
            record.args()
        );
        io::stderr().write_all(line.as_bytes()).unwrap();
    }

    fn flush(&self) {}
}

#[test]
#[ignore = "run in a child process by p3_mcp_01_stdout_is_byte_clean_at_debug_level"]
fn p3_mcp_01_stdio_child_process() {
    // `--ignored` runs (the platform job) reach it without its parent.
    if std::env::var_os(CHILD_VAR).is_none() {
        return;
    }
    log::set_logger(&Stderr).unwrap();
    log::set_max_level(log::LevelFilter::Debug);
    io::stdout().write_all(STARTS.as_bytes()).unwrap();
    Transport::start(io::stdin(), io::stdout(), Echo::default())
        .unwrap()
        .wait()
        .unwrap();
    // The harness prints the test's result on stdout next: leave first.
    std::process::exit(0);
}
