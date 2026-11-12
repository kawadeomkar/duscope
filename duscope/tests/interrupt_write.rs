//! P5-CORE-07: Ctrl-C, and an MCP tool call's cancellation or deadline,
//! stop the index write that follows a scan's walk. The process exits 130
//! with `scan`'s one line on stderr, the output already written stands, and
//! neither the index nor its temporary file is left.
//!
//! `DUSCOPE_TEST_SLOW_WRITE_MS` holds the write, inside its first statement
//! long enough to reach the progress handler, until it is stopped or the
//! time is up: the tests wait for the temporary file, which the write
//! creates first, and stop it there.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use duscope_testkit::TestEnv;
use serde_json::{Value, json};

/// How long a test waits for a state before it fails.
const PATIENCE: Duration = Duration::from_secs(30);

/// How long the slowed write would hold: far longer than a stopped one
/// takes to end.
const HOLD_MS: &str = "120000";

/// A tree of `files` files under the work directory, canonical: rows
/// enough for the write's statements to reach its progress handler.
fn tree(env: &TestEnv, files: usize) -> PathBuf {
    let root = env.work_dir().canonicalize().unwrap().join("tree");
    for dir in 0..files.div_ceil(100) {
        let dir = root.join(format!("d{dir:02}"));
        std::fs::create_dir_all(&dir).unwrap();
    }
    for n in 0..files {
        let name = format!("d{:02}/file-with-a-longer-name-{n:05}", n / 100);
        std::fs::write(root.join(name), b"x").unwrap();
    }
    root
}

/// The names under the index directory's `scans`, sorted.
fn scans(env: &TestEnv) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(env.index_dir().join("scans"))
        .map(|entries| {
            entries
                .map(|e| e.unwrap().file_name().into_string().unwrap())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// Waits until the write has started: its temporary file is there.
fn wait_for_the_write(env: &TestEnv, child: &mut Child) {
    let started = Instant::now();
    while !scans(env).iter().any(|name| name.contains(".db.tmp-")) {
        if let Some(status) = child.try_wait().unwrap() {
            panic!("the process ended before its write began: {status}");
        }
        assert!(
            started.elapsed() < PATIENCE,
            "no temporary file: {:?}",
            scans(env)
        );
        thread::sleep(Duration::from_millis(5));
    }
}

fn interrupt(child: &Child) {
    let sent = Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(sent.success());
}

#[test]
fn p5_core_07_ctrl_c_during_the_write_exits_130_and_leaves_nothing() {
    let env = TestEnv::new();
    let root = tree(&env, 300);
    let mut child = env
        .cmd()
        .arg("scan")
        .arg(&root)
        .args(["--save", "--json"])
        .env("DUSCOPE_TEST_SLOW_WRITE_MS", HOLD_MS)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_for_the_write(&env, &mut child);
    interrupt(&child);
    let signalled = Instant::now();
    let out = child.wait_with_output().unwrap();
    let took = signalled.elapsed();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(130), "{stderr}");
    // Stopped where it was held, not after the hold.
    assert!(took < PATIENCE, "exit took {took:?}");
    interrupted_alone(&stderr);
    // The output stands, as for a write that fails: it was out before the
    // write began.
    let envelope: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(envelope["ok"], true, "{envelope}");
    assert_eq!(envelope["scan"]["file_count"], 300);
    assert_eq!(scans(&env), Vec::<String>::new(), "no .db, no .db.tmp-*");
}

/// What stderr holds after a write stopped by Ctrl-C: the footer the
/// output wrote for people (where a verb names the id it is saving, the id
/// names no file, as after a write that failed), then `scan`'s one line, and
/// nothing else of the write.
fn interrupted_alone(stderr: &str) {
    assert_eq!(
        stderr.lines().last(),
        Some("duscope: interrupted"),
        "{stderr}"
    );
    assert_eq!(stderr.matches("interrupted").count(), 1, "{stderr}");
    assert!(!stderr.contains("cannot write"), "{stderr}");
}

/// Ctrl-C once the walk is over and before the write starts: here, while
/// the output of `top` on a path no scan has saved, which scans it and saves
/// the scan after its rows, waits on a full pipe. The write never starts.
#[test]
fn p5_core_07_ctrl_c_before_the_write_exits_130_and_leaves_nothing() {
    let env = TestEnv::new();
    let root = tree(&env, 3_000);
    let mut child = env
        .cmd()
        .args(["top", "--files", "--all", "--format", "ndjson"])
        .arg(&root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    // The header: the walk is over and the output under way. Its rows
    // (about a megabyte) fill the pipe, and the process waits there.
    let mut header = String::new();
    stdout.read_line(&mut header).unwrap();
    assert!(header.starts_with("{\"type\":\"scan\""), "{header}");
    thread::sleep(Duration::from_millis(200));
    assert!(scans(&env).is_empty(), "the write began: {:?}", scans(&env));
    interrupt(&child);
    let mut rest = Vec::new();
    stdout.read_to_end(&mut rest).unwrap();
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(130), "{stderr}");
    interrupted_alone(&stderr);
    // The output was written whole: every row, then the trailer.
    let lines: Vec<Value> = String::from_utf8(rest)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let trailer = lines.last().unwrap();
    assert_eq!(trailer["type"], "end", "{trailer}");
    assert_eq!(trailer["returned_count"], 3_000, "{trailer}");
    assert_eq!(scans(&env), Vec::<String>::new(), "no .db, no .db.tmp-*");
}

/// `duscope mcp` with its write slowed, and what it wrote, line by line.
struct Server {
    child: Child,
    stdin: std::process::ChildStdin,
    lines: mpsc::Receiver<Value>,
}

impl Server {
    fn start(env: &TestEnv, root: &Path, vars: &[(&str, &str)]) -> Server {
        let mut child = env
            .cmd()
            .arg("mcp")
            .arg("--roots")
            .arg(root)
            .env("DUSCOPE_TEST_SLOW_WRITE_MS", HOLD_MS)
            .envs(vars.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout: ChildStdout = child.stdout.take().unwrap();
        let (sent, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { return };
                if sent.send(serde_json::from_str(&line).unwrap()).is_err() {
                    return;
                }
            }
        });
        Server {
            child,
            stdin,
            lines,
        }
    }

    fn send(&mut self, message: &Value) {
        writeln!(self.stdin, "{message}").unwrap();
    }

    /// The response to request `id`, and the messages before it.
    fn response(&self, id: u64) -> (Value, Vec<Value>) {
        let mut before = Vec::new();
        loop {
            let message = self
                .lines
                .recv_timeout(PATIENCE)
                .unwrap_or_else(|e| panic!("no response to {id} ({e}): {before:#?}"));
            if message["id"] == id {
                return (message, before);
            }
            before.push(message);
        }
    }

    /// Closes the server's input and waits for it to exit 0.
    fn finish(mut self) {
        drop(self.stdin);
        let status = self.child.wait().unwrap();
        assert_eq!(status.code(), Some(0));
    }
}

