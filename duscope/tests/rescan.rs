//! P2-IDX-05: `rescan` of the fixture tree: a golden of a full rescan, an
//! index whose dirs totals equal a fresh scan's and whose `scan_meta` carries
//! the parent and its options, `latest` reading the child, and
//! `--incremental` not available where no change history is kept
//! (P5-CORE-01 brings it to macOS); P5-CORE-12: a rescan with neither flag
//! reads a small tree whole, where the walk is cheaper than a replay.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fs;
use std::path::Path;
use std::process::{Output, Stdio};
use std::time::{Duration, Instant};

use duscope_core::query::timegrammar;
use duscope_testkit::{TestEnv, golden};
use serde_json::Value;

fn run(env: &TestEnv, args: &[&str]) -> Output {
    env.cmd().args(args).output().unwrap()
}

/// `rescan` with `args`, `hours` after the instant the environment's
/// clock reads: a rescan starts after the scan it renews, which `latest`
/// tells by.
fn rescan(env: &TestEnv, hours: i64, args: &[&str]) -> Output {
    let now = timegrammar::parse_date(env.test_now()).unwrap().ns;
    let later = timegrammar::iso8601(now + hours * 3_600_000_000_000);
    env.cmd()
        .env("DUSCOPE_TEST_NOW", later)
        .arg("rescan")
        .args(args)
        .output()
        .unwrap()
}

