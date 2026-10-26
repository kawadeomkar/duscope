//! P0-BENCH-01: the benchmark harness under `bench/`.
//!
//! These tests run `bench/run.sh` on scaled-down trees in scratch directories
//! under `CARGO_TARGET_TMPDIR`, with the system `du` as the subject and
//! `bench/testdata/hyperfine_stub.py` standing in for hyperfine, which CI's
//! test job does not install; the real hyperfine runs in the acceptance runs
//! the pull request reports and in the nightly job. No pinned competitor is
//! installed (`--competitors none`), so nothing touches the network: an ad-hoc
//! `find` command plays the competitor. They read repository files and run
//! Python; they never run the duscope binary.
//!
//! The interpreter is `$PYTHON` when it is set, else `/usr/bin/python3` when it
//! exists, else `python3` from `PATH`, which is how `bench/run.sh` picks it.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The scale of every tree these tests build.
const SCALE: &str = "0.0002";

/// Each tree at `SCALE`: its digest, directories and files. A generator change
/// that changes a tree bumps `GENERATOR_VERSION` in `bench/trees.py` and
/// updates these digests in the same pull request.
const TREES: &[(&str, &str, usize, usize)] = &[
    ("T1", "a1ddf1f022c8aca72a18b689a9952357", 20, 80),
    ("T2", "74880bb642c8fd37710fcb51881c0c04", 25, 175),
    ("T3", "2b05ebab75314618af0d3ea578c488d4", 0, 200),
];

/// Prints each `(path, JSON type)` of a result file as `T\t<path>\t<type>`,
/// with `[]` for the elements of an array of objects, and each scalar as
/// `V\t<indexed path>\t<JSON value>`.
const FLATTEN: &str = r#"
import json, sys

def kind(v):
    if v is None:
        return "null"
    if isinstance(v, bool):
        return "boolean"
    if isinstance(v, int):
        return "integer"
    if isinstance(v, float):
        return "number"
    if isinstance(v, str):
        return "string"
    if isinstance(v, list):
        inner = sorted({kind(x) for x in v})
        return "array of " + " or ".join(inner) if inner else "array"
    return "object"

def walk(v, path, indexed):
    if isinstance(v, dict):
        for k, x in v.items():
            child = path + "." + k if path else k
            print("T\t" + child + "\t" + kind(x))
            walk(x, child, indexed + "." + k if indexed else k)
    elif isinstance(v, list):
        if v and all(isinstance(x, dict) for x in v):
            for i, x in enumerate(v):
                walk(x, path + "[]", indexed + "[" + str(i) + "]")
    else:
        print("V\t" + indexed + "\t" + json.dumps(v))

with open(sys.argv[1], encoding="utf-8") as f:
    walk(json.load(f), "", "")
"#;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn bench(path: &str) -> PathBuf {
    repo_root().join("bench").join(path)
}

