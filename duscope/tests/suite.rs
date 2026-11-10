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

/// The scan's peak RSS per entry on each benchmark tree that is built,
/// counted above a small scan's (DEV-106): the median peak of five runs on
/// the tree, less the median peak of five runs on a tree of 1,000 directories
/// of 4 files that the test makes, over the entries between the two. The
/// small scan holds what any scan costs before its entries (the binary, the
/// worker threads and their allocator heaps), which grows with each verb the
/// binary gains and would otherwise weigh on T1's 500,001 entries twice as
/// hard as on T2's million. With basic attributes at most 100 bytes on T1 and
/// T2 and 130 on T3 (P1-CORE-01's budget), and with `--attrs full` at most 130
/// on T1 and T2 and 160 on T3 (P1-TEST-01's); the gate holds both (DEV-49).
/// The whole peak per entry is printed beside it. The peak moves by a few
/// bytes per entry from run to run with how the workers' batches overlap
/// (Linux T1 measured 128.2 to 132.6), so the highest run would fail some
/// nights with no change. The trees are `DUSCOPE_BENCH_TREES`, else
/// `bench/.trees`; hyperfine (`HYPERFINE`, else on the PATH) reads the peak,
/// as the harness does. The binary is the [`shipped`] one: run it on a
/// release build (`cargo test --release`).
///
/// The same measurement runs a second time with `--save`, the figure
/// acceptance target 5 names (P2-IDX-07), against budgets of its own: the
/// peak then includes `SQLite`'s page cache and the sort of the index keys
/// besides the arena, and the small scan is saved too, so the per-entry
/// figure is still counted above what any save costs. Each pass also holds
/// the small scan's own peak to a cap of its own. A budget or a cap of
/// [`None`] is one not yet set from a nightly's figures: that pass prints
/// its figures and ends with a `::warning` annotation saying that it
/// enforced nothing ([`pending_warning`]), and fails no check. Each tree's
/// saves are removed before the next is measured, so that the runner holds
/// six index files per tree per attribute level at the most.
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
    let small = env.tmp_dir().join("small");
    for dir in 0..1000 {
        let dir = small.join(format!("d{dir:03}"));
        fs::create_dir_all(&dir).unwrap();
        for file in 0..4 {
            fs::write(dir.join(format!("f{file}")), b"x").unwrap();
        }
    }
    let tenths = |bytes: u64, entries: u64| {
        let tenths = bytes * 10 / entries;
        format!("{}.{}", tenths / 10, tenths % 10)
    };
    let mut measured = 0;
    for gate in &RSS_GATES {
        let measure = |root: &Path, attrs: &str, name: &str| {
            rss_peaks(&env, &hyperfine, &bin, root, attrs, name, gate)
        };
        for (attrs, rows) in &gate.budgets {
            if !rows.iter().any(|(tree, _)| trees.join(tree).is_dir()) {
                for (tree, _) in rows {
                    eprintln!("skipped {tree}: not built under {}", trees.display());
                }
                continue;
            }
            let (base_entries, base_peaks) = measure(&small, attrs, "small");
            let base = base_peaks[2];
            eprintln!(
                "{} --attrs {attrs}: {base_entries}-entry baseline {base} bytes ({})",
                gate.name,
                limit_text(gate.baseline_cap, "cap")
            );
            if let Some(cap) = gate.baseline_cap {
                assert!(
                    base <= cap,
                    "{} --attrs {attrs}: the baseline is over {cap} bytes",
                    gate.name
                );
            }
            for &(tree, budget) in rows {
                let root = trees.join(tree);
                if !root.is_dir() {
                    eprintln!("skipped {tree}: not built under {}", trees.display());
                    continue;
                }
                let (entries, peaks) = measure(&root, attrs, tree);
                let above = entries - base_entries;
                let peak = peaks[2];
                let per_entry = peak.saturating_sub(base);
                let runs: Vec<String> = peaks
                    .iter()
                    .map(|&p| tenths(p.saturating_sub(base), above))
                    .collect();
                eprintln!(
                    "{tree} {} --attrs {attrs}: {entries} entries, median peak {peak} bytes, {} bytes per entry above a {base_entries}-entry scan's {base} (runs {}; {}); {} bytes per entry in all",
                    gate.name,
                    tenths(per_entry, above),
                    runs.join(", "),
                    limit_text(budget, "budget"),
                    tenths(peak, entries)
                );
                if let Some(budget) = budget {
                    assert!(
                        per_entry * 10 / above <= budget * 10,
                        "{tree} {} --attrs {attrs}: over {budget} bytes per entry",
                        gate.name
                    );
                }
                measured += 1;
                // A pass with --save leaves six files for the tree (the
                // warmup and five runs), each the size of an index of it.
                let _ = fs::remove_dir_all(env.index_dir().join("scans"));
            }
        }
        if let Some(warning) = pending_warning(gate) {
            eprintln!("{warning}");
        }
    }
    assert!(
        measured > 0 || named.is_none(),
        "no bench tree is built under {}",
        trees.display()
    );
}

