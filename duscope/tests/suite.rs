//! P1-TEST-01: the phase-1 suite's own checks. The scan's `--json` goldens,
//! basic and `--attrs full`, and the `--plain` and table goldens of scan,
//! tree, by-type and stale (the other `--json` goldens and top's four
//! formats live beside their packages' tests); `du` parity of the binary's
//! totals on the fixture tree, and on a system tree in the nightly; and the
//! scan's peak RSS per entry on the benchmark trees.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use duscope_testkit::{TestEnv, golden, replace_values};

fn duscope(env: &TestEnv) -> Command {
    Command::from_std(env.cmd())
}

/// The binary the nightly checks: `DUSCOPE_BENCH_BIN` (its release build,
/// musl on Linux, as the packages ship it), else the one this test run
/// built.
fn shipped(env: &TestEnv) -> OsString {
    std::env::var_os("DUSCOPE_BENCH_BIN").unwrap_or_else(|| env.cmd().get_program().to_owned())
}

/// Whether the platform tests were asked for: the nightly sets
/// `DUSCOPE_TEST_PLATFORM=1`; a bare `--ignored` run skips them.
fn platform_tests() -> bool {
    let on = std::env::var_os("DUSCOPE_TEST_PLATFORM").is_some_and(|v| v == "1");
    if !on {
        eprintln!("skipped: set DUSCOPE_TEST_PLATFORM=1");
    }
    on
}

/// The shared normalisation, and the thread count, which follows the
/// machine's cores.
fn normalize(text: &str, root: &Path) -> String {
    let mut text = duscope_testkit::normalize(text, root);
    replace_values(&mut text, "\"thread_count\":", ',', "0");
    text
}

/// The `--json` scan of `root` by the [`shipped`] binary.
fn scan_json(env: &TestEnv, args: &[&str], root: &Path) -> serde_json::Value {
    let out = Command::from_std(env.command(shipped(env)))
        .arg("scan")
        .args(args)
        .arg("--json")
        .arg(root)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

/// The first number `du` prints for `root` with `flags`, in KiB.
fn du(env: &TestEnv, flags: &[&str], root: &Path) -> u64 {
    let out = env.command("du").args(flags).arg(root).output().unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    text.split_whitespace()
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| {
            panic!(
                "du {flags:?}: {text}{}",
                String::from_utf8_lossy(&out.stderr)
            )
        })
}

/// Asserts that a scan of `root` allocates what `du -skx` counts. Clones
/// are left undeduplicated, as `du` counts them. Apparent sizes are held
/// to `du` by P1-WALK-02's and P1-WALK-03's provider tests, which know
/// where the `du`s differ (coreutils 9.4 left a directory's own size out
/// of `--apparent-size`; Apple's `-A` rounds each entry to 512 bytes).
fn assert_du_parity(env: &TestEnv, root: &Path) {
    let scan = &scan_json(env, &["-x", "--no-clones", "--treemap", "never"], root)["scan"];
    assert_eq!(
        scan["error_count"],
        0,
        "{} has entries this user cannot read; du and duscope count those differently",
        root.display()
    );
    // On overlayfs an inode number is not stable across layers, so duscope
    // counts every link of a file and du only the first (a container's /usr).
    assert_ne!(
        scan["fs_type"],
        "overlay",
        "{} is on overlayfs, where duscope does not deduplicate hard links",
        root.display()
    );
    let kib = scan["total_allocated_bytes"]
        .as_u64()
        .unwrap()
        .div_ceil(1024);
    assert_eq!(kib, du(env, &["-skx"], root), "du -skx {}", root.display());
}

#[test]
fn p1_test_01_scan_json_golden_on_the_fixture_tree() {
    let env = TestEnv::new();
    let home = env.fixtures().path("home");
    let out = duscope(&env)
        .args(["scan", "--json", "--threads", "4"])
        .arg(&home)
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    golden(
        &format!("scan_json_home@{}.snap", std::env::consts::OS),
        &normalize(&text, &home),
    );
}

