//! P5-SEC-01: every fuzz target starts from seeds committed beside it, in
//! `fuzz/corpus/<target>/seed-*`, so a run with no corpus cached (a pull
//! request's fuzz-smoke, a night whose cache was evicted, a developer's
//! first `just fuzz`) begins from inputs of the target's own format rather
//! than from nothing. What a run adds to a corpus is not committed
//! (`fuzz/.gitignore`); nightly-fuzz.yml keeps it in the Actions cache.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fs;
use std::path::{Path, PathBuf};

/// A seed is a small input: the largest committed is about 1 KiB.
const MAX_SEED: u64 = 64 * 1024;

fn fuzz_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../fuzz")
}

/// The names of `fuzz/Cargo.toml`'s targets.
fn targets() -> Vec<String> {
    let manifest: toml::Table =
        toml::from_str(&fs::read_to_string(fuzz_dir().join("Cargo.toml")).unwrap()).unwrap();
    let mut names: Vec<String> = manifest["bin"]
        .as_array()
        .unwrap()
        .iter()
        .map(|bin| bin["name"].as_str().unwrap().to_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn p5_sec_01_every_fuzz_target_starts_from_committed_seeds() {
    let corpus = fuzz_dir().join("corpus");
    for target in targets() {
        let dir = corpus.join(&target);
        let seeds: Vec<(String, u64)> = fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("{target}: no seeds in {}: {e}", dir.display()))
            .map(|entry| {
                let entry = entry.unwrap();
                let name = entry.file_name().into_string().unwrap();
                (name, entry.metadata().unwrap().len())
            })
            .filter(|(name, _)| name.starts_with("seed-"))
            .collect();
        assert!(
            !seeds.is_empty(),
            "{target}: no seed-* file in {}",
            dir.display()
        );
        for (name, len) in seeds {
            // libFuzzer skips an empty file; it tries the empty input itself.
            assert!(len > 0, "{target}/{name} is empty");
            assert!(len <= MAX_SEED, "{target}/{name}: {len} bytes");
        }
    }
    // No corpus for a target that is gone.
    let mut dirs: Vec<String> = fs::read_dir(&corpus)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    dirs.sort();
    assert_eq!(dirs, targets());
    // Git keeps the seeds and leaves out what a run adds.
    let ignore = fs::read_to_string(fuzz_dir().join(".gitignore")).unwrap();
    let rules: Vec<&str> = ignore
        .lines()
        .filter(|line| !line.starts_with('#') && !line.is_empty())
        .collect();
    assert!(
        rules.contains(&"corpus/*/*") && rules.contains(&"!corpus/*/seed-*"),
        "fuzz/.gitignore: {rules:?}"
    );
}
