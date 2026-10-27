//! P1-CLI-02: the output layer: each format's golden on `top`, `name_raw`,
//! and nothing but the payload on stdout. JSON for an agent on a terminal is
//! in `pty/output.rs`.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

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

fn duscope(env: &TestEnv) -> Command {
    Command::from_std(env.cmd())
}

/// The fixture home tree, its symbolic links resolved as the scan block
/// reports it.
fn home(env: &TestEnv) -> PathBuf {
    fs::canonicalize(env.fixtures().path("home")).unwrap()
}

/// `text` without what differs between runs, machines and releases: the
/// root's path, the scan's times, the volumes (sizes and free space), the
/// version, and the scan's duration in a footer.
fn normalize(text: &str, root: &Path) -> String {
    let mut text = text.replace(root.to_str().unwrap(), "<root>");
    for (key, placeholder) in [
        ("\"started_at\":\"", "<time>"),
        ("\"finished_at\":\"", "<time>"),
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
    let key = "\"volumes\":[";
    let mut from = 0;
    while let Some(at) = text[from..].find(key) {
        let start = from + at + key.len() - 1;
        let mut depth = 0;
        let mut end = start;
        for (i, c) in text[start..].char_indices() {
            match c {
                '[' | '{' => depth += 1,
                ']' | '}' => depth -= 1,
                _ => {}
            }
            if depth == 0 {
                end = start + i + 1;
                break;
            }
        }
        text.replace_range(start..end, "[\"<volumes>\"]");
        from = start;
    }
    if let Some(at) = text.find(" s, ") {
        let start = text[..at].rfind(" · ").unwrap() + " · ".len();
        text.replace_range(start..at, "<secs>");
    }
    text
}

#[test]
fn p1_cli_02_top_goldens_for_each_format() {
    let env = TestEnv::new();
    let root = home(&env);
    let os = std::env::consts::OS;
    let cases: [(&str, &[&str]); 4] = [
        ("json", &["--json"]),
        ("plain", &["--plain", "--header"]),
        ("ndjson", &["--format", "ndjson"]),
        ("table", &["--format", "table"]),
    ];
    for (format, flags) in cases {
        let out = duscope(&env)
            .arg("top")
            .arg(&root)
            .args(["--threads", "4", "--limit", "8"])
            .args(flags)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{format}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8(out.stdout).unwrap();
        golden(
            &format!("top_{format}_home@{os}.snap"),
            &normalize(&stdout, &root),
        );
    }
}

#[test]
fn p1_cli_02_nothing_but_json_on_stdout() {
    let env = TestEnv::new();
    let root = home(&env);
    let out = duscope(&env)
        .args(["top", "--json", "--threads", "4"])
        .arg(&root)
        .output()
        .unwrap();
    assert!(out.status.success());
    // One compact JSON object and a newline: no pretty-printing, no footer.
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert_eq!(stdout.matches('\n').count(), 1, "{stdout}");
    assert!(stdout.ends_with("}\n"));
    let envelope: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["entries"].as_array().unwrap().len(), 13);
    // The footer went to stderr, alone.
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(stderr.contains(" allocated · 43,137 files · "), "{stderr}");

    // A failure is the error envelope on stdout, and nothing on stderr.
    let out = duscope(&env)
        .args(["top", "--json", "/nonexistent/duscope"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    assert!(
        out.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let envelope: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["error"]["code"], "scan_not_found");
    assert_eq!(envelope["error"]["fixable_by"], "agent");

    // So is a usage error when the command line asked for JSON.
    let out = duscope(&env)
        .args(["top", "--json", "--no-such-flag"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(
        out.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let envelope: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(envelope["error"]["code"], "usage");
    let message = envelope["error"]["message"].as_str().unwrap();
    assert!(message.contains("--no-such-flag"), "{message}");
}

#[test]
fn p1_cli_02_an_invalid_utf8_name_yields_name_raw() {
    let env = TestEnv::new();
    let fixtures = env.fixtures();
    // APFS refuses names that are not UTF-8, so the case is built only on
    // Linux.
    if !fixtures.built("non-utf8-names") {
        eprintln!("skipped: the filesystem refuses names that are not UTF-8");
        return;
    }
    let tree = fixtures.path("cases/bytes");
    // `top --files` lists the files; the case's directory is left out.
    let invalid: Vec<Vec<u8>> = fs::read_dir(&tree)
        .unwrap()
        .map(Result::unwrap)
        .filter(|e| !e.file_type().unwrap().is_dir())
        .map(|e| e.file_name())
        .filter(|name| name.to_str().is_none())
        .map(|name| name.as_bytes().to_vec())
        .collect();
    assert!(!invalid.is_empty(), "{}", tree.display());
    let out = duscope(&env)
        .args(["top", "--files", "--json"])
        .arg(&tree)
        .output()
        .unwrap();
    assert!(out.status.success());
    let envelope: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let rows = envelope["entries"].as_array().unwrap();
    for raw in &invalid {
        let row = rows
            .iter()
            .find(|row| row["name"] == String::from_utf8_lossy(raw).as_ref())
            .unwrap_or_else(|| panic!("no row for {raw:?}: {rows:?}"));
        assert_eq!(row["name_raw"], base64(raw));
    }
    // A valid name carries no name_raw at all.
    assert!(
        rows.iter()
            .any(|row| row["name"].as_str().unwrap().is_ascii() && row.get("name_raw").is_none())
    );
    // --plain writes the path's bytes as they are.
    let out = duscope(&env)
        .args(["top", "--files", "--plain"])
        .arg(&tree)
        .output()
        .unwrap();
    for raw in &invalid {
        assert!(out.stdout.windows(raw.len()).any(|w| w == raw.as_slice()));
    }
}

/// Standard base64, to check `name_raw` against.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, &b)| n | u32::from(b) << (16 - 8 * i));
        for i in 0..4 {
            out.push(if i <= chunk.len() {
                char::from(ALPHABET[(n >> (18 - 6 * i)) as usize & 63])
            } else {
                '='
            });
        }
    }
    out
}

#[test]
fn p1_cli_02_plain_records_end_with_nul_under_dash_0() {
    let env = TestEnv::new();
    let root = home(&env);
    let out = duscope(&env)
        .args(["top", "--plain", "--header", "-0", "--limit", "2"])
        .arg(&root)
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(!out.stdout.contains(&b'\n'));
    let records: Vec<&[u8]> = out.stdout.split(|&b| b == 0).collect();
    // The header, two rows, and nothing after the last NUL.
    assert_eq!(
        records.len(),
        4,
        "{:?}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(records[0].starts_with(b"allocated_bytes\tapparent_bytes\t"));
    assert!(records[3].is_empty());
}

#[test]
fn p1_cli_02_sizes_and_colour_for_people() {
    let env = TestEnv::new();
    let root = home(&env);
    let table = |flags: &[&str]| {
        let out = duscope(&env)
            .args(["top", "--format", "table", "--limit", "1"])
            .args(flags)
            .arg(&root)
            .output()
            .unwrap();
        assert!(out.status.success());
        String::from_utf8(out.stdout).unwrap()
    };
    // Not a terminal: no colour unless asked for.
    assert!(!table(&[]).contains('\x1b'));
    assert!(table(&["--color", "always"]).starts_with("\x1b[1m"));
    assert!(!table(&["--no-color"]).contains('\x1b'));
    // IEC by default, powers of 1,000 with --si, bytes with -b.
    assert!(table(&[]).contains(" MiB "));
    assert!(table(&["--si"]).contains(" MB "));
    let bytes = table(&["-b"]);
    let size = bytes
        .lines()
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap();
    assert!(size.chars().all(|c| c.is_ascii_digit()), "{bytes}");
}

#[test]
fn p1_cli_02_output_flags_go_before_or_after_the_verb() {
    let env = TestEnv::new();
    let tree = env.fixtures().case_root("empty");
    for args in [&["--json", "top"][..], &["top", "--json"]] {
        let out = duscope(&env).args(args).arg(&tree).output().unwrap();
        assert!(out.status.success(), "{args:?}");
        let envelope: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(envelope["query"]["command"], "top", "{args:?}");
    }
    // Before a verb, scan's own flags and path are refused: they would be
    // dropped.
    let out = duscope(&env)
        .args(["-x", "top"])
        .arg(&tree)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--one-file-system comes before the verb"),
        "{stderr}"
    );
    let out = duscope(&env).arg(&tree).arg("top").output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    // Without a verb they scan, as before.
    duscope(&env).arg("-x").arg(&tree).assert().success();
}

#[test]
fn p1_cli_02_a_closed_stderr_is_no_panic() {
    let env = TestEnv::new();
    let root = home(&env);
    // stdout and stderr into one pipe whose reader leaves after a byte:
    // the footer then meets a closed stderr.
    let mut child = env
        .cmd()
        .args(["top", "--format", "ndjson", "--threads", "4"])
        .arg(&root)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    drop(child.stderr.take());
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(0));
    // A failure whose stderr is closed is still its exit code.
    let mut child = env
        .cmd()
        .args(["top", "--plain", "/nonexistent/duscope"])
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stderr.take());
    assert_eq!(child.wait().unwrap().code(), Some(3));
}

#[test]
fn p1_cli_02_a_usage_error_ranks_json_first() {
    let env = TestEnv::new();
    // --json and --plain conflict; the decision ranks --json first, so the
    // usage error is the envelope.
    let out = duscope(&env)
        .args(["top", "--plain", "--json", "--no-such-flag"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let envelope: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(envelope["error"]["code"], "usage");
}
