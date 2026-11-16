//! P5-BENCH-01: the 1.0 acceptance run as one harness command, and its
//! judgement against the goal-1 and goal-2 targets.
//!
//! `bench/run.sh acceptance` runs whole against the binary this test run
//! built, on scaled trees, with `bench/testdata/hyperfine_stub.py` for
//! hyperfine, `true` for the cache drop and a stand-in for diskus that answers
//! as its pin: nothing touches the network, and its numbers are a smoke
//! tree's. The Python tests run under the interpreter `bench/run.sh` picks.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use duscope_testkit::TestEnv;
use serde_json::Value;

const SCALE: &str = "0.0002";

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

/// The interpreter `bench/run.sh` picks: `$PYTHON`, else `/usr/bin/python3`,
/// else `python3`.
fn python() -> String {
    std::env::var("PYTHON")
        .ok()
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| {
            if Path::new("/usr/bin/python3").is_file() {
                "/usr/bin/python3".to_owned()
            } else {
                "python3".to_owned()
            }
        })
}

fn script(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn platform() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    }
}

/// P5-BENCH-01: the unit tests of the acceptance run, the rescans, the
/// judgement and the summary.
#[test]
fn p5_bench_01_python_unit_tests() {
    let out = Command::new(python())
        .args([
            "-B", "-E", "-s", "-m", "unittest", "discover", "-s", "bench", "-p",
        ])
        .arg("test_acceptance.py")
        .current_dir(root())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "bench/test_acceptance.py:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `bench/run.sh acceptance` on the scaled trees under `scratch`, with a
/// stand-in diskus as the one competitor.
fn acceptance(env: &TestEnv, scratch: &Path) -> Output {
    let diskus = scratch.join("diskus");
    script(
        &diskus,
        "#!/bin/sh\n[ \"$1\" = --version ] && { echo 'diskus 0.9.0'; exit 0; }\nexec du -sk \"$1\" > /dev/null\n",
    );
    let stub = root().join("bench/testdata/hyperfine_stub.py");
    env.command(root().join("bench/run.sh"))
        .arg("acceptance")
        .arg("--duscope")
        .arg(env.cmd().get_program())
        .args(["--platform", platform(), "--scale", SCALE, "--jobs", "1"])
        .args(["--competitors", "diskus", "--cold-cmd", "true"])
        .arg("--tool-path")
        .arg(format!("diskus={}", diskus.display()))
        .args(["--runs", "2", "--warmup", "0", "--index-runs", "2"])
        .arg("--hyperfine")
        .arg(format!("{} {}", python(), stub.display()))
        .arg("--trees-dir")
        .arg(scratch.join("trees"))
        .arg("--tools-dir")
        .arg(scratch.join("tools"))
        .arg("--out-dir")
        .arg(scratch.join("out"))
        .output()
        .unwrap()
}

/// The rescan part of an acceptance file of this platform: each tree measured
/// or not with a reason, and every failure a rescan that read less than its
/// change, which only a busy machine's slow history makes (macOS).
fn check_rescans(result: &Value) {
    let rescans = result["rescan"]["trees"].as_array().unwrap();
    let names: Vec<&str> = rescans
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["T1", "T2", "T3"]);
    for tree in rescans {
        let measured = tree["status"] == "measured";
        assert!(
            measured || tree["reason"].as_str().is_some_and(|r| !r.is_empty()),
            "{tree}"
        );
        if platform() == "linux" {
            assert!(!measured, "{tree}");
            assert!(
                tree["reason"]
                    .as_str()
                    .unwrap()
                    .contains("keeps no change history"),
                "{tree}"
            );
            assert_eq!(tree["rounds"], 0, "{tree}");
        }
    }
    assert_eq!(rescans[2]["status"], "not measured", "{}", rescans[2]);
    for failure in result["failures"].as_array().unwrap() {
        let failure = failure.as_str().unwrap();
        assert!(
            platform() == "macos" && failure.contains("raise --settle"),
            "{failure}"
        );
    }
}

/// P5-BENCH-01: one command measures the harness's matrix, the index on T2
/// and the incremental rescans against full ones, and writes them in one
/// file named for this platform (a scaled run's name says so), whose
/// targets it judges as it ends. Linux keeps no change history: each tree
/// says so in the binary's words. T3's one directory is every directory it
/// has, so no platform reads it incrementally. The trees match their stamps
/// after the rescans' changes.
#[test]
fn p5_bench_01_the_acceptance_run_writes_one_file_for_this_platform() {
    let env = TestEnv::new();
    let scratch = env.tmp_dir();
    let out = acceptance(&env, &scratch);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let file = scratch.join(format!("out/{}-x{SCALE}.json", platform()));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        file.to_str().unwrap(),
        "{stderr}"
    );
    let result: Value = serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(result["kind"], "acceptance");
    assert_eq!(result["platform"], platform());
    // The harness: duscope paired with diskus on every tree, warm and cold.
    let run = &result["run"];
    assert_eq!(run["subject"], "duscope");
    let mut pairs: Vec<String> = run["results"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["paired_with"] == "duscope")
        .map(|r| format!("{} {} {}", r["tree"], r["mode"], r["tool"]).replace('"', ""))
        .collect();
    pairs.sort();
    assert_eq!(
        pairs,
        [
            "T1 cold diskus",
            "T1 warm diskus",
            "T2 cold diskus",
            "T2 warm diskus",
            "T3 cold diskus",
            "T3 warm diskus"
        ]
    );
    // The index measurements, on T2, the tree the gates read.
    assert_eq!(result["index"]["kind"], "index");
    assert_eq!(result["index"]["gates_tree"], "T2");
    check_rescans(&result);
    assert_eq!(
        out.status.success(),
        result["failures"].as_array().unwrap().is_empty(),
        "{stderr}"
    );
    // The run judges its own platform's targets as it ends.
    let target = if platform() == "linux" {
        "bench: goal 1: Linux warm p50 <= 1.2x diskus"
    } else {
        "bench: goal 1: macOS warm p50 <= 1.0x diskus"
    };
    assert!(stderr.contains(target), "{stderr}");
    assert!(
        stderr.contains("not the acceptance run"),
        "a smoke run says so: {stderr}"
    );
    let verified = env
        .command(root().join("bench/run.sh"))
        .args(["verify", "--trees", "all", "--scale", SCALE, "--trees-dir"])
        .arg(scratch.join("trees"))
        .output()
        .unwrap();
    assert!(
        verified.status.success(),
        "{}",
        String::from_utf8_lossy(&verified.stderr)
    );
    // `judge` reads the file back; the other platform has none.
    let judged = env
        .command(root().join("bench/run.sh"))
        .arg("judge")
        .arg(&file)
        .output()
        .unwrap();
    assert_eq!(judged.status.code(), Some(1));
    let other = if platform() == "linux" {
        "macos"
    } else {
        "linux"
    };
    assert!(
        String::from_utf8_lossy(&judged.stdout).contains(&format!("{other}: no result file")),
        "{judged:?}"
    );
}