fn python() -> OsString {
    if let Some(python) = std::env::var_os("PYTHON").filter(|p| !p.is_empty()) {
        return python;
    }
    let system = Path::new("/usr/bin/python3");
    if system.is_file() {
        system.into()
    } else {
        "python3".into()
    }
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

/// A shell word for `--hyperfine`, which the harness splits like a shell.
fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// A directory under `CARGO_TARGET_TMPDIR`, removed when dropped.
#[derive(Debug)]
struct Scratch(PathBuf);

impl Scratch {
    fn new(test: &str) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("p0_bench_01-{test}-{}-{n}", std::process::id()));
        // A directory left by a killed run of the same pid would hold stale files.
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn join(&self, path: &str) -> PathBuf {
        self.0.join(path)
    }

    /// `bench/run.sh <args>` with the scratch directories and the stub, the
    /// stub logging its runs to `stub.log`.
    fn harness(&self, args: &[&str]) -> Output {
        self.harness_with(args, &[])
    }

    /// [`Scratch::harness`] with these environment variables set.
    fn harness_with(&self, args: &[&str], vars: &[(&str, &str)]) -> Output {
        let python = python();
        let stub = format!(
            "{} {}",
            quote(&python.to_string_lossy()),
            quote(&bench("testdata/hyperfine_stub.py").to_string_lossy())
        );
        let mut command = Command::new(bench("run.sh"));
        command
            .args(args)
            .env("PYTHON", &python)
            .env("BENCH_STUB_LOG", self.join("stub.log"));
        if matches!(args.first(), None | Some(&"run")) || args[0].starts_with('-') {
            command.args(["--hyperfine", &stub]);
            command.arg("--tools-dir").arg(self.join("tools"));
            command.arg("--results-dir").arg(self.join("results"));
        }
        if !matches!(args.first(), Some(&"tools")) {
            command.arg("--trees-dir").arg(self.join("trees"));
        }
        command.envs(vars.iter().copied());
        command.output().unwrap_or_else(|e| {
            panic!("cannot run bench/run.sh: {e}; set PYTHON to a Python 3.9 or later interpreter")
        })
    }

    fn stub_log(&self) -> Vec<String> {
        fs::read_to_string(self.join("stub.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn succeeded(output: &Output, what: &str) -> String {
    assert!(
        output.status.success(),
        "{what} failed with {}:\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout.clone()).unwrap()
}

/// A run of du paired with `find` on the scaled T1.
fn small_run(scratch: &Scratch, extra: &[&str]) -> PathBuf {
    small_run_with(scratch, extra, &[])
}

/// [`small_run`] with these environment variables set.
fn small_run_with(scratch: &Scratch, extra: &[&str], vars: &[(&str, &str)]) -> PathBuf {
    let mut args = vec![
        "--trees",
        "T1",
        "--scale",
        SCALE,
        "--competitors",
        "none",
        "--command",
        "find=find {tree}",
        "--runs",
        "3",
        "--warmup",
        "1",
    ];
    args.extend_from_slice(extra);
    let stdout = succeeded(&scratch.harness_with(&args, vars), "bench/run.sh");
    let path = PathBuf::from(stdout.trim());
    assert!(
        path.is_file(),
        "the run printed {stdout:?}, which is not a file"
    );
    path
}

/// A result file's (path, JSON types) and (indexed path, JSON value) pairs.
fn flatten(result: &Path) -> (BTreeMap<String, BTreeSet<String>>, BTreeMap<String, String>) {
    let output = Command::new(python())
        .args(["-B", "-E", "-s", "-c", FLATTEN])
        .arg(result)
        .output()
        .unwrap();
    let stdout = succeeded(&output, "flattening the result file");
    let mut types: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut values = BTreeMap::new();
    for line in stdout.lines() {
        let mut fields = line.splitn(3, '\t');
        match (fields.next(), fields.next(), fields.next()) {
            (Some("T"), Some(path), Some(kind)) => {
                types
                    .entry(path.to_owned())
                    .or_default()
                    .insert(kind.to_owned());
            }
            (Some("V"), Some(path), Some(value)) => {
                values.insert(path.to_owned(), value.to_owned());
            }
            _ => panic!("unexpected line {line:?}"),
        }
    }
    (types, values)
}

/// The rows of the table under `## <heading>` of a markdown file, as cells.
fn table_rows(text: &str, heading: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut in_section = false;
    for line in text.lines() {
        if let Some(title) = line.strip_prefix("## ") {
            in_section = title.trim() == heading;
            continue;
        }
        if !in_section || !line.trim_start().starts_with('|') {
            continue;
        }
        // `\|` is a literal bar inside a cell.
        let protected = line.trim().replace(r"\|", "\u{1}");
        let cells: Vec<String> = protected
            .trim_matches('|')
            .split('|')
            .map(|c| c.trim().replace('\u{1}', "|"))
            .collect();
        if cells
            .iter()
            .all(|c| c.chars().all(|ch| matches!(ch, '-' | ':' | ' ')))
        {
            continue;
        }
        rows.push(cells);
    }
    assert!(!rows.is_empty(), "no table under ## {heading}");
    rows
}

/// The first code span of a cell, if any.
fn code_span(cell: &str) -> Option<&str> {
    cell.split('`').nth(1)
}

/// bench/README.md's result-file fields and their documented types.
fn documented_fields() -> BTreeMap<String, BTreeSet<String>> {
    table_rows(&read(&bench("README.md")), "Result file")
        .iter()
        .skip(1) // the header
        .map(|cells| {
            let field = code_span(&cells[0]).unwrap_or_else(|| panic!("no field in {cells:?}"));
            let kinds = cells[1]
                .split(" or ")
                .map(|k| k.trim().to_owned())
                .collect();
            (field.to_owned(), kinds)
        })
        .collect()
}

fn value<'a>(values: &'a BTreeMap<String, String>, path: &str) -> &'a str {
    values
        .get(path)
        .unwrap_or_else(|| panic!("no {path} in the result file"))
}

fn number(values: &BTreeMap<String, String>, path: &str) -> f64 {
    value(values, path)
        .parse()
        .unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn expected_platform() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    }
}

#[test]
fn p0_bench_01_result_matches_documented_shape() {
    let scratch = Scratch::new("shape");
    let result = small_run(&scratch, &[]);
    let (types, values) = flatten(&result);

    // Every field the run wrote is documented with the type it has, and every
    // documented field was written.
    let documented = documented_fields();
    let observed: BTreeSet<&String> = types.keys().collect();
    let listed: BTreeSet<&String> = documented.keys().collect();
    assert_eq!(
        observed,
        listed,
        "fields written but not documented: {:?}; documented but not written: {:?}",
        observed.difference(&listed).collect::<Vec<_>>(),
        listed.difference(&observed).collect::<Vec<_>>()
    );
    for (path, kinds) in &types {
        for kind in kinds {
            assert!(
                documented[path].contains(kind),
                "{path} is {kind}, but bench/README.md documents it as {:?}",
                documented[path]
            );
        }
    }

    // The file is <results-dir>/<date>-<platform>.json.
    let date = value(&values, "date").trim_matches('"');
    let platform = value(&values, "platform").trim_matches('"');
    assert_eq!(platform, expected_platform());
    assert_eq!(
        result,
        scratch
            .join("results")
            .join(format!("{date}-{platform}.json"))
    );
    assert!(value(&values, "started_at").starts_with(&format!("\"{date}T")));
    assert_eq!(value(&values, "subject"), "\"du\"");
    assert_eq!(value(&values, "harness.runs"), "3");
    assert_eq!(value(&values, "harness.cold_command"), "null");

    assert_eq!(value(&values, "trees[0].name"), "\"T1\"");
    assert_eq!(value(&values, "trees[0].dirs"), "20");
    assert_eq!(value(&values, "trees[0].files"), "80");
    assert_eq!(
        value(&values, "trees[0].digest"),
        format!("\"{}\"", TREES[0].1)
    );
    for (i, (name, source)) in [("du", "system"), ("find", "command")].iter().enumerate() {
        assert_eq!(
            value(&values, &format!("tools[{i}].name")),
            format!("\"{name}\"")
        );
        assert_eq!(
            value(&values, &format!("tools[{i}].source")),
            format!("\"{source}\"")
        );
        assert_eq!(
            value(&values, &format!("tools[{i}].status")),
            "\"measured\""
        );
    }

    // The subject's entry comes first; the competitor's carries the pairing.
    assert_eq!(value(&values, "results[0].tool"), "\"du\"");
    assert_eq!(value(&values, "results[0].paired_with"), "null");
    assert_eq!(value(&values, "results[0].command"), "\"du -sk {tree}\"");
    assert_eq!(value(&values, "results[1].tool"), "\"find\"");
    assert_eq!(value(&values, "results[1].paired_with"), "\"du\"");
    assert_eq!(value(&values, "results[1].command"), "\"find {tree}\"");
    assert!(!values.contains_key("results[2].tool"));
    for i in 0..2 {
        let at = |field: &str| number(&values, &format!("results[{i}].{field}"));
        assert_eq!(value(&values, &format!("results[{i}].mode")), "\"warm\"");
        assert_eq!(value(&values, &format!("results[{i}].run_count")), "3");
        assert!(at("min_s") > 0.0);
        assert!(
            at("min_s") <= at("p50_s") && at("p50_s") <= at("p95_s") && at("p95_s") <= at("max_s")
        );
    }
    assert!(number(&values, "results[1].subject_ratio_p50") > 0.0);
}

#[test]
fn p0_bench_01_pairs_alternate_order() {
    let scratch = Scratch::new("pairs");
    small_run(&scratch, &["--runs", "4"]);
    // One warm-up of each at the start of the pair, then the subject first on
    // even rounds and second on odd ones.
    assert_eq!(
        scratch.stub_log(),
        [
            "warmup du",
            "run du",
            "warmup find",
            "run find",
            "run find",
            "run du",
            "run du",
            "run find",
            "run find",
            "run du",
        ]
    );
}

#[test]
fn p0_bench_01_cold_mode_fails_clearly_without_the_cache_drop() {
    for mode in ["cold", "both"] {
        let scratch = Scratch::new("cold-fails");
        let output = scratch.harness(&[
            "--trees",
            "T1",
            "--scale",
            SCALE,
            "--competitors",
            "none",
            "--mode",
            mode,
            "--cold-cmd",
            "false",
        ]);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(1), "--mode {mode}: {stderr}");
        assert!(
            stderr.contains("cold runs drop the filesystem caches")
                && stderr.contains("bench/README.md"),
            "--mode {mode}: the error does not say what cold runs need: {stderr}"
        );
        // It stops before building, measuring or writing anything.
        assert!(!scratch.join("trees").exists(), "--mode {mode} built trees");
        assert!(
            scratch.stub_log().is_empty(),
            "--mode {mode} measured: {:?}",
            scratch.stub_log()
        );
        assert!(
            !scratch.join("results").exists(),
            "--mode {mode} wrote a result"
        );
    }
}

