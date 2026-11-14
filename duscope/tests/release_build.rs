//! P5-SEC-06: a release build ignores the test variables. The binary reads
//! each knob behind a guard of its own, `cfg!(debug_assertions)` or
//! `#[cfg(debug_assertions)]`, which neither the release nor the `dist`
//! profile turns on, so nothing but this test holds them all off. It runs
//! the release build (`just platform-tests`, CI's platform job) with every
//! knob set and checks what a test can see without a terminal: a save is
//! written whole, at once and under the real clock; `stale` under a clock
//! in 2099 lists what the real clock does; the Full Disk Access probe is not
//! mocked; and `mcp` offers no fixture tool, keeps no shortened deadline and
//! sends no early progress. `DUSCOPE_TEST_PANIC`, `DUSCOPE_TEST_OPENER` and
//! `DUSCOPE_TEST_PLAN_GATE` act in the viewer or after a panic, and
//! `DUSCOPE_TEST_TRASH` on a real removal: they are set, not observed. The
//! knobs are held to the names the binary's sources hold, so a new one
//! cannot be left out.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use duscope_testkit::TestEnv;
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};

/// The clock `DUSCOPE_TEST_NOW` sets in a debug build: far past the real one.
const FAR_FUTURE: &str = "2099-01-01T00:00:00Z";

/// How long `DUSCOPE_TEST_SLOW_WRITE_MS` holds a debug build's index write.
const SLOW_WRITE_MS: u64 = 60_000;

/// The names the binary's crates hold in their test modules alone, outside
/// the files named `tests.rs`: duscope-walk's descriptor tests name a
/// directory to the copy of themselves they start.
const IN_TEST_MODULES: [&str; 2] = ["DUSCOPE_TEST_EMFILE_DIR", "DUSCOPE_TEST_KEEP_DIR"];

/// The source directories of the binary and of the crates it is built from.
const SOURCES: [&str; 6] = [
    "duscope/src",
    "crates/duscope-core/src",
    "crates/duscope-walk/src",
    "crates/duscope-treemap/src",
    "crates/duscope-tui/src",
    "crates/duscope-mcp/src",
];

/// Every test variable the binary reads, each with a value a debug build
/// acts on.
fn knobs(env: &TestEnv) -> Vec<(&'static str, String)> {
    let file = |name: &str| env.tmp_dir().join(name).display().to_string();
    vec![
        // The clock every verb reads (duscope-core's `clock::now`).
        ("DUSCOPE_TEST_NOW", FAR_FUTURE.to_owned()),
        // The index writer ends its own process before the rename.
        ("DUSCOPE_TEST_KILL_BEFORE_RENAME", "1".to_owned()),
        // The index write's first long statement waits.
        ("DUSCOPE_TEST_SLOW_WRITE_MS", SLOW_WRITE_MS.to_string()),
        // The Full Disk Access probe meets macOS's refusal, on any platform.
        ("DUSCOPE_TEST_TCC", "denied".to_owned()),
        // `clean`'s Trash takes only what a Trash of the test's own can.
        ("DUSCOPE_TEST_TRASH", "guard".to_owned()),
        // `mcp` adds its fixture tools, `test_panic` among them.
        ("DUSCOPE_TEST_MCP_TOOLS", "panic".to_owned()),
        // `mcp` reports progress every millisecond, and ends every call at
        // once.
        ("DUSCOPE_TEST_MCP_PROGRESS_MS", "1".to_owned()),
        ("DUSCOPE_TEST_MCP_DEADLINE_MS", "0".to_owned()),
        // A panic ends the process with 134 (`1`: the viewer panics after
        // its first frame).
        ("DUSCOPE_TEST_PANIC", "abort".to_owned()),
        // The mark pane's plan waits for the file; the viewer's `o` writes
        // its commands to the other.
        ("DUSCOPE_TEST_PLAN_GATE", file("gate")),
        ("DUSCOPE_TEST_OPENER", file("opened")),
    ]
}

/// The `DUSCOPE_TEST_*` names in [`SOURCES`], outside their `tests.rs`
/// files and `tests` directories.
fn names_in_sources() -> BTreeSet<String> {
    let top = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut dirs: Vec<PathBuf> = SOURCES.iter().map(|dir| top.join(dir)).collect();
    let mut names = BTreeSet::new();
    while let Some(dir) = dirs.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                if !path.ends_with("tests") {
                    dirs.push(path);
                }
                continue;
            }
            if path.ends_with("tests.rs") || path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let text = fs::read_to_string(&path).unwrap();
            for (at, _) in text.match_indices("DUSCOPE_TEST_") {
                let name: String = text[at..]
                    .chars()
                    .take_while(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == '_')
                    .collect();
                let name = name.trim_end_matches('_');
                if name.len() > "DUSCOPE_TEST".len() {
                    names.insert(name.to_owned());
                }
            }
        }
    }
    names
}

/// Unix nanoseconds now, on the real clock.
fn real_now() -> i64 {
    let since = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    i64::try_from(since.as_nanos()).unwrap()
}