/// A tree's budget: its name, and its bytes per entry above the small
/// scan's peak, [`None`] until one is set.
type TreeBudget = (&'static str, Option<u64>);

/// An attribute level's budgets, one for each tree.
type AttrsBudgets = (&'static str, [TreeBudget; 3]);

/// One pass of the peak RSS gate: a `scan` command and its limits.
struct RssGate {
    /// What the report calls the command.
    name: &'static str,
    /// `scan`'s flags beyond the gate's own.
    flags: &'static [&'static str],
    /// The budgets of the two attribute levels.
    budgets: [AttrsBudgets; 2],
    /// The most the small scan's peak may be, in bytes, the fixed cost of
    /// any run of this command, which nothing bounded (the implementation
    /// audit, 3.3). [`None`] is a cap not yet set.
    baseline_cap: Option<u64>,
}

/// The gate's passes. `scan` holds P1-TEST-01's budgets. `scan --save`, what
/// acceptance target 5 names, has budgets of its own, not yet set: they come
/// from the nightly's figures on both platforms (P2-IDX-07).
const RSS_GATES: [RssGate; 2] = [
    RssGate {
        name: "scan",
        flags: &[],
        budgets: [
            (
                "basic",
                [("T1", Some(100)), ("T2", Some(100)), ("T3", Some(130))],
            ),
            (
                "full",
                [("T1", Some(130)), ("T2", Some(130)), ("T3", Some(160))],
            ),
        ],
        baseline_cap: None,
    },
    RssGate {
        name: "scan --save",
        flags: &["--save"],
        budgets: [
            ("basic", [("T1", None), ("T2", None), ("T3", None)]),
            ("full", [("T1", None), ("T2", None), ("T3", None)]),
        ],
        baseline_cap: None,
    },
];

/// A limit as the report prints it.
fn limit_text(limit: Option<u64>, what: &str) -> String {
    limit.map_or_else(
        || format!("{what} pending"),
        |limit| format!("{what} {limit}"),
    )
}

/// The annotation for a pass with a budget or a cap not yet set, which the
/// nightly's run page shows beside its figures: a pass that enforces nothing
/// must say so, and cannot fail or pass in silence.
fn pending_warning(gate: &RssGate) -> Option<String> {
    let pending = gate.baseline_cap.is_none()
        || gate
            .budgets
            .iter()
            .any(|(_, rows)| rows.iter().any(|(_, budget)| budget.is_none()));
    pending.then(|| {
        format!(
            "::warning title=Peak RSS::`{}`: limits not yet set, so this pass reported its figures and enforced nothing (P2-IDX-07); set them from this nightly's figures",
            gate.name
        )
    })
}

/// The command hyperfine measures.
fn rss_command(bin: &str, root: &str, attrs: &str, flags: &[&str]) -> String {
    let mut command = format!(
        "{} scan --attrs {attrs} --json --treemap never",
        shell_quote(bin)
    );
    for flag in flags {
        command.push(' ');
        command.push_str(flag);
    }
    command.push(' ');
    command.push_str(&shell_quote(root));
    command
}

#[test]
fn p2_idx_07_the_rss_gate_runs_scan_save_and_says_when_a_limit_is_pending() {
    let save = RSS_GATES
        .iter()
        .find(|gate| gate.flags == ["--save"])
        .expect("a pass with --save");
    assert_eq!(
        rss_command("bin", "/root", "basic", save.flags),
        "'bin' scan --attrs basic --json --treemap never --save '/root'"
    );
    // A limit not set is reported, a limit set is not.
    let all_set = RssGate {
        name: "set",
        flags: &[],
        budgets: [
            ("basic", [("T1", Some(1)), ("T2", Some(1)), ("T3", Some(1))]),
            ("full", [("T1", Some(1)), ("T2", Some(1)), ("T3", Some(1))]),
        ],
        baseline_cap: Some(1),
    };
    assert!(pending_warning(&all_set).is_none());
    let one_missing = RssGate {
        budgets: [
            ("basic", [("T1", Some(1)), ("T2", None), ("T3", Some(1))]),
            ("full", [("T1", Some(1)), ("T2", Some(1)), ("T3", Some(1))]),
        ],
        ..all_set
    };
    let warning = pending_warning(&one_missing).expect("a pending budget warns");
    assert!(warning.starts_with("::warning title="), "{warning}");
    assert_eq!(limit_text(None, "budget"), "budget pending");
    assert_eq!(limit_text(Some(100), "budget"), "budget 100");
}

/// A `--json` run of `gate`'s command on `root` by `bin`: its entries, and
/// the peak RSS of each of five `scan --attrs <attrs>` runs under hyperfine,
/// lowest first.
fn rss_peaks(
    env: &TestEnv,
    hyperfine: &OsString,
    bin: &OsString,
    root: &Path,
    attrs: &str,
    name: &str,
    gate: &RssGate,
) -> (u64, Vec<u64>) {
    let mut args = vec!["--attrs", attrs, "--treemap", "never"];
    args.extend_from_slice(gate.flags);
    let scan = &scan_json(env, &args, root)["scan"];
    let entries = scan["file_count"].as_u64().unwrap() + scan["dir_count"].as_u64().unwrap();
    let suffix = if gate.flags.is_empty() { "" } else { "-save" };
    let export = env.tmp_dir().join(format!("{name}-{attrs}{suffix}.json"));
    let command = rss_command(
        &bin.to_string_lossy(),
        &root.to_string_lossy(),
        attrs,
        gate.flags,
    );
    let out = env
        .command(hyperfine)
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
    (entries, peaks)
}

/// `text` quoted for the shell hyperfine runs its command in.
fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}
