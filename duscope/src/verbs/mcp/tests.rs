//! P3-MCP-05: a scan a tool call runs, in this process as the server runs
//! it: it counts its entries into the call's count, which the server reports
//! as progress, and stops on the call's flag, which a cancellation or the
//! deadline sets, saving nothing. And a walk cancelled through the server,
//! over its stdio transport, stops within one directory per walk worker.

use std::ffi::OsString;
use std::io::{self, BufRead, BufReader, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use duscope_mcp::discover::ServerConfig;
use duscope_mcp::roots::{Refusal, Roots};
use duscope_mcp::server::{Server, Tools};
use duscope_mcp::tools::{Annotations, Context, Invocation, Outcome, Registry, Runner, Tool};
use duscope_mcp::transport::Transport;
use duscope_mcp::worker::{CallSignals, Timing};
use duscope_testkit::TestEnv;
use duscope_walk::{
    DirHandle, Entry, MemProvider, Provider, ScanError, ScanEvent, VolumeInfo, WalkError,
    WalkOptions, walk_with,
};
use serde_json::{Map, Value, json};

use super::VerbRunner;

/// How long a test waits for a message or a state before it fails.
const PATIENCE: Duration = Duration::from_secs(10);

/// The runner the server builds, with the test's index directory and no
/// configuration file.
fn runner(env: &TestEnv) -> VerbRunner {
    VerbRunner {
        index_dir: env.index_dir(),
        max_chars: None,
        config: Some("none".into()),
    }
}

/// `scan --save ROOT`, as `duscope_scan` runs it.
fn scan(root: &Path) -> Invocation {
    Invocation {
        verb: "scan",
        args: vec![OsString::from("--save"), root.into()],
    }
}

/// A tree of 3 directories and 12 files under the test's work directory.
fn tree(env: &TestEnv) -> std::path::PathBuf {
    let root = env.work_dir().canonicalize().unwrap().join("tree");
    for dir in ["a", "b", "b/c"] {
        std::fs::create_dir_all(root.join(dir)).unwrap();
        for file in 0..4 {
            std::fs::write(root.join(dir).join(format!("f{file}")), b"x").unwrap();
        }
    }
    root
}

/// The files in the index directory's `scans`: the saved scans, and any
/// index left half-written.
fn saved(env: &TestEnv) -> usize {
    std::fs::read_dir(env.index_dir().join("scans"))
        .map_or(0, |entries| entries.filter_map(Result::ok).count())
}

#[test]
fn p3_mcp_05_a_tool_calls_scan_counts_its_entries_for_progress() {
    let env = TestEnv::new();
    let root = tree(&env);
    let call = CallSignals::default();
    let outcome = runner(&env).run_call(&scan(&root), &call);
    assert!(outcome.ok, "{}", outcome.json);
    let envelope: Value = serde_json::from_str(&outcome.json).unwrap();
    let scan = &envelope["scan"];
    // The root, its 3 directories and 12 files.
    assert_eq!(scan["file_count"], 12);
    assert_eq!(scan["dir_count"], 4);
    assert_eq!(call.entries.load(Ordering::Relaxed), 16);
    assert_eq!(saved(&env), 1);
    // The call's signals are set for its run alone.
    assert_eq!(
        super::running_call().map(|c| c.entries.load(Ordering::Relaxed)),
        None
    );
}

#[test]
fn p3_mcp_05_a_cancelled_tool_calls_scan_stops_and_saves_nothing() {
    let env = TestEnv::new();
    let root = tree(&env);
    let call = CallSignals::default();
    call.cancel.store(true, Ordering::Relaxed);
    let outcome = runner(&env).run_call(&scan(&root), &call);
    assert!(!outcome.ok);
    // The walk stops at its first directory: only the root was read.
    assert_eq!(call.entries.load(Ordering::Relaxed), 1);
    assert_eq!(saved(&env), 0);
    // The verb's envelope, as for any other failure, where a shell gets one
    // line on stderr.
    let envelope: Value = serde_json::from_str(&outcome.json).unwrap();
    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["error"]["code"], "interrupted");
}

/// How many directories the held walk reads before it is held.
const OPEN_READS: usize = 10;

/// A tool whose verb, `name`, the walk runner runs.
struct FakeTool(&'static str);

impl Tool for FakeTool {
    fn name(&self) -> &'static str {
        self.0
    }

    fn description(&self) -> &'static str {
        "A fake verb (a test fixture)."
    }

    fn input_schema(&self) -> &'static str {
        r#"{"type":"object","additionalProperties":false}"#
    }

    fn annotations(&self) -> Annotations {
        Annotations {
            read_only: true,
            destructive: false,
            idempotent: true,
        }
    }

    fn invocation(&self, _: &Map<String, Value>, _: &Context<'_>) -> Result<Invocation, Refusal> {
        Ok(Invocation {
            verb: self.0,
            args: Vec::new(),
        })
    }

    fn summary(&self, _: &Value) -> String {
        "done".to_owned()
    }
}