#[test]
fn p0_bench_01_cold_mode_drops_the_caches_before_every_run() {
    let scratch = Scratch::new("cold-runs");
    let result = small_run(
        &scratch,
        &["--mode", "cold", "--cold-cmd", "true", "--runs", "2"],
    );
    // No warm-ups in cold mode; the cache drop precedes every measured run.
    assert_eq!(
        scratch.stub_log(),
        [
            "prepare true",
            "run du",
            "prepare true",
            "run find",
            "prepare true",
            "run find",
            "prepare true",
            "run du",
        ]
    );
    let (_, values) = flatten(&result);
    assert_eq!(value(&values, "harness.cold_command"), "\"true\"");
    assert_eq!(value(&values, "results[0].mode"), "\"cold\"");
    assert_eq!(value(&values, "results[1].mode"), "\"cold\"");
}

/// A tree's entries as (relative path, directory?, size, mtime in ns), sorted.
fn listing(root: &Path) -> Vec<(String, bool, u64, u128)> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, bool, u64, u128)>) {
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let meta = fs::symlink_metadata(entry.path()).unwrap();
            let relative = entry
                .path()
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            let mtime = meta
                .modified()
                .unwrap()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            assert!(
                meta.is_dir() || meta.is_file(),
                "{relative} is neither a file nor a directory"
            );
            let size = if meta.is_dir() { 0 } else { meta.len() };
            out.push((relative, meta.is_dir(), size, mtime));
            if meta.is_dir() {
                walk(root, &entry.path(), out);
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

/// The `"digest"` of a tree's stamp.
fn stamp_digest(stamp: &Path) -> String {
    let text = read(stamp);
    let at = text
        .find("\"digest\": \"")
        .unwrap_or_else(|| panic!("no digest in {text}"))
        + 11;
    text[at..at + 32].to_owned()
}

#[test]
fn p0_bench_01_trees_are_deterministic() {
    let first = Scratch::new("trees-a");
    let second = Scratch::new("trees-b");
    succeeded(
        &first.harness(&["build", "--trees", "all", "--scale", SCALE, "--jobs", "1"]),
        "build",
    );
    succeeded(
        &second.harness(&["build", "--trees", "all", "--scale", SCALE, "--jobs", "3"]),
        "build",
    );
    for (name, digest, dirs, files) in TREES {
        let dirname = format!("{name}-x{SCALE}");
        let a = listing(&first.join("trees").join(&dirname));
        let b = listing(&second.join("trees").join(&dirname));
        assert_eq!(a, b, "{name}: two builds differ");
        assert_eq!(
            a.iter().filter(|e| e.1).count(),
            *dirs,
            "{name}: directories"
        );
        assert_eq!(a.iter().filter(|e| !e.1).count(), *files, "{name}: files");
        // Every time lies in the generator's window, at an even second.
        let base = 1_700_000_000_u128 * 1_000_000_000;
        assert!(a.iter().all(|e| e.3 >= base
            && e.3 < base + 20_000 * 1_000_000_000
            && e.3 % 2_000_000_000 == 0));
        for scratch in [&first, &second] {
            let stamp = scratch.join("trees").join(format!("{dirname}.json"));
            assert_eq!(
                stamp_digest(&stamp),
                *digest,
                "{name}: the digest changed; see TREES"
            );
        }
    }
    succeeded(
        &first.harness(&["verify", "--trees", "all", "--scale", SCALE]),
        "verify",
    );

    // verify notices a tree that changed after it was built.
    let tree = first.join("trees").join(format!("T2-x{SCALE}"));
    let victim = listing(&tree).into_iter().find(|e| !e.1).unwrap().0;
    let file = fs::OpenOptions::new()
        .write(true)
        .open(tree.join(victim))
        .unwrap();
    file.set_modified(UNIX_EPOCH + Duration::from_secs(1_800_000_000))
        .unwrap();
    drop(file);
    let output = first.harness(&["verify", "--trees", "T2", "--scale", SCALE]);
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("DIFFERS FROM its stamp"));

    // A build (as a run does first) does not reuse that tree under its old
    // digest: the walk disagrees with the stamp, so it is rebuilt.
    fs::write(tree.join("added-after-the-build"), b"x").unwrap();
    let output = first.harness(&["build", "--trees", "T2", "--scale", SCALE, "--jobs", "1"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(stderr.contains("changed since it was built"), "{stderr}");
    assert!(!tree.join("added-after-the-build").exists());
    succeeded(
        &first.harness(&["verify", "--trees", "T2", "--scale", SCALE]),
        "verify after the rebuild",
    );
}

#[test]
fn p0_bench_01_trees_dir_must_be_the_harness_own() {
    let scratch = Scratch::new("trees-dir");
    let precious = scratch
        .join("trees")
        .join(format!("T3-x{SCALE}"))
        .join("precious");
    fs::create_dir_all(precious.parent().unwrap()).unwrap();
    fs::write(&precious, b"not the harness's").unwrap();
    let output = scratch.harness(&["build", "--trees", "T3", "--scale", SCALE]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("is not a directory of benchmark trees"),
        "{stderr}"
    );
    assert_eq!(fs::read(&precious).unwrap(), b"not the harness's");
}

/// Seconds since the Unix epoch of an RFC 3339 UTC time, `YYYY-MM-DDTHH:MM:SSZ`.
fn epoch_seconds(time: &str) -> i64 {
    let field = |range: std::ops::Range<usize>| -> i64 { time[range].parse().unwrap() };
    let (y, m, d) = (field(0..4), field(5..7), field(8..10));
    // Days from the civil date (Howard Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    let days = era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468;
    days * 86_400 + field(11..13) * 3_600 + field(14..16) * 60 + field(17..19)
}

#[test]
fn p0_bench_01_result_is_named_by_the_utc_date() {
    assert_eq!(epoch_seconds("2026-10-24T22:30:05Z"), 1_792_881_005);
    // Fourteen hours ahead of UTC: a local time written with a `Z` would be off
    // by that much, and so would a file named by the local date.
    let scratch = Scratch::new("utc");
    let result = small_run_with(&scratch, &[], &[("TZ", "Pacific/Kiritimati")]);
    let (_, values) = flatten(&result);
    let started = value(&values, "started_at").trim_matches('"');
    assert!(started.ends_with('Z'), "{started}");
    let now = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
    .unwrap();
    assert!(
        (now - epoch_seconds(started)).abs() < 600,
        "started_at {started} is not the UTC time of the run"
    );
    let date = value(&values, "date").trim_matches('"');
    assert_eq!(date, &started[..10]);
    assert!(result.ends_with(format!("{date}-{}.json", expected_platform())));
}

#[test]
fn p0_bench_01_cold_mode_refuses_an_empty_cache_drop() {
    let scratch = Scratch::new("cold-empty");
    let output = scratch.harness(&[
        "--trees",
        "T1",
        "--scale",
        SCALE,
        "--competitors",
        "none",
        "--mode",
        "cold",
        "--cold-cmd",
        "",
    ]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("--cold-cmd is empty"), "{stderr}");
    assert!(scratch.stub_log().is_empty(), "{:?}", scratch.stub_log());
    assert!(!scratch.join("results").exists());
}

#[test]
fn p0_bench_01_readme_lists_the_pinned_tools() {
    let scratch = Scratch::new("tools");
    let stdout = succeeded(&scratch.harness(&["tools"]), "bench/run.sh tools");
    let listed: BTreeMap<String, (String, String)> = stdout
        .lines()
        .skip(1)
        .map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            (
                fields[0].to_owned(),
                (fields[1].to_owned(), fields[2].to_owned()),
            )
        })
        .collect();
    let documented: BTreeMap<String, (String, String)> =
        table_rows(&read(&bench("README.md")), "The tools")
            .iter()
            .skip(1)
            .map(|cells| {
                let name = code_span(&cells[0]).unwrap().to_owned();
                let pinned = code_span(&cells[1]).unwrap_or("-").to_owned();
                let command = code_span(&cells[2]).unwrap().to_owned();
                (name, (pinned, command))
            })
            .collect();
    assert_eq!(
        documented, listed,
        "bench/README.md \"The tools\" and bench/tools.py disagree"
    );
    let competitors: BTreeSet<&str> = listed
        .keys()
        .map(String::as_str)
        .filter(|n| *n != "du")
        .collect();
    assert_eq!(
        competitors,
        BTreeSet::from([
            "diskus", "dua", "pdu", "dust", "gdu", "ncdu", "fdu", "dumac"
        ]),
        "the eight competitors plan/PROPOSAL.md pins"
    );
}

#[test]
fn p0_bench_01_python_unit_tests() {
    let output = Command::new(python())
        .args([
            "-B",
            "-E",
            "-s",
            "-m",
            "unittest",
            "discover",
            "-s",
            "bench",
            "-p",
            "test_*.py",
        ])
        .current_dir(repo_root())
        .output()
        .unwrap();
    succeeded(&output, "the unit tests in bench/test_bench.py");
}

#[test]
fn p0_bench_01_just_bench_runs_the_harness() {
    let justfile = read(&repo_root().join("justfile"));
    assert!(
        justfile.contains("\nbench *args:\n    bench/run.sh {{args}}\n"),
        "the justfile's bench recipe must pass its arguments to bench/run.sh"
    );
    let run = bench("run.sh");
    let mode = fs::metadata(&run).unwrap().permissions();
    assert!(
        std::os::unix::fs::PermissionsExt::mode(&mode) & 0o111 != 0,
        "bench/run.sh must be executable"
    );
}

/// Puts an executable `name` holding `script` in `bin`.
fn stub(bin: &Path, name: &str, script: &str) {
    let path = bin.join(name);
    fs::write(&path, script).unwrap();
    let mut mode = fs::metadata(&path).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut mode, 0o755);
    fs::set_permissions(&path, mode).unwrap();
}