fn ok(out: &Output) -> Value {
    assert!(
        out.status.success(),
        "exit {:?}: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

/// Saves a scan of `root` with options a rescan must keep, and returns its
/// id.
fn save(env: &TestEnv, root: &Path) -> String {
    let saved = ok(&run(
        env,
        &[
            "scan",
            root.to_str().unwrap(),
            "--save",
            "--json",
            "--threads",
            "4",
            "--exclude",
            "*.mov",
        ],
    ));
    saved["scan"]["id"].as_str().unwrap().to_owned()
}

/// One statement's rows from the scan `scan`.
fn rows(env: &TestEnv, scan: &str, sql: &str) -> Value {
    let answer = ok(&run(
        env,
        &[
            "query",
            "--scan",
            scan,
            "--sql",
            sql,
            "--max-rows",
            "10000",
            "--max-chars",
            "0",
            "--json",
        ],
    ));
    answer["result"]["rows"].clone()
}

#[test]
fn p2_idx_05_a_rescan_is_a_fresh_scan_with_the_parents_options() {
    let env = TestEnv::new();
    let home = fs::canonicalize(env.fixtures().path("home")).unwrap();
    let parent = save(&env, &home);
    // The whole tree, as every rescan read it before P5-CORE-01: what an
    // incremental one reads depends on the volume's history.
    let out = rescan(&env, 1, &[home.to_str().unwrap(), "--full", "--json"]);
    let answer = ok(&out);
    let os = std::env::consts::OS;
    golden(
        &format!("rescan_json_home@{os}.snap"),
        &duscope_testkit::normalize(&String::from_utf8(out.stdout).unwrap(), &home),
    );
    assert_eq!(answer["query"]["command"], "rescan");
    let child = answer["scan"]["id"].as_str().unwrap().to_owned();
    assert_ne!(child, parent);
    // The parent and its options.
    let meta = |scan: &str, key: &str| {
        rows(
            &env,
            scan,
            &format!("SELECT value FROM scan_meta WHERE key = '{key}'"),
        )[0][0]
            .clone()
    };
    assert_eq!(meta(&child, "parent_scan_id"), parent.as_str());
    assert_eq!(meta(&child, "options"), meta(&parent, "options"));
    assert!(meta(&parent, "parent_scan_id").is_null());
    // The dirs totals of a fresh scan with the same options.
    let fresh = save(&env, &home);
    let totals = "SELECT id, cum_allocated, cum_apparent, n_files, n_dirs FROM dirs ORDER BY id";
    let child_totals = rows(&env, &child, &format!("{totals} LIMIT 10000"));
    assert!(child_totals.as_array().unwrap().len() > 100);
    assert_eq!(
        child_totals,
        rows(&env, &fresh, &format!("{totals} LIMIT 10000"))
    );
    // The parent stays, beside the child.
    let scans = ok(&run(&env, &["scans", "--json"]));
    let ids: Vec<&str> = scans["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_str().unwrap())
        .collect();
    assert!(
        ids.contains(&parent.as_str()) && ids.contains(&child.as_str()),
        "{ids:?}"
    );
}

#[test]
fn p2_idx_05_latest_reads_the_child_and_people_see_the_parent() {
    let env = TestEnv::new();
    let home = fs::canonicalize(env.fixtures().path("home")).unwrap();
    let parent = save(&env, &home);
    let out = rescan(&env, 1, &[home.to_str().unwrap(), "--format", "table"]);
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    let short = &parent[..4];
    assert!(
        text.lines()
            .last()
            .unwrap()
            .ends_with(&format!("rescan of saved scan {short}")),
        "{text}"
    );
    let stderr = String::from_utf8(out.stderr).unwrap();
    let child = stderr
        .lines()
        .find_map(|l| l.strip_prefix("saved as "))
        .and_then(|l| l.split_whitespace().next())
        .unwrap()
        .to_owned();
    // latest reads the newest scan of the root: the child. (schema takes
    // whichever options it was taken with.)
    let latest = ok(&run(&env, &["schema", home.to_str().unwrap(), "--json"]));
    assert_eq!(latest["scan"]["id"], child.as_str());
    let scans = ok(&run(&env, &["scans", "--json"]));
    let entries = scans["entries"].as_array().unwrap();
    let child_entry = entries.iter().find(|e| e["id"] == child.as_str()).unwrap();
    assert_eq!(child_entry["parent_scan_id"], parent.as_str());
    let rescanned = ok(&rescan(&env, 2, &[home.to_str().unwrap(), "--json"]));
    // A second rescan starts from the newest scan of the root: the child.
    let grandchild = rescanned["scan"]["id"].as_str().unwrap().to_owned();
    let scans = ok(&run(&env, &["scans", "--json"]));
    let entry = scans["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == grandchild.as_str())
        .cloned()
        .unwrap();
    assert_eq!(entry["parent_scan_id"], child.as_str());
}

#[test]
fn p2_idx_05_incremental_is_not_available_and_a_rescan_needs_a_saved_scan() {
    let env = TestEnv::new();
    let home = fs::canonicalize(env.fixtures().path("home")).unwrap();
    let out = run(&env, &["rescan", home.to_str().unwrap(), "--json"]);
    assert_eq!(out.status.code(), Some(3));
    let error: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(error["error"]["code"], "scan_not_found");
    save(&env, &home);
    let out = run(
        &env,
        &["rescan", home.to_str().unwrap(), "--incremental", "--json"],
    );
    // Where the platform keeps no change history (Linux). On macOS it reads
    // what FSEvents reports (P5-CORE-01), or fails the same way when a
    // check does.
    let read_again = out.status.success()
        && serde_json::from_slice::<Value>(&out.stdout).unwrap()["scan"]["incremental"] == true;
    if duscope_walk::fsevents::current_event_id().is_none() || !read_again {
        assert_eq!(out.status.code(), Some(2));
        let error: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(error["error"]["code"], "not_available");
    }
    // --full is what a rescan does.
    let out = run(
        &env,
        &["rescan", home.to_str().unwrap(), "--full", "--json"],
    );
    ok(&out);
    let out = run(&env, &["rescan", "--full", "--incremental", "--json"]);
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn p2_idx_05_a_rescan_from_elsewhere_reads_the_scans_own_files() {
    let env = TestEnv::new();
    let root = env.tmp_dir().join("tree");
    for (path, size) in [("keep.bin", 3000), ("drop.log", 2000), ("sub/drop.log", 10)] {
        let file = root.join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, vec![b'x'; size]).unwrap();
    }
    let root = fs::canonicalize(root).unwrap();
    // A relative --exclude-from, from the working directory.
    fs::write(env.work_dir().join("excludes.txt"), "*.log\n").unwrap();
    let saved = ok(&run(
        &env,
        &[
            "scan",
            root.to_str().unwrap(),
            "--save",
            "--json",
            "--exclude-from",
            "excludes.txt",
        ],
    ));
    assert_eq!(saved["scan"]["file_count"], 1);
    // From another directory, the rescan reads the same file.
    let elsewhere = env.tmp_dir();
    let out = env
        .cmd()
        .current_dir(&elsewhere)
        .env(
            "DUSCOPE_TEST_NOW",
            timegrammar::iso8601(timegrammar::parse_date(env.test_now()).unwrap().ns + 1),
        )
        .args(["rescan", root.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    let rescanned = ok(&out);
    assert_eq!(rescanned["scan"]["file_count"], 1);
    // A named scan must hold the path given beside it.
    let id = saved["scan"]["id"].as_str().unwrap();
    let out = run(&env, &["rescan", "/", "--scan", id, "--json"]);
    assert_eq!(out.status.code(), Some(3));
    // A wrong --fields costs no scan.
    let out = run(
        &env,
        &[
            "rescan",
            root.to_str().unwrap(),
            "--fields",
            "help",
            "--json",
        ],
    );
    assert!(out.status.success());
    let listed: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(
        listed["fields"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f == "path")
    );
}

#[test]
fn p5_sec_01_a_rescan_refuses_a_pattern_file_that_became_a_fifo() {
    let env = TestEnv::new();
    let root = env.tmp_dir().join("tree");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("a.log"), b"x").unwrap();
    let root = fs::canonicalize(root).unwrap();
    let patterns = env.work_dir().join("excludes.txt");
    fs::write(&patterns, "*.log\n").unwrap();
    let root_arg = root.to_str().unwrap();
    let patterns_arg = patterns.to_str().unwrap();
    ok(&run(
        &env,
        &[
            "scan",
            root_arg,
            "--save",
            "--json",
            "--exclude-from",
            patterns_arg,
        ],
    ));
    // The file the saved scan names is now a FIFO nothing writes to: read,
    // it would block the rescan for ever.
    fs::remove_file(&patterns).unwrap();
    let made = env.command("mkfifo").arg(&patterns).status().unwrap();
    assert!(made.success(), "mkfifo {}", patterns.display());
    let later = timegrammar::iso8601(timegrammar::parse_date(env.test_now()).unwrap().ns + 1);
    let mut child = env
        .cmd()
        .env("DUSCOPE_TEST_NOW", later)
        .args(["rescan", root_arg, "--json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() > deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("the rescan blocked on the FIFO");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let out = child.wait_with_output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let refused: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(refused["error"]["code"], "usage");
    assert_eq!(
        refused["error"]["message"],
        format!("--exclude-from {patterns_arg}: not a regular file")
    );
}

#[test]
fn p5_core_12_a_rescan_of_a_small_tree_walks_and_says_why() {
    let env = TestEnv::new();
    let root = fs::canonicalize(env.tmp_dir()).unwrap().join("small");
    for dir in ["a", "b/c"] {
        fs::create_dir_all(root.join(dir)).unwrap();
        fs::write(root.join(dir).join("f"), b"bytes").unwrap();
    }
    save(&env, &root);
    let path = root.to_str().unwrap();
    let history = duscope_walk::fsevents::current_event_id().is_some();
    // With neither flag: a walk of a few entries is predicted cheaper than
    // any replay of the volume's history, so the tree is read whole, and
    // the scan block says why. Linux keeps no history, and gives no reason.
    let auto = ok(&rescan(&env, 1, &[path, "--json"]));
    assert_eq!(auto["scan"]["incremental"], false);
    let why = if history {
        "replay_slower".into()
    } else {
        Value::Null
    };
    assert_eq!(auto["scan"]["fallback_reason"], why);
    let table = rescan(&env, 2, &[path, "--format", "table"]);
    let footer = String::from_utf8(table.stdout).unwrap();
    assert_eq!(
        footer.contains(" · full rescan: replay slower · "),
        history,
        "{footer}"
    );
    if history {
        // `--incremental` replays whatever the prediction: it reads what
        // changed, or fails for a reason of the history's own.
        let asked = rescan(&env, 3, &[path, "--incremental", "--json"]);
        let answer: Value = serde_json::from_slice(&asked.stdout).unwrap();
        if asked.status.success() {
            assert_eq!(answer["scan"]["incremental"], true, "{answer}");
        } else {
            let message = answer["error"]["message"].as_str().unwrap();
            assert!(!message.contains("replay_slower"), "{message}");
        }
    }
}
