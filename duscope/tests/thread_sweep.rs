//! P1-BENCH-01: the shipped thread defaults follow the thread sweep, committed
//! per bench runner as `bench/results/thread-sweep-<platform>-<arch>.json`.
//! macOS runs the fixed count the sweep's rule chose (bench/sweep.py) in
//! bench-macos's file. Linux keeps its CPU-scaled count, twice the logical
//! CPUs clamped to 4..=32, which bench-linux's file holds to the rule's bound
//! of 5% of the best warm T1 p50 (DEV-54). A missing file fails.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fs;
use std::path::{Path, PathBuf};

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

/// The committed sweep for a platform key, and its path.
fn sweep(key: &str) -> (PathBuf, serde_json::Value) {
    let file = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../bench/results")
        .join(format!("thread-sweep-{key}.json"));
    let text = fs::read_to_string(&file)
        .unwrap_or_else(|e| panic!("{}: {e}: no sweep is committed for {key}", file.display()));
    (file, serde_json::from_str(&text).unwrap())
}

/// The thread count a scan without `--threads` reports.
fn default_thread_count() -> u64 {
    let env = TestEnv::new();
    let out = Command::from_std(env.cmd())
        .args(["scan", "--json", "--treemap", "never"])
        .arg(env.tmp_dir())
        .env_remove("DUSCOPE_THREADS")
        .output()
        .unwrap();
    assert!(out.status.success());
    let envelope: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    envelope["scan"]["thread_count"].as_u64().unwrap()
}

/// The Linux and fallback providers' count on a machine with `cpus` logical
/// CPUs.
fn cpu_scaled(cpus: u64) -> u64 {
    cpus.saturating_mul(2).clamp(4, 32)
}

#[test]
#[cfg_attr(
    not(all(target_os = "macos", target_arch = "aarch64")),
    ignore = "the macOS provider's fixed count, from bench-macos's sweep (macos-arm64)"
)]
fn p1_bench_01_the_default_follows_the_committed_sweep() {
    let (file, sweep) = sweep(&platform_key());
    let Some(default) = sweep["default"].as_u64() else {
        panic!("{} has no default", file.display());
    };
    assert_eq!(
        default_thread_count(),
        default,
        "the default disagrees with {}",
        file.display()
    );
}

#[test]
fn p1_bench_01_the_linux_default_ran_within_the_rules_bound() {
    let (file, sweep) = sweep("linux-x86_64");
    let cpus = sweep["host"]["logical_cpus"].as_u64().unwrap();
    let within = sweep["rule"]["within"].as_f64().unwrap();
    let warm_t1: Vec<(u64, f64)> = sweep["results"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["tree"] == "T1" && r["mode"] == "warm")
        .map(|r| (r["threads"].as_u64().unwrap(), r["p50_s"].as_f64().unwrap()))
        .collect();
    let best = warm_t1
        .iter()
        .map(|&(_, p50)| p50)
        .fold(f64::INFINITY, f64::min);
    let count = cpu_scaled(cpus);
    let p50 = warm_t1
        .iter()
        .find(|&&(n, _)| n == count)
        .map(|&(_, p50)| p50)
        .unwrap_or_else(|| panic!("{} has no warm T1 row at {count} threads", file.display()));
    assert!(
        p50 <= best * (1.0 + within),
        "{count} threads, the count on bench-linux's {cpus} CPUs, ran {p50} s against the best {best} s in {}",
        file.display()
    );
}

#[test]
#[cfg_attr(not(target_os = "linux"), ignore = "the Linux provider's count")]
fn p1_bench_01_linux_scans_start_twice_the_cpus() {
    let cpus = std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get);
    assert_eq!(
        default_thread_count(),
        cpu_scaled(u64::try_from(cpus).unwrap())
    );
}
