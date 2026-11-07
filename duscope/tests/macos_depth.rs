//! P5-WALK-03: `--skip-containers` end to end (a golden of the scan, the
//! saved scan and its rescan), and the Full Disk Access warning under a
//! mocked refusal (`DUSCOPE_TEST_TCC=denied`), from the command line and
//! over MCP. The dataless placeholders' share of the package is tested at
//! the engine (`explain`, the tallies) and in the scan's warnings, since no
//! runner holds an evicted iCloud file.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::ffi::OsString;
use std::fs;
use std::io::Write as _;
use std::os::unix::fs::{MetadataExt as _, symlink};
use std::path::{Path, PathBuf};
use std::process::Stdio;

use duscope_testkit::{TestEnv, golden, replace_values};
use serde_json::{Value, json};

/// The fixture home, canonical, with the test's own HOME (a temporary
/// directory, never the real one) holding a link to its `Library`: the
/// containers are known by identity, so HOME's lead to the fixture's.
fn linked_home(env: &TestEnv) -> PathBuf {
    let root = fs::canonicalize(env.fixtures().path("home")).unwrap();
    symlink(root.join("Library"), env.home().join("Library")).unwrap();
    root
}

/// `duscope ARGS --json`'s envelope; the command must succeed.
fn json(env: &TestEnv, args: &[&std::ffi::OsStr], vars: &[(&str, &str)]) -> Value {
    let out = env
        .cmd()
        .args(args)
        .arg("--json")
        .envs(vars.iter().copied())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

fn os(s: &str) -> &std::ffi::OsStr {
    std::ffi::OsStr::new(s)
}

fn codes(envelope: &Value) -> Vec<String> {
    envelope["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["code"].as_str().unwrap().to_owned())
        .collect()
}

fn warning<'a>(envelope: &'a Value, code: &str) -> &'a Value {
    envelope["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["code"] == code)
        .unwrap_or_else(|| panic!("no {code} in {envelope}"))
}

#[test]
fn p5_walk_03_skip_containers_golden() {
    let env = TestEnv::new();
    let root = linked_home(&env);
    let out = env
        .cmd()
        .args(["scan", "--json", "--threads", "4", "--skip-containers"])
        .arg(&root)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    let mut normalized = duscope_testkit::normalize(&stdout, &root);
    replace_values(&mut normalized, "\"thread_count\":", ',', "0");
    golden(
        &format!("scan_json_skip_containers@{}.snap", std::env::consts::OS),
        &normalized,
    );
    let skipped: Value = serde_json::from_str(&stdout).unwrap();
    let full = json(
        &env,
        &[os("scan"), os("--threads"), os("4"), root.as_os_str()],
        &[],
    );
    // Both containers are listed, not entered, and out of the totals.
    let excluded = skipped["scan"]["excluded"].as_array().unwrap();
    let records: Vec<(String, String)> = excluded
        .iter()
        .map(|r| {
            (
                r["path"].as_str().unwrap().to_owned(),
                r["reason"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    let path = |rel: &str| root.join(rel).display().to_string();
    assert_eq!(
        records,
        [
            (path("Library/Containers"), "skipped_containers".to_owned()),
            (
                path("Library/Group Containers"),
                "skipped_containers".to_owned()
            ),
        ]
    );
    assert_eq!(skipped["scan"]["excluded_count"], 2);
    let files = |e: &Value| e["scan"]["file_count"].as_u64().unwrap();
    // The containers hold 12,000 and 3,800 of the home's files.
    assert_eq!(files(&full) - files(&skipped), 12_000 + 3_800);
    // A scan of a tree outside the home skips nothing.
    let projects = json(
        &env,
        &[
            os("scan"),
            os("--skip-containers"),
            root.join("Projects").as_os_str(),
        ],
        &[],
    );
    assert_eq!(projects["scan"]["excluded_count"], 0);
}

#[test]
fn p5_walk_03_a_saved_scan_and_its_rescan_keep_the_containers_out() {
    let env = TestEnv::new();
    let root = linked_home(&env);
    let saved = json(
        &env,
        &[
            os("scan"),
            os("--save"),
            os("--skip-containers"),
            root.as_os_str(),
        ],
        &[],
    );
    let id = saved["scan"]["id"].as_str().unwrap().to_owned();
    // A verb with the flag reads the saved scan, which holds its tree,
    // reasons and all; one without it does not.
    let top = json(
        &env,
        &[
            os("top"),
            os("--no-scan"),
            os("--skip-containers"),
            root.as_os_str(),
        ],
        &[],
    );
    assert_eq!(top["scan"]["id"], id.as_str());
    assert_eq!(top["scan"]["excluded"], saved["scan"]["excluded"]);
    let plain = env
        .cmd()
        .args(["top", "--no-scan", "--json"])
        .arg(&root)
        .output()
        .unwrap();
    assert_eq!(
        plain.status.code(),
        Some(3),
        "a scan without the flag answered"
    );
    // The rescan scans with the options it was taken with.
    let rescan = json(&env, &[os("rescan"), os("--scan"), os(&id)], &[]);
    assert_eq!(rescan["scan"]["excluded"], saved["scan"]["excluded"]);
    assert_eq!(rescan["scan"]["file_count"], saved["scan"]["file_count"]);
}

#[test]
fn p5_walk_03_a_refused_probe_warns_full_disk_access_missing() {
    let env = TestEnv::new();
    let home = env.home();
    fs::create_dir_all(home.join("Documents")).unwrap();
    let denied = [("DUSCOPE_TEST_TCC", "denied")];
    // The root holds the home: the probe runs, and macOS (mocked) refuses.
    let scan = json(&env, &[os("scan"), home.as_os_str()], &denied);
    let warned = warning(&scan, "full_disk_access_missing");
    let message = warned["message"].as_str().unwrap();
    let hint = warned["hint"].as_str().unwrap();
    assert!(message.contains("no Full Disk Access"), "{message}");
    assert!(message.contains("~/Library/Mail"), "{message}");
    assert!(
        hint.starts_with("Grant ") && hint.contains("System Settings"),
        "{hint}"
    );
    assert!(hint.ends_with("then rerun duscope scan"), "{hint}");
    assert!(!message.contains("MCP"), "{message}");
    // A root inside the home that does not hold Library: no probe.
    let documents = json(
        &env,
        &[os("scan"), home.join("Documents").as_os_str()],
        &denied,
    );
    assert!(!codes(&documents).contains(&"full_disk_access_missing".to_owned()));
    // Without the refusal: a temporary home has neither folder, which is
    // no refusal.
    let granted = json(&env, &[os("scan"), home.as_os_str()], &[]);
    assert!(!codes(&granted).contains(&"full_disk_access_missing".to_owned()));
    // Warnings never change the exit code; json() asserted success.
}

#[test]
fn p5_walk_03_the_probe_runs_for_the_data_volume_path_of_the_home() {
    let env = TestEnv::new();
    let home = env.home();
    fs::create_dir_all(home.join("Documents")).unwrap();
    // macOS gives a directory of the Data volume a second path, under the
    // volume's own mount point, which resolving a path keeps as given
    // (`/System/Volumes/Data/private/var/...` beside `/private/var/...`).
    // Elsewhere there is no such volume, and nothing to test.
    let mount = Path::new("/System/Volumes/Data");
    if !mount.is_dir() {
        return;
    }
    let data = mount.join(home.strip_prefix("/").unwrap());
    let same = match (fs::metadata(&home), fs::metadata(&data)) {
        (Ok(a), Ok(b)) => (a.dev(), a.ino()) == (b.dev(), b.ino()),
        _ => false,
    };
    if !same {
        duscope_testkit::skip(&format!("{} is not the home", data.display()));
        return;
    }
    let denied = [("DUSCOPE_TEST_TCC", "denied")];
    // The home by its Data volume path holds the home's Library: the probe
    // runs, and macOS (mocked) refuses.
    let scan = json(&env, &[os("scan"), data.as_os_str()], &denied);
    assert!(
        codes(&scan).contains(&"full_disk_access_missing".to_owned()),
        "{scan}"
    );
    // A root on that path which does not hold the Library: no probe.
    let documents = json(
        &env,
        &[os("scan"), data.join("Documents").as_os_str()],
        &denied,
    );
    assert!(!codes(&documents).contains(&"full_disk_access_missing".to_owned()));
}

#[test]
fn p5_walk_03_over_mcp_the_warning_names_the_host() {
    let env = TestEnv::new();
    let home = env.home();
    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientCapabilities": {},
            },
            "name": "duscope_scan",
            "arguments": {},
        },
    });
    let mut child = env
        .cmd()
        .arg("mcp")
        .args([OsString::from("--roots"), home.clone().into_os_string()])
        .env("DUSCOPE_TEST_TCC", "denied")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    writeln!(stdin, "{request}").unwrap();
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    let response: Value = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|message| message["id"] == 1)
        .unwrap_or_else(|| panic!("{}", String::from_utf8_lossy(&output.stderr)));
    let envelope = &response["result"]["structuredContent"];
    let warned = warning(envelope, "full_disk_access_missing");
    let message = warned["message"].as_str().unwrap();
    let hint = warned["hint"].as_str().unwrap();
    // The grant is the host's: the app running the server, by name where
    // its process tree tells it; never duscope's.
    assert!(message.contains("MCP server"), "{message}");
    assert!(hint.ends_with("call duscope_scan again"), "{hint}");
    assert!(!hint.contains("Grant duscope "), "{hint}");
}