/// `bench/drop-caches.sh --remount`, on a stand-in macOS: `uname`, `id`,
/// `sudo`, `hdiutil` and `mount` are stubs, the last listing the image's
/// volume at the mount point when it is attached.
#[test]
fn p0_bench_01_macos_cold_runs_remount_the_trees_disk_image() {
    let scratch = Scratch::new("remount");
    let bin = scratch.join("bin");
    let mnt = scratch.join("mnt");
    fs::create_dir_all(&bin).unwrap();
    fs::create_dir_all(&mnt).unwrap();
    let mnt_real = fs::canonicalize(&mnt).unwrap();
    let calls = scratch.join("calls.log");
    let recorder = |name: &str| format!("#!/bin/sh\necho \"{name} $*\" >> \"$CALLS\"\n");
    stub(&bin, "uname", "#!/bin/sh\necho Darwin\n");
    stub(&bin, "id", "#!/bin/sh\necho 501\n");
    stub(&bin, "sudo", &recorder("sudo"));
    stub(&bin, "hdiutil", &recorder("hdiutil"));
    stub(
        &bin,
        "mount",
        "#!/bin/sh\n[ -z \"$MOUNTED\" ] || echo \"/dev/disk9s1 on $MOUNTED (apfs, local, journaled)\"\n",
    );
    let run = |args: &[&str], mounted: Option<&Path>| -> (Output, Vec<String>) {
        let _ = fs::remove_file(&calls);
        let mut path = OsString::from(bin.as_os_str());
        path.push(":");
        path.push(std::env::var_os("PATH").unwrap());
        let out = Command::new(bench("drop-caches.sh"))
            .args(args)
            .env("PATH", path)
            .env("CALLS", &calls)
            .env("MOUNTED", mounted.map(Path::as_os_str).unwrap_or_default())
            .output()
            .unwrap();
        let logged = fs::read_to_string(&calls)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect();
        (out, logged)
    };
    let image = "/Users/runner/duscope-bench/trees.sparsebundle";
    let m = mnt.to_str().unwrap();
    let purge = "sudo -n /usr/sbin/purge".to_owned();
    let attach =
        format!("hdiutil attach -quiet -nobrowse -noverify -noautoopen -mountpoint {m} {image}");

    // Attached: detached, purged, attached again.
    let (out, logged) = run(&["--remount", image, m], Some(&mnt_real));
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        logged,
        [
            format!("hdiutil detach -quiet {m}"),
            purge.clone(),
            attach.clone()
        ]
    );
    // Not attached, as after a reboot: purged and attached.
    let (out, logged) = run(&["--remount", image, m], None);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(logged, [purge.clone(), attach]);
    // Without --remount, the purge alone; a short --remount is refused.
    assert_eq!(run(&[], None).1, [purge]);
    let (out, logged) = run(&["--remount", image], None);
    assert_eq!(out.status.code(), Some(2));
    assert!(logged.is_empty());

    // Linux drops every cache: --remount is refused there.
    stub(&bin, "uname", "#!/bin/sh\necho Linux\n");
    let (out, logged) = run(&["--remount", image, m], None);
    assert_eq!(out.status.code(), Some(2));
    assert!(logged.is_empty());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("--remount is for a macOS disk image"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
