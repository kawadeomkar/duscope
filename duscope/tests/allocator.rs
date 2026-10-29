//! P1-BENCH-03: the global allocator follows the measured rule
//! (bench/results/allocator.json): mimalloc on musl when its gain on a warm
//! T2 scan is at least 2%, and on macOS only when it is at least 10%.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fs;
use std::path::Path;

fn read(relative: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(relative);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[test]
fn p1_bench_03_the_allocator_follows_the_measured_rule() {
    let result: serde_json::Value =
        serde_json::from_str(&read("bench/results/allocator.json")).unwrap();
    // The bench runners' rows decide: bench-linux's x86_64 run replaced the
    // provisional one from an aarch64 container on the bench Mac.
    let gain = |platform: &str, arch: &str| {
        result["measurements"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["platform"] == platform && m["arch"] == arch)
            .and_then(|m| m["gain_pct"].as_f64())
            .unwrap_or_else(|| panic!("no {platform} {arch} measurement"))
    };
    let musl = gain("linux", "x86_64") >= 2.0;
    let macos = gain("macos", "arm64") >= 10.0;
    let manifest = read("duscope/Cargo.toml");
    let main = read("duscope/src/main.rs");
    let musl_dependency =
        "[target.'cfg(target_env = \"musl\")'.dependencies]\nmimalloc.workspace = true";
    assert_eq!(
        manifest.contains(musl_dependency),
        musl,
        "duscope/Cargo.toml"
    );
    assert_eq!(
        main.contains("#[cfg(target_env = \"musl\")]\n#[global_allocator]"),
        musl,
        "duscope/src/main.rs"
    );
    assert_eq!(
        manifest.contains("target_os = \"macos\""),
        macos,
        "the macOS allocator"
    );
}