/// A 2026-07-28 tool call.
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

/// `duscope_scans`, which runs once the call before it has returned: what
/// it lists.
fn listed(server: &mut Server, id: u64) -> (Value, Vec<Value>) {
    server.send(&call(id, "duscope_scans", &json!({})));
    let (response, before) = server.response(id);
    (
        response["result"]["structuredContent"]["entries"].clone(),
        before,
    )
}

#[test]
fn p5_core_07_a_tool_call_cancelled_during_its_write_saves_nothing() {
    let env = TestEnv::new();
    let root = tree(&env, 300);
    let mut server = Server::start(&env, &root, &[]);
    server.send(&call(1, "duscope_scan", &json!({"root": root})));
    wait_for_the_write(&env, &mut server.child);
    server.send(&json!({
        "jsonrpc": "2.0",
        "method": "notifications/cancelled",
        "params": {"requestId": 1},
    }));
    let cancelled = Instant::now();
    let (entries, before) = listed(&mut server, 2);
    // The cancelled call is owed no response, and got none; the next call
    // ran once the scan's verb had returned, long before the write's hold.
    assert!(
        before.iter().all(|message| message["id"] != 1),
        "{before:#?}"
    );
    assert!(cancelled.elapsed() < PATIENCE, "{:?}", cancelled.elapsed());
    assert_eq!(entries, json!([]));
    assert_eq!(scans(&env), Vec::<String>::new(), "no .db, no .db.tmp-*");
    server.finish();
}

#[test]
fn p5_core_07_a_tool_call_past_its_deadline_during_its_write_saves_nothing() {
    let env = TestEnv::new();
    let root = tree(&env, 300);
    let mut server = Server::start(&env, &root, &[("DUSCOPE_TEST_MCP_DEADLINE_MS", "5000")]);
    server.send(&call(1, "duscope_scan", &json!({"root": root})));
    wait_for_the_write(&env, &mut server.child);
    // Answered at the deadline, while the write is held.
    let (response, _) = server.response(1);
    let result = &response["result"];
    assert_eq!(result["isError"], true, "{response}");
    assert_eq!(
        result["structuredContent"]["error"]["code"], "deadline_exceeded",
        "{response}"
    );
    let (entries, _) = listed(&mut server, 2);
    assert_eq!(entries, json!([]));
    assert_eq!(scans(&env), Vec::<String>::new(), "no .db, no .db.tmp-*");
    server.finish();
}
