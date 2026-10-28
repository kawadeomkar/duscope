//! P1-BENCH-01: the shipped thread defaults follow the thread sweep. A scan
//! without `--threads` runs the count the sweep's rule chose (bench/sweep.py)
//! in the bench runner's committed sweep for this platform and architecture,
//! `bench/results/thread-sweep-<platform>-<arch>.json`; a missing file fails.
//! bench-macos's sweep (Apple silicon) is committed; bench-linux's comes from
//! a `sweep` dispatch of nightly-bench.yml once it is on main, so the test is
//! ignored elsewhere until then.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use duscope_testkit::TestEnv;

/// The harness's name for this machine's platform and architecture.
fn platform_key() -> String {
    let arch = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "arm64",
        (_, arch) => arch,
    };
    format!("{}-{arch}", std::env::consts::OS)
}

#[test]
#[cfg_attr(
    not(all(target_os = "macos", target_arch = "aarch64")),
    ignore = "only bench-macos's sweep (macos-arm64) is committed; bench-linux's needs a sweep dispatch from main"
)]
fn p1_bench_01_the_default_follows_the_committed_sweep() {
    let file = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../bench/results")
        .join(format!("thread-sweep-{}.json", platform_key()));
    let text = fs::read_to_string(&file).unwrap_or_else(|e| {
        panic!(
            "{}: {e}: no sweep is committed for {}",
            file.display(),
            platform_key()
        )
    });
    let sweep: serde_json::Value = serde_json::from_str(&text).unwrap();
    let Some(default) = sweep["default"].as_u64() else {
        panic!("{} has no default", file.display());
    };
    let env = TestEnv::new();
    let out = Command::from_std(env.cmd())
        .args(["scan", "--json", "--treemap", "never"])
        .arg(env.tmp_dir())
        .env_remove("DUSCOPE_THREADS")
        .output()
        .unwrap();
    assert!(out.status.success());
    let envelope: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        envelope["scan"]["thread_count"].as_u64(),
        Some(default),
        "the default disagrees with {}",
        file.display()
    );
}
