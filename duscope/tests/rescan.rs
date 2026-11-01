//! P2-IDX-05: `rescan` of the fixture tree: a golden, an index whose dirs
//! totals equal a fresh scan's and whose scan_meta carries the parent and
//! its options, `latest` reading the child, and `--incremental` not yet.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fs;
use std::path::Path;
use std::process::Output;

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
    let out = rescan(&env, 1, &[home.to_str().unwrap(), "--json"]);
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
    assert_eq!(out.status.code(), Some(2));
    let error: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(error["error"]["code"], "not_available");
    // --full is what a rescan does.
    let out = run(
        &env,
        &["rescan", home.to_str().unwrap(), "--full", "--json"],
    );
    ok(&out);
    let out = run(&env, &["rescan", "--full", "--incremental", "--json"]);
    assert_eq!(out.status.code(), Some(2));
}
