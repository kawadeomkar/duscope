//! P1-CLI-03: the phase-1 verbs on the arena: `--json` goldens on the
//! fixture tree under the manifest's `DUSCOPE_TEST_NOW`, `stale --atime` on
//! a basic scan, `--strict`, and a closed pipe under long output. The
//! cut-through example is a query test (`duscope-core`, `query/tests.rs`).

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fs;
use std::io::{BufRead, BufReader};
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::Stdio;

use assert_cmd::Command;
use duscope_core::query::timegrammar;
use duscope_testkit::{TestEnv, golden, replace_values};

/// The shared normalisation, and the files' ctimes: a fixture's ctime is
/// when the tree was built.
fn normalize(text: &str, root: &Path) -> String {
    let mut text = duscope_testkit::normalize(text, root);
    replace_values(&mut text, "\"ctime\":\"", '"', "<built>");
    text
}

fn duscope(env: &TestEnv) -> Command {
    Command::from_std(env.cmd())
}

fn json(env: &TestEnv, args: &[&str]) -> (String, serde_json::Value) {
    let out = duscope(env).args(args).arg("--json").output().unwrap();
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).unwrap();
    let value = serde_json::from_str(&text).unwrap();
    (text, value)
}

#[test]
fn p1_cli_03_json_goldens_for_the_four_verbs() {
    let env = TestEnv::new();
    let root = fs::canonicalize(env.fixtures().path("home")).unwrap();
    let home = root.to_str().unwrap();
    let os = std::env::consts::OS;
    // stale needs a file's ctime before its cutoff too, and a fixture's
    // ctime is when the tree was built (only its mtime is set): the stale
    // golden holds while the tree is built more than a year before the
    // manifest's instant, DUSCOPE_TEST_NOW.
    let now = timegrammar::parse_date(env.test_now()).unwrap().ns;
    let cutoff = now - timegrammar::parse_age("1y").unwrap();
    let built = fs::metadata(&root)
        .unwrap()
        .ctime()
        .saturating_mul(1_000_000_000);
    assert!(
        built < cutoff,
        "the fixture tree was built after {}, so stale lists none of it: move the manifest's instant later",
        timegrammar::iso8601(cutoff)
    );
    let cases: [(&str, Vec<&str>); 4] = [
        ("top", vec!["top", home, "--depth", "2", "--limit", "10"]),
        (
            "tree",
            vec![
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
        ("by_type", vec!["by-type", home, "--examples", "2"]),
        (
            "stale",
            vec![
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
    for (name, mut args) in cases {
        args.extend(["--threads", "4"]);
        let (text, value) = json(&env, &args);
        assert_eq!(value["query"]["command"], args[0]);
        golden(
            &format!("{name}_json_verbs@{os}.snap"),
            &normalize(&text, &root),
        );
    }
}

#[test]
fn p1_cli_03_stale_atime_on_a_basic_scan_returns_attrs_not_collected() {
    let env = TestEnv::new();
    let root = env.fixtures().case_root("timestamps");
    let out = duscope(&env)
        .args(["stale", "--atime", "--json"])
        .arg(&root)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let envelope: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(envelope["error"]["code"], "attrs_not_collected");
    // With --attrs full it answers.
    let (_, value) = json(
        &env,
        &[
            "stale",
            "--atime",
            "--attrs",
            "full",
            "--min-size",
            "0",
            root.to_str().unwrap(),
        ],
    );
    assert!(
        value["entries"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["atime"].is_string())
    );
}

#[test]
fn p1_cli_03_filters_are_reported_and_strict_fails_partial_reads() {
    let env = TestEnv::new();
    let fixtures = env.fixtures();
    let home = fs::canonicalize(fixtures.path("home")).unwrap();
    let (_, value) = json(
        &env,
        &[
            "top",
            home.to_str().unwrap(),
            "--files",
            "--glob",
            "*.mov",
            "--min-size",
            "1M",
            "--newer-than",
            "1y",
        ],
    );
    assert_eq!(
        value["query"]["filters"],
        serde_json::json!(["--glob=*.mov", "--min-size=1048576", "--newer-than=1y"])
    );
    assert!(value["entries"].as_array().unwrap().iter().all(|r| {
        Path::new(r["path"].as_str().unwrap())
            .extension()
            .is_some_and(|e| e == "mov")
    }));
    if fixtures.unreadable_effective() {
        let unreadable = fixtures.case_root("unreadable");
        duscope(&env)
            .args(["top", "--strict"])
            .arg(&unreadable)
            .assert()
            .code(4);
        duscope(&env).arg("top").arg(&unreadable).assert().code(0);
    }
}

#[test]
fn p1_cli_03_a_closed_pipe_under_long_output_exits_zero() {
    let env = TestEnv::new();
    // Every file of the fixture tree, one ndjson line each: far more than
    // a pipe holds, so the writer meets EPIPE.
    let mut child = env
        .cmd()
        .args(["top", "--files", "--format", "ndjson"])
        .arg(env.fixtures().root())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut first = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut first)
        .unwrap();
    // The read end closes here, with the rest unread.
    let out = child.wait_with_output().unwrap();
    assert!(first.starts_with("{\"type\":\"scan\""), "{first}");
    assert_eq!(out.status.code(), Some(0));
    // The footer goes to stderr in machine formats; nothing else does.
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.lines().count() <= 1, "{stderr}");
}

#[test]
fn p1_cli_03_top_reports_the_depth_it_listed_and_takes_a_file_root() {
    let env = TestEnv::new();
    let fixtures = env.fixtures();
    let tree = fixtures.case_root("empty");
    // --depth 0 lists depth 1, and says so.
    let (_, top) = json(&env, &["top", "--depth", "0", tree.to_str().unwrap()]);
    assert_eq!(top["query"]["depth"], 1);
    // A file as the root: one row for top --files and tree, none for top.
    let file = fixtures.case_root("symlinks").join("target.txt");
    let file = file.to_str().unwrap();
    let (_, files) = json(&env, &["top", "--files", file]);
    assert_eq!(files["total_count"], 1);
    let (_, dirs) = json(&env, &["top", file]);
    assert_eq!(dirs["total_count"], 0);
    let (_, rows) = json(&env, &["tree", file]);
    assert_eq!(rows["entries"][0]["kind"], "file");
}

#[test]
fn p1_cli_03_by_type_merges_a_classes_file_over_the_vendored_one() {
    let env = TestEnv::new();
    let tree = env.fixtures().case_root("extensions");
    let tree = tree.to_str().unwrap();
    // The class whose examples hold `name`, and its file count.
    let class_of = |value: &serde_json::Value, name: &str| {
        value["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| {
                e["largest"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|x| x["path"].as_str().unwrap().ends_with(&format!("/{name}")))
            })
            .map(|e| {
                (
                    e["class"].as_str().unwrap().to_owned(),
                    e["file_count"].clone(),
                )
            })
            .unwrap()
    };
    let (_, vendored) = json(&env, &["by-type", "--examples", "30", tree]);
    assert_eq!(class_of(&vendored, "run.sh").0, "code");
    // A user's table: its rule wins, the vendored rules it does not name
    // stay.
    let classes = env.tmp_dir().join("classes.toml");
    fs::write(&classes, "[ext]\nsh = \"media\"\n").unwrap();
    let classes = classes.to_str().unwrap();
    let (_, merged) = json(
        &env,
        &["by-type", "--examples", "30", "--classes", classes, tree],
    );
    assert_eq!(class_of(&merged, "run.sh").0, "media");
    assert_eq!(
        class_of(&merged, "PHOTO.JPG"),
        ("media".to_owned(), 3.into())
    );
    assert_eq!(class_of(&merged, "a.tar.gz").0, "archives");
    // A malformed table is a usage error naming the file and the line.
    let bad = env.tmp_dir().join("bad.toml");
    fs::write(&bad, "[ext]\nsh = \"media\"\nrs = \"sideways\"\n").unwrap();
    let out = duscope(&env)
        .args(["by-type", "--json", "--classes"])
        .arg(&bad)
        .arg(tree)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let envelope: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(envelope["error"]["code"], "usage");
    let message = envelope["error"]["message"].as_str().unwrap();
    assert!(message.contains("bad.toml:3:"), "{message}");
}

#[test]
fn p1_cli_03_date_filters_are_reported_and_a_missing_zone_warns() {
    let env = TestEnv::new();
    let tree = env.fixtures().case_root("timestamps");
    let tree = tree.to_str().unwrap();
    let (_, value) = json(
        &env,
        &[
            "top",
            "--files",
            "--since",
            "2020-01-01",
            "--until",
            "2036-12-31T00:00:00Z",
            "--time",
            "ctime",
            tree,
        ],
    );
    assert_eq!(
        value["query"]["filters"],
        serde_json::json!([
            "--since=2020-01-01",
            "--until=2036-12-31T00:00:00Z",
            "--time=ctime"
        ])
    );
    assert_eq!(value["warnings"], serde_json::json!([]));
    // Without zoneinfo for the local zone, a YYYY-MM-DD date is read as
    // UTC midnight and tz_unavailable says so; RFC 3339 needs no zone.
    let out = duscope(&env)
        .env("TZ", "Nowhere/Bogus")
        .args(["top", "--files", "--json", "--since", "2020-01-01", tree])
        .output()
        .unwrap();
    assert!(out.status.success());
    let envelope: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let codes: Vec<&str> = envelope["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["code"].as_str().unwrap())
        .collect();
    assert_eq!(codes, ["tz_unavailable"]);
    let out = duscope(&env)
        .env("TZ", "Nowhere/Bogus")
        .args([
            "top",
            "--files",
            "--json",
            "--since",
            "2020-01-01T00:00:00Z",
            tree,
        ])
        .output()
        .unwrap();
    let envelope: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(envelope["warnings"], serde_json::json!([]));
}

#[test]
fn p1_cli_03_a_limit_above_100_is_a_usage_error() {
    let env = TestEnv::new();
    let tree = env.fixtures().case_root("empty");
    for verb in ["top", "tree", "by-type", "stale"] {
        // Refused before the scan: a missing path does not get that far.
        let out = duscope(&env)
            .args([verb, "--json", "--limit", "101", "/nonexistent/duscope"])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "{verb}");
        let envelope: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(envelope["error"]["code"], "usage");
        let hint = envelope["error"]["hint"].as_str().unwrap();
        assert!(hint.contains("--format ndjson"), "{hint}");
        duscope(&env)
            .args([verb, "--limit", "100"])
            .arg(&tree)
            .assert()
            .code(0);
    }
    // ndjson streams every row, whatever --limit says.
    duscope(&env)
        .args(["top", "--format", "ndjson", "--limit", "500"])
        .arg(&tree)
        .assert()
        .code(0);
}

#[test]
fn p1_cli_03_by_type_plain_carries_the_share_of_the_root() {
    let env = TestEnv::new();
    let out = duscope(&env)
        .args(["by-type", "--plain", "--header"])
        .arg(env.fixtures().case_root("extensions"))
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    let mut lines = text.lines();
    assert_eq!(
        lines.next(),
        Some("allocated_bytes\tapparent_bytes\tpct_of_root\tfile_count\tclass")
    );
    // Each class's share is its bytes over the root's total, which also
    // holds the directories' own blocks (4 KiB each on ext4, none on APFS).
    let scan = duscope(&env)
        .args(["scan", "--json"])
        .arg(env.fixtures().case_root("extensions"))
        .output()
        .unwrap();
    let envelope: serde_json::Value = serde_json::from_slice(&scan.stdout).unwrap();
    let total = envelope["scan"]["total_allocated_bytes"].as_f64().unwrap();
    for line in lines {
        let fields: Vec<&str> = line.split('\t').collect();
        let bytes: f64 = fields[0].parse().unwrap();
        let share: f64 = fields[2].parse().unwrap();
        assert!(
            (share - bytes * 100.0 / total).abs() < 0.1,
            "{line}\n{text}"
        );
    }
}