/// How the held walk ended: whether it was cancelled, and how many
/// directories it read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Walked {
    cancelled: bool,
    reads: usize,
}

/// Runs `test_scan`, a walk of a 129-directory tree with duscope-walk, the
/// call's flag as its own and its entries counted into the call's count,
/// held after its first reads; and `test_echo`, which returns at once.
#[derive(Clone)]
struct WalkRunner {
    tree: Arc<MemProvider>,
    threads: usize,
    /// The directories the held walk has begun to read.
    reads: Arc<AtomicUsize>,
    walked: Arc<Mutex<Option<Walked>>>,
}

impl Runner for WalkRunner {
    fn run(&self, invocation: &Invocation) -> Outcome {
        self.run_call(invocation, &CallSignals::default())
    }

    fn run_call(&self, invocation: &Invocation, call: &CallSignals) -> Outcome {
        if invocation.verb == "test_scan" {
            let provider = Held {
                inner: &self.tree,
                reads: Arc::clone(&self.reads),
                stop: Arc::clone(&call.cancel),
            };
            let opts = WalkOptions {
                threads: self.threads,
                cancel: Arc::clone(&call.cancel),
                ..WalkOptions::default()
            };
            let result = walk_with(&provider, self.tree.root_path(), &opts, |event| {
                if matches!(event, ScanEvent::Entry(_)) {
                    call.entries.fetch_add(1, Ordering::Relaxed);
                }
            });
            *self.walked.lock().unwrap() = Some(Walked {
                cancelled: matches!(result, Err(WalkError::Cancelled)),
                reads: provider.reads.load(Ordering::Relaxed),
            });
        }
        Outcome {
            json: json!({"ok": true}).to_string(),
            ok: true,
        }
    }

    fn refuse(&self, refusal: &Refusal) -> Outcome {
        Outcome {
            json: json!({"ok": false, "error": {"code": refusal.code}}).to_string(),
            ok: false,
        }
    }
}

/// The tree, read as `MemProvider` reads it, but every directory after the
/// first `OPEN_READS` is held until `stop` is set (for `PATIENCE` at most,
/// so that a broken test fails instead of hanging).
struct Held<'a> {
    inner: &'a MemProvider,
    reads: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
}

impl Provider for Held<'_> {
    fn read_dir(
        &self,
        dir: &DirHandle,
        opts: &WalkOptions,
        out: &mut Vec<Entry>,
        errs: &mut Vec<ScanError>,
    ) -> io::Result<()> {
        if self.reads.fetch_add(1, Ordering::Relaxed) >= OPEN_READS {
            let held = Instant::now();
            while !self.stop.load(Ordering::Relaxed) && held.elapsed() < PATIENCE {
                thread::sleep(Duration::from_millis(1));
            }
        }
        self.inner.read_dir(dir, opts, out, errs)
    }

    fn volume(&self, path: &Path) -> io::Result<VolumeInfo> {
        self.inner.volume(path)
    }

    fn root(&self, path: &Path, opts: &WalkOptions) -> io::Result<Entry> {
        self.inner.root(path, opts)
    }
}

/// A 2026-07-28 `tools/call` of `tool`, asking for progress under `token`
/// when there is one.
fn call(id: u64, tool: &str, token: Option<&str>) -> Value {
    let mut meta = json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientCapabilities": {},
    });
    if let Some(token) = token {
        meta["progressToken"] = json!(token);
    }
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": {"name": tool, "_meta": meta},
    })
}