/// The scan's `--json` with `--attrs full`: the same rows and totals as a
/// basic scan, under `attrs: full`. The phase-1 rows have no field for
/// mode, owner, group or birth time; P1-WALK-05's tests hold those to
/// `lstat`.
#[test]
fn p1_test_01_scan_json_attrs_full_golden_on_the_fixture_tree() {
    let env = TestEnv::new();
    let home = env.fixtures().path("home");
    let out = duscope(&env)
        .args(["scan", "--json", "--attrs", "full", "--threads", "4"])
        .arg(&home)
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    golden(
        &format!("scan_json_attrs_full@{}.snap", std::env::consts::OS),
        &normalize(&text, &home),
    );
}

/// `--plain` and table goldens of scan, tree, by-type and stale on the
/// fixture tree, with the arguments of their `--json` goldens (P1-CLI-03's
/// `verbs.rs`; top's four formats are P1-CLI-02's `output.rs`).
#[test]
fn p1_test_01_plain_and_table_goldens_for_scan_tree_by_type_and_stale() {
    let env = TestEnv::new();
    let root = fs::canonicalize(env.fixtures().path("home")).unwrap();
    let home = root.to_str().unwrap();
    let os = std::env::consts::OS;
    let verbs: [(&str, &[&str]); 4] = [
        ("scan", &["scan", home]),
        (
            "tree",
            &[
                "tree",
                home,
                "--depth",
                "2",
                "--min-pct",
                "5",
                "--limit",
                "20",
            ],
        ),
        ("by_type", &["by-type", home, "--examples", "2"]),
        (
            "stale",
            &[
                "stale",
                home,
                "--older-than",
                "1y",
                "--min-size",
                "1M",
                "--limit",
                "10",
            ],
        ),
    ];
    let formats: [(&str, &[&str]); 2] = [
        ("plain", &["--plain", "--header"]),
        ("table", &["--format", "table"]),
    ];
    for (verb, args) in verbs {
        for (format, flags) in formats {
            let out = duscope(&env)
                .args(args)
                .args(["--threads", "4"])
                .args(flags)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{verb} {format}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let text = normalize(&String::from_utf8(out.stdout).unwrap(), &root);
            // stale's plain records are the same on both platforms (each
            // file it lists allocates exactly its length), so one copy.
            if (verb, format) == ("stale", "plain") {
                golden("stale_plain_home.snap", &without_ctime(&text));
            } else {
                golden(&format!("{verb}_{format}_home@{os}.snap"), &text);
            }
        }
    }
}

/// `stale --plain --header` with each record's ctime, the fourth field, as
/// `<built>`: a fixture's ctime is when the tree was built.
fn without_ctime(text: &str) -> String {
    let mut lines = text.lines();
    let header = lines.next().unwrap_or_default();
    assert_eq!(header.split('\t').nth(3), Some("ctime"), "{header}");
    let mut out = format!("{header}\n");
    for line in lines {
        let mut fields: Vec<&str> = line.split('\t').collect();
        fields[3] = "<built>";
        out.push_str(&fields.join("\t"));
        out.push('\n');
    }
    out
}

#[test]
fn p1_test_01_scan_totals_equal_du_on_the_fixture_tree() {
    let env = TestEnv::new();
    assert_du_parity(&env, &env.fixtures().path("home"));
}

/// `du` parity on a system tree: `/usr` on Linux and `/Applications` on
/// macOS, or `DUSCOPE_PARITY_ROOT`, of the [`shipped`] binary. The nightly
/// runs it on the bench runners; a pull request never does.
#[test]
#[ignore = "reads a system tree; DUSCOPE_TEST_PLATFORM=1, run by nightly-bench.yml"]
fn p1_test_01_scan_totals_equal_du_on_a_system_tree() {
    if !platform_tests() {
        return;
    }
    let env = TestEnv::new();
    let root = std::env::var_os("DUSCOPE_PARITY_ROOT").map_or_else(
        || {
            PathBuf::from(if cfg!(target_os = "macos") {
                "/Applications"
            } else {
                "/usr"
            })
        },
        PathBuf::from,
    );
    assert_du_parity(&env, &root);
}

