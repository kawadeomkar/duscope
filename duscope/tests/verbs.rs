//! P1-CLI-03: the phase-1 verbs on the arena: `--json` goldens on the
//! fixture tree under the manifest's `DUSCOPE_TEST_NOW`, cut-through,
//! `stale --atime` on a basic scan, `--strict`, and a closed pipe under
//! long output.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::Stdio;

use assert_cmd::Command;
use duscope_testkit::TestEnv;

/// The golden file `name` under `duscope/tests/golden/`, compared with
/// `actual`; with `DUSCOPE_TEST_UPDATE_GOLDEN=1` it is rewritten instead.
fn golden(name: &str, actual: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name);
    if std::env::var_os("DUSCOPE_TEST_UPDATE_GOLDEN").is_some_and(|v| v == "1") {
        fs::write(&path, actual).unwrap();
        return;
    }
    let expected = fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert_eq!(
        actual, expected,
        "golden {name} differs; rerun with DUSCOPE_TEST_UPDATE_GOLDEN=1 to regenerate"
    );
}

/// `text` without what differs between runs, machines and releases: the
/// root's path, the scan's times, the volumes, the version, and the inode
/// change times, which are when the fixture tree was built.
fn normalize(text: &str, root: &Path) -> String {
    let mut text = text.replace(root.to_str().unwrap(), "<root>");
    for (key, placeholder) in [
        ("\"started_at\":\"", "<time>"),
        ("\"finished_at\":\"", "<time>"),
        ("\"ctime\":\"", "<built>"),
        ("\"name\":\"duscope\",\"version\":\"", "<version>"),
    ] {
        let mut from = 0;
        while let Some(at) = text[from..].find(key) {
            let start = from + at + key.len();
            let end = start + text[start..].find('"').unwrap();
            text.replace_range(start..end, placeholder);
            from = start;
        }
    }
    if let Some(at) = text.find("\"volumes\":[") {
        let start = at + "\"volumes\":".len();
        let end = start + text[start..].find("}]").unwrap() + 2;
        text.replace_range(start..end, "[\"<volumes>\"]");
    }
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
fn p1_cli_03_cut_through_reproduces_the_library_containers_example() {
    let env = TestEnv::new();
    let home = env.tmp_dir().join("home");
    // Library holds a Containers of more than 80% of its bytes; Movies
    // has no subdirectory.
    for (file, size) in [
        ("Library/Containers/com.example/data.db", 9 << 20),
        ("Library/Preferences/p.plist", 1 << 20),
        ("Movies/a.mov", 5 << 20),
    ] {
        let path = home.join(file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, vec![7u8; size]).unwrap();
    }
    let home = fs::canonicalize(home).unwrap();
    let (_, value) = json(&env, &["top", home.to_str().unwrap()]);
    let rows = value["entries"].as_array().unwrap();
    let paths: Vec<&str> = rows.iter().map(|r| r["path"].as_str().unwrap()).collect();
    let library = format!("{}/Library", home.display());
    assert_eq!(
        paths,
        [
            format!("{library}/Containers/com.example"),
            format!("{}/Movies", home.display())
        ]
    );
    assert_eq!(
        rows[0]["via"],
        serde_json::json!([library.clone(), format!("{library}/Containers")])
    );
    assert_eq!(rows[1]["via"], serde_json::json!([]));
    // --no-cut-through keeps strict depth.
    let (_, strict) = json(&env, &["top", home.to_str().unwrap(), "--no-cut-through"]);
    assert_eq!(strict["entries"][0]["path"], library);
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