/// `duscope ARGS --json` with every knob set: its envelope, the command
/// having succeeded.
fn json(env: &TestEnv, args: &[&std::ffi::OsStr]) -> Value {
    let out = with_knobs(env, env.cmd().args(args).arg("--json"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{args:?}: {:?} {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

fn with_knobs<'c>(env: &TestEnv, cmd: &'c mut Command) -> &'c mut Command {
    cmd.envs(knobs(env))
}

/// The codes of an envelope's warnings.
fn codes(envelope: &Value) -> Vec<&str> {
    envelope["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["code"].as_str().unwrap())
        .collect()
}

/// A 2026-07-28 request.
fn request(id: u64, method: &str, params: &Value) -> Value {
    let mut all = json!({
        "_meta": {
            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
            "io.modelcontextprotocol/clientCapabilities": {},
        },
    });
    all.as_object_mut()
        .unwrap()
        .extend(params.as_object().unwrap().clone());
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": all})
}

/// A tree whose index write runs the long statement the slow write waits
/// in (forty files or more), under the home, which a scan that holds it
/// probes for Full Disk Access.
fn tree(env: &TestEnv) -> PathBuf {
    let home = env.home();
    for dir in 0..40 {
        let dir = home.join("tree").join(format!("d{dir}"));
        fs::create_dir_all(&dir).unwrap();
        for file in 0..50 {
            fs::write(dir.join(format!("f{file}")), b"x").unwrap();
        }
    }
    home
}

/// `scan --save` of `home`: written whole (no kill before the rename), at
/// once (no wait in the write), on the real clock, and with no refusal
/// mocked. The saved scan's id.
fn saves_on_the_real_clock(env: &TestEnv, home: &Path) -> String {
    let before = real_now();
    let started = Instant::now();
    let saved = json(env, &["scan".as_ref(), home.as_os_str(), "--save".as_ref()]);
    let took = started.elapsed();
    let after = real_now();
    assert!(
        took < Duration::from_millis(SLOW_WRITE_MS / 3),
        "the save took {took:?}"
    );
    assert!(
        !codes(&saved).contains(&"full_disk_access_missing"),
        "{saved}"
    );
    let id = saved["scan"]["id"].as_str().unwrap().to_owned();
    let scans = env.index_dir().join("scans");
    let names: Vec<String> = fs::read_dir(&scans)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(names, [format!("{id}.db")]);
    let index =
        Connection::open_with_flags(scans.join(&names[0]), OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let started_at: i64 = index
        .query_row(
            "SELECT value FROM scan_meta WHERE key = 'started_at'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let slack = 2_000_000_000;
    assert!(
        (before - slack..=after + slack).contains(&started_at),
        "started_at {started_at}, not between {before} and {after}"
    );
    id
}

/// `stale` of the saved scan `id` judges ages on the real clock: the files
/// written a moment ago are not 180 days old, and the scan is not old.
fn stale_reads_the_real_clock(env: &TestEnv, home: &Path, id: &str) {
    let stale = json(
        env,
        &[
            "stale".as_ref(),
            "--scan".as_ref(),
            id.as_ref(),
            "--min-size".as_ref(),
            "0".as_ref(),
            home.as_os_str(),
        ],
    );
    assert_eq!(stale["entries"], json!([]), "{stale}");
    for old in ["scan_age", "stale_index"] {
        assert!(!codes(&stale).contains(&old), "{stale}");
    }
}

/// `mcp` serving `home`: the shipped tools alone, a scan that runs to its
/// end, and no progress before two seconds have passed.
fn mcp_keeps_its_tools_and_times(env: &TestEnv, home: &Path) {
    let mut scan = request(
        2,
        "tools/call",
        &json!({"name": "duscope_scan", "arguments": {"root": home}}),
    );
    scan["params"]["_meta"]["progressToken"] = json!("scan-2");
    let started = Instant::now();
    let mut child = with_knobs(env, env.cmd().arg("mcp").arg("--roots").arg(home))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    for message in [request(1, "tools/list", &json!({})), scan] {
        writeln!(stdin, "{message}").unwrap();
    }
    drop(stdin);
    let served = child.wait_with_output().unwrap();
    let took = started.elapsed();
    let stderr = String::from_utf8_lossy(&served.stderr);
    assert!(served.status.success(), "{:?} {stderr}", served.status);
    let messages: Vec<Value> = String::from_utf8(served.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let answer = |id: u64| {
        messages
            .iter()
            .find(|m| m["id"] == id)
            .unwrap_or_else(|| panic!("no answer to {id}: {messages:#?} {stderr}"))
    };
    let tools: Vec<&str> = answer(1)["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert!(
        !tools.is_empty() && tools.iter().all(|name| name.starts_with("duscope_")),
        "{tools:?}"
    );
    let result = &answer(2)["result"];
    assert_eq!(result["isError"], false, "{result}");
    let progress = messages
        .iter()
        .filter(|m| m["method"] == "notifications/progress")
        .count();
    assert!(
        u64::try_from(progress).unwrap() <= took.as_secs() / 2,
        "{progress} progress notifications in {took:?}"
    );
}

#[test]
#[ignore = "runs the release build; just platform-tests, DUSCOPE_TEST_PLATFORM=1"]
fn p5_sec_06_a_release_build_ignores_the_test_variables() {
    if std::env::var_os("DUSCOPE_TEST_PLATFORM").is_none_or(|v| v != "1") {
        eprintln!("skipped: set DUSCOPE_TEST_PLATFORM=1");
        return;
    }
    if cfg!(debug_assertions) {
        duscope_testkit::skip(
            "a debug build honours the test variables: run this in release, as just \
             platform-tests does",
        );
        return;
    }
    let env = TestEnv::new();
    // Every knob the binary reads is set, and no other.
    let set: BTreeSet<String> = knobs(&env)
        .into_iter()
        .map(|(name, _)| name.to_owned())
        .chain(IN_TEST_MODULES.map(str::to_owned))
        .collect();
    assert_eq!(names_in_sources(), set, "the knobs the binary reads");
    let home = tree(&env);
    let id = saves_on_the_real_clock(&env, &home);
    stale_reads_the_real_clock(&env, &home, &id);
    mcp_keeps_its_tools_and_times(&env, &home);
}
