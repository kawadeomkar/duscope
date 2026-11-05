//! P5-WALK-01: a saved scan records each volume's place in its `FSEvents`
//! history (macOS; NULL on Linux), and `cargo deny` still keeps the
//! `FSEvents` crates out.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use duscope_testkit::TestEnv;
use duscope_walk::fsevents;
use serde_json::Value;

fn run(env: &TestEnv, args: &[&str]) -> Value {
    let out: Output = env.cmd().args(args).output().unwrap();
    assert!(
        out.status.success(),
        "exit {:?}: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

#[test]
fn p5_walk_01_a_saved_scan_records_each_volumes_fsevents_mark() {
    let env = TestEnv::new();
    let root = env.root().join("tree");
    fs::create_dir_all(root.join("dir")).unwrap();
    fs::write(root.join("dir/file"), b"bytes").unwrap();
    let low = fsevents::current_event_id();
    let saved = run(&env, &["scan", root.to_str().unwrap(), "--save", "--json"]);
    let high = fsevents::current_event_id();
    let scan = saved["scan"]["id"].as_str().unwrap();
    let answer = run(
        &env,
        &[
            "query",
            "--scan",
            scan,
            "--sql",
            "SELECT dev, entered, fsevents_uuid, fsevents_event_id, os_build FROM scan_volumes",
            "--json",
        ],
    );
    let rows = answer["result"]["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    let row = &rows[0];
    assert_eq!(row[1], 1, "the root's volume is entered");
    // Read back as the platform reads it now: a UUID and an event id
    // sampled during the scan on macOS, nothing on Linux.
    let dev = row[0].as_i64().unwrap().cast_unsigned();
    if let Some(uuid) = fsevents::volume_uuid(dev) {
        assert_eq!(row[2], uuid.as_str());
        let id = row[3].as_u64().unwrap();
        let (low, high) = (low.unwrap(), high.unwrap());
        assert!((low..=high).contains(&id), "{low} <= {id} <= {high}");
        assert!(!row[4].as_str().unwrap().is_empty(), "{row}");
    } else {
        assert!(low.is_none() && high.is_none());
        assert_eq!(
            row.as_array().unwrap()[2..],
            [Value::Null, Value::Null, Value::Null],
            "{row}"
        );
    }
}

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

#[test]
fn p5_walk_01_cargo_deny_still_bans_the_fsevents_crates() {
    let deny = fs::read_to_string(workspace().join("deny.toml")).unwrap();
    // The [bans] deny list: from `deny = [` to the closing bracket.
    let list: Vec<&str> = deny
        .lines()
        .skip_while(|line| line.trim() != "deny = [")
        .take_while(|line| line.trim() != "]")
        .collect();
    let lock = fs::read_to_string(workspace().join("Cargo.lock")).unwrap();
    for name in ["objc2-core-services", "notify", "fsevent-sys"] {
        let entry = format!("{{ crate = \"{name}\",");
        assert!(
            list.iter()
                .any(|line| line.trim_start().starts_with(&entry)),
            "deny.toml no longer bans {name}"
        );
        assert!(
            !lock.contains(&format!("\nname = \"{name}\"\n")),
            "{name} is in Cargo.lock"
        );
    }
}