/// Has a client call `test_scan` with `threads` walk workers, wait for the
/// scan's first progress, cancel the call, and call `test_echo`, which the
/// worker runs once the walk has stopped. How the walk ended.
fn cancel_a_walk(threads: usize) -> Walked {
    let env = TestEnv::new();
    let mut tree = MemProvider::new("/mem");
    for d in 0..64 {
        for f in 0..8 {
            tree.file(format!("d{d}/e{}/f{f}", d % 4), 4096);
        }
    }
    let runner = WalkRunner {
        tree: Arc::new(tree),
        threads,
        reads: Arc::default(),
        walked: Arc::default(),
    };
    let mut registry = Registry::new(24_000, false);
    registry.register(Box::new(FakeTool("test_scan"))).unwrap();
    registry.register(Box::new(FakeTool("test_echo"))).unwrap();
    let server = Server::new(ServerConfig::new("test"))
        .with_tools(Tools {
            registry,
            roots: Roots::new([env.work_dir()]).unwrap(),
            index_dir: env.index_dir(),
            runner: Box::new(runner.clone()),
        })
        .with_timing(Timing {
            progress_every: Duration::from_millis(20),
            deadline: None,
        });
    let (server_in, mut input) = io::pipe().unwrap();
    let (output, server_out) = io::pipe().unwrap();
    let transport = Transport::start(server_in, server_out, server).unwrap();
    let (sent, messages) = mpsc::channel::<Value>();
    let reader = thread::spawn(move || {
        for line in BufReader::new(output).lines() {
            let Ok(line) = line else { break };
            sent.send(serde_json::from_str(&line).unwrap()).unwrap();
        }
    });
    let next = || {
        messages
            .recv_timeout(PATIENCE)
            .expect("the server wrote nothing")
    };
    writeln!(input, "{}", call(1, "test_scan", Some("walk"))).unwrap();
    let progress = next();
    assert_eq!(progress["method"], "notifications/progress", "{progress}");
    assert_eq!(progress["params"]["progressToken"], "walk");
    assert!(progress["params"]["progress"].as_u64().unwrap() >= 1);
    // That progress can be the root's own entry, counted before any
    // directory is read: the cancellation has to find the walk at its hold.
    let started = Instant::now();
    while runner.reads.load(Ordering::Relaxed) <= OPEN_READS {
        assert!(
            started.elapsed() < PATIENCE,
            "the walk never reached its hold"
        );
        thread::sleep(Duration::from_millis(1));
    }
    writeln!(
        input,
        "{}",
        json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": 1}})
    )
    .unwrap();
    let cancelled = Instant::now();
    writeln!(input, "{}", call(2, "test_echo", None)).unwrap();
    // Progress that left before the cancellation may come first; never a
    // response to the cancelled call.
    loop {
        let message = next();
        assert_ne!(message["id"], 1, "the cancelled call was answered");
        if message["id"] == 2 {
            break;
        }
        assert_eq!(message["method"], "notifications/progress", "{message}");
    }
    // Held until the cancellation reached it: had the walk waited out its
    // patience instead, the echo would have waited ten seconds.
    assert!(
        cancelled.elapsed() < PATIENCE / 2,
        "{:?}",
        cancelled.elapsed()
    );
    drop(input);
    transport.wait().unwrap();
    reader.join().unwrap();
    assert!(messages.iter().all(|message| message["id"] != 1));
    runner.walked.lock().unwrap().unwrap()
}

#[test]
fn p3_mcp_05_a_cancelled_scan_returns_within_one_directory_per_worker() {
    for threads in [1, 4, 16] {
        let walked = cancel_a_walk(threads);
        assert!(walked.cancelled, "{threads} workers: {walked:?}");
        // The reads before the hold, then at most the one directory each
        // worker had started; none starts after the cancellation.
        assert!(
            walked.reads <= OPEN_READS + threads,
            "{threads} workers: {walked:?}"
        );
        assert!(
            walked.reads > OPEN_READS,
            "the hold was reached: {walked:?}"
        );
    }
}

/// P5-CORE-07: a tool call's scan stopped once its write has begun (its
/// temporary file is there; a thread watching for it sets the call's flag)
/// saves nothing, and its verb's answer is the error envelope alone, never
/// the scan's envelope with the error after it. A write that wins the race
/// and finishes first is tried again, in a directory of its own.
#[test]
fn p5_core_07_a_tool_calls_scan_stopped_during_its_write_answers_interrupted_alone() {
    let env = TestEnv::new();
    let root = env.work_dir().canonicalize().unwrap().join("wide");
    for n in 0..3_000 {
        let dir = root.join(format!("d{}", n / 100));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("f{n}")), b"x").unwrap();
    }
    for attempt in 0..5 {
        let index_dir = env.index_dir().join(format!("try{attempt}"));
        let call = CallSignals::default();
        let watching = {
            let scans = index_dir.join("scans");
            let cancel = Arc::clone(&call.cancel);
            thread::spawn(move || {
                let started = Instant::now();
                while started.elapsed() < PATIENCE {
                    let writing = std::fs::read_dir(&scans).is_ok_and(|mut names| {
                        names.any(|e| {
                            e.is_ok_and(|e| e.file_name().to_string_lossy().contains(".db.tmp-"))
                        })
                    });
                    if writing {
                        cancel.store(true, Ordering::Relaxed);
                        return;
                    }
                }
            })
        };
        let runner = VerbRunner {
            index_dir: index_dir.clone(),
            ..runner(&env)
        };
        let outcome = runner.run_call(&scan(&root), &call);
        watching.join().unwrap();
        if outcome.ok {
            eprintln!("the write finished before the flag was set; trying again");
            continue;
        }
        let envelope: Value = serde_json::from_str(&outcome.json).unwrap_or_else(|e| {
            let lines: Vec<String> = outcome
                .json
                .lines()
                .map(|l| l.chars().take(80).collect())
                .collect();
            panic!("{e}: not one JSON value: {lines:#?}")
        });
        assert_eq!(envelope["ok"], false, "{envelope}");
        assert!(envelope.get("scan").is_none(), "a scan id in {envelope}");
        assert_eq!(envelope["error"]["code"], "interrupted", "{envelope}");
        // The walk was whole: it read every entry before the write began.
        assert_eq!(call.entries.load(Ordering::Relaxed), 3_031);
        let left: Vec<_> = std::fs::read_dir(index_dir.join("scans"))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert!(left.is_empty(), "no .db, no .db.tmp-*: {left:?}");
        return;
    }
    panic!("every write finished before its flag was set");
}
