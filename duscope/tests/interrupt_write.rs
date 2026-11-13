//! P5-CORE-07: Ctrl-C, and an MCP tool call's cancellation or deadline,
//! stop the index write that follows a scan's walk. The process exits 130
//! with `scan`'s one line on stderr, the output already written stands, and
//! neither the index nor its temporary file is left.
//!
//! `DUSCOPE_TEST_SLOW_WRITE_MS` holds the write, inside its first statement
//! long enough to reach the progress handler, until it is stopped or the
//! time is up: the tests wait for the temporary file, which the write
//! creates first, and stop it there.
//!
//! P5-CORE-09: `import`, which reads its tree from a file or stdin rather
//! than walking one, stops as `scan` does: Ctrl-C while it reads ends the
//! process at once, Ctrl-C after it stops the write, and Ctrl-C once the
//! index has its name leaves the save to finish.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fmt::Write as _;
use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, ExitStatus, Stdio};
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

/// An ncdu export of a root, `/srv/imported`, holding `files` files. Cut
/// after its last file when `whole` is false, the root's array and the
/// export's still open, as ncdu leaves its output while it writes it.
fn ncdu_export(files: usize, whole: bool) -> String {
    let mut export = String::from(
        "[1,2,{\"progname\":\"ncdu\",\"progver\":\"2.4\",\"timestamp\":2114380800},\n\
         [{\"name\":\"/srv/imported\",\"asize\":4096,\"dsize\":4096,\"mtime\":2114380000}",
    );
    for n in 0..files {
        write!(
            export,
            ",\n{{\"name\":\"file-with-a-longer-name-{n:06}\",\"asize\":1,\"dsize\":4096,\
             \"mtime\":2114370000}}"
        )
        .unwrap();
    }
    if whole {
        export.push_str("]]\n");
    }
    export
}

/// An export of `files` files, whole, in the work directory.
fn export_file(env: &TestEnv, files: usize) -> PathBuf {
    let file = env.work_dir().join("export.json");
    std::fs::write(&file, ncdu_export(files, true)).unwrap();
    file
}

/// How `child` exited, within [`PATIENCE`]; past it the child is killed
/// and the test fails.
fn exited(child: &mut Child) -> ExitStatus {
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if started.elapsed() > PATIENCE {
            let _ = child.kill();
            panic!("still running {PATIENCE:?} after the signal");
        }
        thread::sleep(Duration::from_millis(5));
    }
}

