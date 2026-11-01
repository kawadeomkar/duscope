//! P2-QRY-02: the phase-1 verbs on a saved scan answer as on the scan run
//! for them, and match the phase-1 goldens; pages join into the list;
//! cursors of another query or scan are refused; `--fields`, `--detail`,
//! `-o FILE`, `--max-chars` and `--all`.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Output;

use duscope_testkit::{TestEnv, golden, replace_values};
use serde_json::Value;

fn run(env: &TestEnv, args: &[&str]) -> Output {
    env.cmd().args(args).output().unwrap()
}

fn ok(out: &Output) -> Value {
    assert!(
        out.status.success(),
        "exit {:?}: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

fn json(env: &TestEnv, args: &[&str]) -> Value {
    let mut args = args.to_vec();
    args.push("--json");
    ok(&run(env, &args))
}

fn error_code(out: &Output) -> String {
    let value: Value = serde_json::from_slice(&out.stdout).unwrap();
    value["error"]["code"].as_str().unwrap().to_owned()
}

/// The home tree, saved; its root and the scan's id.
fn saved_home(env: &TestEnv) -> (String, String) {
    let home = fs::canonicalize(env.fixtures().path("home")).unwrap();
    let home = home.to_str().unwrap().to_owned();
    let saved = json(env, &["scan", &home, "--save", "--threads", "4"]);
    (home, saved["scan"]["id"].as_str().unwrap().to_owned())
}

/// P1-CLI-03's normalisation: the shared one, and the files' ctimes.
fn normalize(text: &str, root: &Path) -> String {
    let mut text = duscope_testkit::normalize(text, root);
    replace_values(&mut text, "\"ctime\":\"", '"', "<built>");
    text
}

#[test]
fn p2_qry_02_a_saved_scan_answers_the_phase_1_goldens() {
    let env = TestEnv::new();
    let (home, id) = saved_home(&env);
    let os = std::env::consts::OS;
    // P1-CLI-03's cases (duscope/tests/verbs.rs), read from the index.
    let cases: [(&str, Vec<&str>); 4] = [
        ("top", vec!["top", &home, "--depth", "2", "--limit", "10"]),
        (
            "tree",
            vec![
                "tree",
                &home,
                "--depth",
                "2",
                "--min-pct",
                "5",
                "--limit",
                "20",
            ],
        ),
        ("by_type", vec!["by-type", &home, "--examples", "2"]),
        (
            "stale",
            vec![
                "stale",
                &home,
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
        args.extend(["--scan", &id, "--json"]);
        let out = run(&env, &args);
        ok(&out);
        let text = String::from_utf8(out.stdout).unwrap();
        golden(
            &format!("{name}_json_verbs@{os}.snap"),
            &normalize(&text, Path::new(&home)),
        );
    }
}

#[test]
fn p2_qry_02_a_saved_scan_answers_as_a_fresh_one() {
    let env = TestEnv::new();
    let (home, id) = saved_home(&env);
    // A fresh scan: an empty index directory and nothing saved.
    let empty = env.tmp_dir().join("empty-index");
    fs::create_dir_all(&empty).unwrap();
    let empty = empty.to_str().unwrap();
    for args in [
        vec!["top", home.as_str(), "--depth", "3", "--limit", "100"],
        vec!["top", home.as_str(), "--files", "--limit", "100"],
        vec!["top", home.as_str(), "--files", "--glob", "*.jpg"],
        vec!["tree", home.as_str(), "--depth", "3", "--limit", "100"],
        vec!["by-type", home.as_str(), "--examples", "3"],
        vec![
            "by-type",
            home.as_str(),
            "--examples",
            "3",
            "--sort",
            "count",
        ],
        vec![
            "stale",
            home.as_str(),
            "--older-than",
            "30d",
            "--min-size",
            "0",
        ],
        vec!["caches", home.as_str(), "--min-size", "0", "--limit", "100"],
    ] {
        // Whole envelopes, however long.
        let mut args = args.clone();
        args.extend(["--max-chars", "0"]);
        let mut saved_args = args.clone();
        saved_args.extend(["--scan", &id]);
        let saved = json(&env, &saved_args);
        let mut fresh_args = args.clone();
        fresh_args.extend(["--index-dir", empty, "--no-save", "--threads", "4"]);
        let fresh = json(&env, &fresh_args);
        for key in ["entries", "total_count", "truncated", "query"] {
            assert_eq!(saved[key], fresh[key], "{args:?}: {key}");
        }
        assert!(
            saved["scan"]["id"].is_string(),
            "{args:?}: {}",
            saved["scan"]
        );
        assert!(fresh["scan"]["id"].is_null(), "{args:?}: {}", fresh["scan"]);
    }
}

/// The paths of the first `want` rows of `args`, read in pages of 7, and
/// whether every page agreed on the total and on whether rows remained.
fn paged(env: &TestEnv, args: &[&str], want: usize) -> Vec<String> {
    let mut rows = Vec::new();
    let mut cursor: Option<String> = None;
    let mut total = None;
    while rows.len() < want {
        let mut page_args = args.to_vec();
        page_args.extend(["--limit", "7"]);
        if let Some(cursor) = &cursor {
            page_args.extend(["--cursor", cursor]);
        }
        let page = json(env, &page_args);
        let entries = page["entries"].as_array().unwrap();
        rows.extend(entries.iter().map(|e| {
            e["path"]
                .as_str()
                .or_else(|| e["class"].as_str())
                .unwrap()
                .to_owned()
        }));
        let count = page["total_count"].as_u64().unwrap();
        assert_eq!(*total.get_or_insert(count), count, "{args:?}");
        cursor = page["next_cursor"].as_str().map(str::to_owned);
        assert_eq!(page["truncated"], cursor.is_some(), "{args:?}");
        if cursor.is_none() {
            assert_eq!(rows.len() as u64, count, "{args:?}");
            break;
        }
    }
    rows
}

#[test]
fn p2_qry_02_pages_join_into_the_list() {
    let env = TestEnv::new();
    let (home, _) = saved_home(&env);
    for args in [
        vec!["top", home.as_str(), "--files"],
        vec!["top", home.as_str(), "--files", "--sort", "path", "-r"],
        vec!["top", home.as_str(), "--files", "--glob", "*.jpg"],
        vec!["top", home.as_str(), "--depth", "3"],
        vec![
            "stale",
            home.as_str(),
            "--older-than",
            "1y",
            "--min-size",
            "0",
        ],
        vec!["caches", home.as_str(), "--min-size", "0"],
    ] {
        let mut whole = args.clone();
        whole.extend(["--limit", "60"]);
        let list: Vec<String> = json(&env, &whole)["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["path"].as_str().unwrap().to_owned())
            .collect();
        assert!(!list.is_empty(), "{args:?}");
        let pages = paged(&env, &args, list.len());
        assert_eq!(pages[..list.len()], list[..], "{args:?}");
    }
}

#[test]
fn p2_qry_02_a_cursor_of_another_query_or_scan_is_refused() {
    let env = TestEnv::new();
    let (home, _) = saved_home(&env);
    let page = json(&env, &["top", &home, "--files", "--limit", "2"]);
    let cursor = page["next_cursor"].as_str().unwrap().to_owned();
    // Another query: directories, not files.
    let out = run(
        &env,
        &["top", &home, "--limit", "2", "--cursor", &cursor, "--json"],
    );
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(error_code(&out), "invalid_cursor");
    // Another scan.
    let other = json(&env, &["scan", &home, "--save"]);
    let other = other["scan"]["id"].as_str().unwrap();
    let out = run(
        &env,
        &[
            "top", &home, "--files", "--limit", "2", "--cursor", &cursor, "--scan", other, "--json",
        ],
    );
    assert_eq!(error_code(&out), "invalid_cursor");
    // Not a cursor; a verb that does not page.
    let out = run(&env, &["top", &home, "--cursor", "bogus!", "--json"]);
    assert_eq!(error_code(&out), "invalid_cursor");
    let out = run(&env, &["tree", &home, "--cursor", &cursor, "--json"]);
    assert_eq!(error_code(&out), "invalid_cursor");
    // The cursor itself still pages.
    let next = json(
        &env,
        &["top", &home, "--files", "--limit", "2", "--cursor", &cursor],
    );
    assert_eq!(next["entries"].as_array().unwrap().len(), 2);
}

#[test]
fn p2_qry_02_fields_choose_the_fields_and_their_order() {
    let env = TestEnv::new();
    let (home, _) = saved_home(&env);
    let listed = json(
        &env,
        &[
            "top",
            &home,
            "--files",
            "--limit",
            "3",
            "--fields",
            "allocated_bytes,name",
        ],
    );
    let text = serde_json::to_string(&listed["entries"][0]).unwrap();
    assert!(text.starts_with("{\"allocated_bytes\":"), "{text}");
    assert_eq!(listed["entries"][0].as_object().unwrap().len(), 2);
    // --plain writes them in that order; --detail writes them all.
    let out = run(
        &env,
        &[
            "top",
            &home,
            "--limit",
            "2",
            "--plain",
            "--header",
            "--fields",
            "name,path",
        ],
    );
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.starts_with("name\tpath\n"), "{text}");
    let out = run(
        &env,
        &[
            "top", &home, "--limit", "2", "--plain", "--header", "--detail",
        ],
    );
    let header = String::from_utf8(out.stdout).unwrap();
    assert!(
        header.starts_with("path\tname\tname_raw\tkind\tdepth\t"),
        "{header}"
    );
    // help lists them; an unknown one is refused before any scan.
    let help = json(&env, &["stale", "--fields", "help"]);
    assert!(help["fields"].as_array().unwrap().contains(&"atime".into()));
    let out = run(
        &env,
        &["top", "/nonexistent/duscope", "--fields", "nope", "--json"],
    );
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(error_code(&out), "unknown_field");
    // A basic scan has no access times to give.
    let out = run(&env, &["stale", &home, "--fields", "path,atime", "--json"]);
    assert_eq!(error_code(&out), "attrs_not_collected");
}

#[test]
fn p2_qry_02_a_file_takes_more_rows_than_a_list() {
    let env = TestEnv::new();
    let (home, _) = saved_home(&env);
    let file = env.tmp_dir().join("top.json");
    let out = run(
        &env,
        &[
            "top",
            &home,
            "--files",
            "--limit",
            "150",
            "-o",
            file.to_str().unwrap(),
            "--json",
        ],
    );
    assert!(out.status.success());
    assert!(out.stdout.is_empty());
    let written: Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
    assert_eq!(written["entries"].as_array().unwrap().len(), 150);
    // Without -o, 150 is refused.
    let out = run(&env, &["top", &home, "--limit", "150", "--json"]);
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn p2_qry_02_a_long_envelope_spills_to_a_file() {
    let env = TestEnv::new();
    let (home, _) = saved_home(&env);
    let args = ["top", home.as_str(), "--files", "--limit", "100"];
    let mut spilled_args = args.to_vec();
    spilled_args.extend(["--max-chars", "2000"]);
    let stub = json(&env, &spilled_args);
    assert_eq!(stub["ok"], true);
    assert_eq!(stub["spilled"]["rows"], 100);
    assert_eq!(stub["preview"].as_array().unwrap().len(), 5);
    assert!(stub["entries"].is_null());
    assert!(
        stub["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["code"] == "spilled")
    );
    let path = Path::new(stub["spilled"]["path"].as_str().unwrap());
    assert!(path.starts_with(env.index_dir().join("out")), "{path:?}");
    let meta = fs::metadata(path).unwrap();
    assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    assert_eq!(meta.len(), stub["spilled"]["bytes"].as_u64().unwrap());
    let whole: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert_eq!(whole["entries"].as_array().unwrap().len(), 100);
    assert_eq!(whole["entries"][0], stub["preview"][0]);
    // The default holds 24,000 characters; 0 never spills.
    let mut never = args.to_vec();
    never.extend(["--max-chars", "0"]);
    assert_eq!(json(&env, &never)["entries"].as_array().unwrap().len(), 100);
}

#[test]
fn p2_qry_02_all_streams_every_row() {
    let env = TestEnv::new();
    let (home, _) = saved_home(&env);
    let total = json(&env, &["top", &home, "--files", "--limit", "1"])["total_count"]
        .as_u64()
        .unwrap();
    let out = run(&env, &["top", &home, "--files", "--all"]);
    assert!(out.status.success());
    let lines = String::from_utf8(out.stdout).unwrap();
    // The scan line, every row, and the end line.
    assert_eq!(lines.lines().count() as u64, total + 2);
    let out = run(&env, &["top", &home, "--files", "--all", "--json"]);
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn p2_qry_02_what_the_review_found() {
    let env = TestEnv::new();
    let (home, _) = saved_home(&env);
    // --limit 0: no row, and rows remain.
    let none = json(&env, &["top", &home, "--files", "--limit", "0"]);
    assert_eq!(none["returned_count"], 0);
    assert_eq!(none["truncated"], true);
    // A field named twice is written once; no field at all is refused.
    let twice = json(
        &env,
        &["top", &home, "--limit", "1", "--fields", "path,path,name"],
    );
    let text = serde_json::to_string(&twice["entries"][0]).unwrap();
    assert_eq!(text.matches("\"path\"").count(), 1, "{text}");
    let out = run(&env, &["top", &home, "--fields", ",", "--json"]);
    assert_eq!(out.status.code(), Some(2));
    // A spilled envelope's preview has the chosen fields only.
    let stub = json(
        &env,
        &[
            "top",
            &home,
            "--files",
            "--limit",
            "100",
            "--fields",
            "path",
            "--max-chars",
            "500",
        ],
    );
    assert_eq!(stub["preview"][0].as_object().unwrap().len(), 1);
    // A cursor whose count of rows seen was tampered with pages without
    // a panic, and ends the list.
    let page = json(&env, &["top", &home, "--files", "--limit", "2"]);
    let cursor = page["next_cursor"].as_str().unwrap();
    let text = decode(cursor);
    let mut wire: Value = serde_json::from_slice(&text).unwrap();
    wire["seen"] = u64::MAX.into();
    let forged = encode(&serde_json::to_vec(&wire).unwrap());
    let next = json(
        &env,
        &["top", &home, "--files", "--limit", "2", "--cursor", &forged],
    );
    assert_eq!(next["truncated"], false);
    assert!(next["next_cursor"].is_null());
}

const URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// base64url without padding, as cursors are written.
fn encode(bytes: &[u8]) -> String {
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..=chunk.len() {
            out.push(char::from(URL[(n >> (18 - 6 * i)) as usize & 63]));
        }
    }
    out
}

/// The bytes of base64url `text`.
fn decode(text: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let (mut acc, mut bits) = (0u32, 0);
    for byte in text.bytes() {
        let value = URL.iter().position(|&a| a == byte).unwrap();
        acc = (acc << 6) | u32::try_from(value).unwrap();
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(u8::try_from((acc >> bits) & 0xff).unwrap());
        }
    }
    out
}