/// The scan's peak RSS per entry with `--attrs full` on each benchmark tree
/// that is built: at most 130 bytes on T1 and T2 and 160 on T3, held by the
/// median of five runs. The peak moves by a few bytes per entry from run to
/// run with how the workers' batches overlap (Linux T1 measured 128.2 to
/// 132.6), so the highest run would fail some nights with no change. The
/// trees are `DUSCOPE_BENCH_TREES`, else `bench/.trees`; hyperfine
/// (`HYPERFINE`, else on the PATH) reads the peak, as the harness does. The
/// binary is the [`shipped`] one: run it on a release build (`cargo test
/// --release`).
#[test]
#[ignore = "needs the bench trees and hyperfine; DUSCOPE_TEST_PLATFORM=1, run by nightly-bench.yml"]
fn p1_test_01_peak_rss_per_entry_on_the_bench_trees() {
    if !platform_tests() {
        return;
    }
    let env = TestEnv::new();
    // Named trees must be there (the nightly); the default ones may not.
    let named = std::env::var_os("DUSCOPE_BENCH_TREES");
    let trees = named.clone().map_or_else(
        || Path::new(env!("CARGO_MANIFEST_DIR")).join("../bench/.trees"),
        PathBuf::from,
    );
    let hyperfine = std::env::var_os("HYPERFINE").unwrap_or_else(|| "hyperfine".into());
    let bin = shipped(&env);
    let mut measured = 0;
    for (tree, budget) in [("T1", 130u64), ("T2", 130), ("T3", 160)] {
        let root = trees.join(tree);
        if !root.is_dir() {
            eprintln!("skipped {tree}: not built under {}", trees.display());
            continue;
        }
        let scan = &scan_json(&env, &["--attrs", "full", "--treemap", "never"], &root)["scan"];
        let entries = scan["file_count"].as_u64().unwrap() + scan["dir_count"].as_u64().unwrap();
        let export = env.tmp_dir().join(format!("{tree}.json"));
        let command = format!(
            "{} scan --attrs full --json --treemap never {}",
            shell_quote(&bin.to_string_lossy()),
            shell_quote(&root.to_string_lossy())
        );
        let out = env
            .command(&hyperfine)
            .args(["-N", "--runs", "5", "--warmup", "1", "--style", "none"])
            .args(["--metrics", "time_wall_clock,memory_peak_resident"])
            .arg("--export-json")
            .arg(&export)
            .arg(&command)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "hyperfine: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let result: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&export).unwrap()).unwrap();
        // Byte counts well inside f64's integers.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let mut peaks: Vec<u64> = result["results"][0]["measurements"]
            .as_array()
            .map(|runs| {
                runs.iter()
                    .filter_map(|run| run["memory_peak_resident"]["value"].as_f64())
                    .map(|peak| peak as u64)
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            peaks.len(),
            5,
            "hyperfine 2.x reports memory_peak_resident for each run"
        );
        peaks.sort_unstable();
        let per_entry = |peak: u64| {
            let tenths = peak * 10 / entries;
            format!("{}.{}", tenths / 10, tenths % 10)
        };
        let runs: Vec<String> = peaks.iter().map(|&p| per_entry(p)).collect();
        let (peak, tenths) = (peaks[2], peaks[2] * 10 / entries);
        eprintln!(
            "{tree}: {entries} entries, median peak {peak} bytes, {} bytes per entry (runs {}; budget {budget})",
            per_entry(peak),
            runs.join(", ")
        );
        assert!(
            tenths <= budget * 10,
            "{tree}: over {budget} bytes per entry"
        );
        measured += 1;
    }
    assert!(
        measured > 0 || named.is_none(),
        "no bench tree is built under {}",
        trees.display()
    );
}

/// `text` quoted for the shell hyperfine runs its command in.
fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}