/// Ctrl-C while `import` reads its tree, from a pipe its producer still
/// holds open, so the read alone would never end: the process ends at once,
/// as during a scan's walk, and nothing has been written.
#[test]
fn p5_core_09_ctrl_c_while_import_reads_exits_130_and_leaves_nothing() {
    let env = TestEnv::new();
    let mut child = env
        .cmd()
        .args(["import", "-", "--json"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    // More than a pipe and the reader's buffers hold (64 KiB by default on
    // both systems, 1 MiB on a kernel with 64 KiB pages): once it is in,
    // the import has read most of it, and is reading.
    let export = ncdu_export(32_768, false);
    assert!(export.len() > 2 << 20, "{}", export.len());
    stdin.write_all(export.as_bytes()).unwrap();
    interrupt(&child);
    let status = exited(&mut child);
    drop(stdin);
    let mut stdout = Vec::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_end(&mut stdout)
        .unwrap();
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert_eq!(status.code(), Some(130), "{status}: {stderr}");
    // `scan`'s one line, and in --json mode no envelope.
    assert_eq!(stderr, "duscope: interrupted\n");
    assert_eq!(String::from_utf8_lossy(&stdout), "");
    assert_eq!(scans(&env), Vec::<String>::new(), "no .db, no .db.tmp-*");
}

/// Ctrl-C while `import` saves the tree it read: the write stops, and the
/// envelope already written stands, as for `scan --save`.
#[test]
fn p5_core_09_ctrl_c_during_import_s_write_exits_130_and_leaves_nothing() {
    let env = TestEnv::new();
    let file = export_file(&env, 300);
    let mut child = env
        .cmd()
        .arg("import")
        .arg(&file)
        .arg("--json")
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
    assert_eq!(out.status.code(), Some(130), "{}: {stderr}", out.status);
    assert!(took < PATIENCE, "exit took {took:?}");
    interrupted_alone(&stderr);
    let envelope: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(envelope["ok"], true, "{envelope}");
    assert_eq!(envelope["query"]["command"], "import", "{envelope}");
    assert_eq!(envelope["scan"]["file_count"], 300, "{envelope}");
    assert_eq!(scans(&env), Vec::<String>::new(), "no .db, no .db.tmp-*");
}

/// Waits until the write has given the index its name; the name.
fn wait_for_the_index(env: &TestEnv, child: &mut Child) -> String {
    let started = Instant::now();
    loop {
        let names = scans(env);
        // `<id>.db`; a temporary file's extension is `tmp-<pid>`.
        if let Some(name) = names
            .iter()
            .find(|name| Path::new(name).extension().is_some_and(|ext| ext == "db"))
        {
            return name.clone();
        }
        if let Some(status) = child.try_wait().unwrap() {
            panic!("the process ended before its index had its name: {status}");
        }
        assert!(started.elapsed() < PATIENCE, "no index: {names:?}");
        thread::sleep(Duration::from_millis(5));
    }
}

/// Fills `stderr`, the process's stderr, with empty lines until a write
/// would block, whatever it holds: in blocks, then a byte at a time, so
/// that no line of the process fits. Non-blocking only meanwhile, since the
/// process shares the flag; it writes nothing while its write is held.
fn fill(stderr: &UnixStream) {
    stderr.set_nonblocking(true).unwrap();
    let mut writer = stderr;
    for block in [&[b'\n'; 4096][..], b"\n"] {
        loop {
            match writer.write(block) {
                Ok(_) => {}
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) => panic!("cannot fill stderr: {e}"),
            }
        }
    }
    stderr.set_nonblocking(false).unwrap();
}

/// Ctrl-C once the import's index has its name: the save is done, and
/// finishes. The process is held after the rename, where it says `saved as`
/// (outside JSON) on a stderr the test filled while the write was held: a
/// socket, which says when it is full, rather than a pipe, whose size
/// differs between systems and page sizes.
#[test]
fn p5_core_09_ctrl_c_after_import_s_rename_keeps_the_scan() {
    let env = TestEnv::new();
    let file = export_file(&env, 300);
    let (mut stderr, writer) = UnixStream::pair().unwrap();
    let mut child = env
        .cmd()
        .arg("import")
        .arg(&file)
        .arg("--plain")
        .env("DUSCOPE_TEST_SLOW_WRITE_MS", "2000")
        .stdout(Stdio::null())
        .stderr(OwnedFd::from(writer.try_clone().unwrap()))
        .spawn()
        .unwrap();
    wait_for_the_write(&env, &mut child);
    fill(&writer);
    drop(writer);
    assert!(
        scans(&env).iter().all(|name| name.contains(".db.tmp-")),
        "the hold ended while stderr was filled: {:?}",
        scans(&env)
    );
    let saved = wait_for_the_index(&env, &mut child);
    thread::sleep(Duration::from_millis(100));
    assert!(
        child.try_wait().unwrap().is_none(),
        "the process ended before the signal: it was not held after the rename"
    );
    interrupt(&child);
    let mut text = Vec::new();
    stderr.read_to_end(&mut text).unwrap();
    let status = child.wait().unwrap();
    let text = String::from_utf8_lossy(&text);
    let lines: Vec<&str> = text.lines().filter(|line| !line.is_empty()).collect();
    assert_eq!(status.code(), Some(0), "{status}: {lines:#?}");
    let id = saved.strip_suffix(".db").unwrap();
    assert!(
        lines
            .last()
            .is_some_and(|line| line.starts_with(&format!("saved as {id} ("))),
        "{lines:#?}"
    );
    assert!(!text.contains("interrupted"), "{lines:#?}");
    assert_eq!(scans(&env), vec![saved.clone()]);
    // The saved scan is whole: `scans` reads it.
    let out = env.cmd().args(["scans", "--json"]).output().unwrap();
    let listed: Value = serde_json::from_slice(&out.stdout).unwrap();
    let entries = listed["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1, "{listed}");
    assert_eq!(entries[0]["id"], id, "{listed}");
    assert_eq!(entries[0]["source"], "import:ncdu", "{listed}");
    assert_eq!(entries[0]["file_count"], 300, "{listed}");
}
