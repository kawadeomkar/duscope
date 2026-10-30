//! P1-WALK-07: what a scan reports of clones: `clones_unavailable` when
//! `--clones` is asked of a filesystem without clone ids.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use assert_cmd::Command;
use duscope_testkit::TestEnv;

fn codes(env: &TestEnv, args: &[&str]) -> Vec<String> {
    let out = Command::from_std(env.cmd())
        .args(args)
        .arg("--json")
        .arg(env.fixtures().case_root("empty"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let envelope: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    envelope["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["code"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn p1_walk_07_clones_asked_of_a_filesystem_without_clone_ids_warn() {
    let env = TestEnv::new();
    let warned = |args: &[&str]| codes(&env, args).iter().any(|c| c == "clones_unavailable");
    // Clones are counted by default: the default never warns.
    assert!(!warned(&["scan"]));
    assert!(!warned(&["top"]));
    assert!(!warned(&["scan", "--clones", "--no-clones"]));
    // Asked for: only APFS has clone ids. The fixture tree lies on the
    // checkout's volume, APFS on macOS and ext4 or similar on Linux.
    let apfs = cfg!(target_os = "macos");
    assert_eq!(warned(&["scan", "--clones"]), !apfs);
    assert_eq!(warned(&["top", "--clones"]), !apfs);
}
