//! P5-CLI-01: `path_raw`. A path whose bytes are not UTF-8 (an ancestor's
//! name, or its own) reaches JSON as lossy text, which names no file; every
//! object with a `path` carries the bytes in base64 beside it, so that a
//! consumer can pass the path back. Linux alone: APFS refuses names that are
//! not UTF-8, and the fixture case records itself as skipped there.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use duscope_testkit::TestEnv;
use serde_json::Value;

/// `duscope ARGS`: its stdout as JSON, whatever the exit code (a dry run of
/// `clean` exits 1 with its plan).
fn json(env: &TestEnv, args: &[&OsStr]) -> Value {
    let out = env.cmd().args(args).output().unwrap();
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "{args:?}: {e}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

fn os(text: &str) -> &OsStr {
    OsStr::new(text)
}

/// Standard base64 (RFC 4648, padded), to check `path_raw` against.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, &b)| n | (u32::from(b) << (16 - 8 * i)));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[(n >> (18 - 6 * i)) as usize & 63]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// `text` decoded from standard base64.
fn unbase64(text: &str) -> Vec<u8> {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut bits = 0u32;
    let mut held = 0;
    let mut out = Vec::new();
    for c in text.bytes().filter(|&c| c != b'=') {
        let value = ALPHABET.iter().position(|&a| a == c).unwrap();
        bits = (bits << 6) | u32::try_from(value).unwrap();
        held += 6;
        if held >= 8 {
            held -= 8;
            out.push(u8::try_from((bits >> held) & 0xff).unwrap());
        }
    }
    out
}

fn raw(path: &Path) -> String {
    base64(path.as_os_str().as_bytes())
}

fn lossy(path: &Path) -> String {
    String::from_utf8_lossy(path.as_os_str().as_bytes()).into_owned()
}

/// Every object in `value` with a string `path` (or `project_root`), and
/// its `path_raw` (or `project_root_raw`): each one present decodes to bytes
/// whose lossy text is the path, and is present exactly when the text holds
/// a replacement character (none of the names here holds one of its own).
/// The paths seen go to `seen`. The blocks the package leaves lossy
/// (`query`, `scan.excluded`) are not looked in.
fn check_raw(value: &Value, seen: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, twin) in [("path", "path_raw"), ("project_root", "project_root_raw")] {
                let Some(Value::String(text)) = map.get(key) else {
                    continue;
                };
                match map.get(twin) {
                    Some(Value::String(encoded)) => {
                        let bytes = unbase64(encoded);
                        assert!(std::str::from_utf8(&bytes).is_err(), "{map:?}");
                        assert_eq!(&String::from_utf8_lossy(&bytes), text, "{map:?}");
                    }
                    Some(other) => panic!("{twin} is {other}: {map:?}"),
                    None => assert!(!text.contains('\u{fffd}'), "no {twin}: {map:?}"),
                }
                seen.push(text.clone());
            }
            map.iter()
                .filter(|(key, _)| !matches!(key.as_str(), "query" | "excluded"))
                .for_each(|(_, v)| check_raw(v, seen));
        }
        Value::Array(items) => items.iter().for_each(|v| check_raw(v, seen)),
        _ => {}
    }
}

/// [`check_raw`] of `value`, which must name `path` somewhere.
fn checked(value: &Value, path: &Path) {
    let mut seen = Vec::new();
    check_raw(value, &mut seen);
    assert!(
        seen.contains(&lossy(path)),
        "{} not in {value}",
        path.display()
    );
}

/// A tree of its own with a directory whose name is not UTF-8, under the
/// test's temporary directory; `None` where the filesystem refuses the name.
fn bytes_tree(env: &TestEnv) -> Option<PathBuf> {
    let base = fs::canonicalize(env.tmp_dir()).unwrap().join("tree");
    let odd = base.join(OsStr::from_bytes(b"caf\xe9"));
    if fs::create_dir_all(&odd).is_err() {
        return None;
    }
    fs::write(odd.join("package.json"), "{}").unwrap();
    fs::create_dir_all(odd.join("node_modules/pkg")).unwrap();
    fs::write(odd.join("node_modules/pkg/index.js"), vec![0u8; 40_000]).unwrap();
    fs::write(odd.join("notes.txt"), vec![0u8; 20_000]).unwrap();
    fs::create_dir_all(base.join("plain")).unwrap();
    fs::write(base.join("plain/a.txt"), vec![0u8; 10_000]).unwrap();
    Some(base)
}

#[test]
// The fixture's case, through each way a row is written.
#[allow(clippy::too_many_lines)]
fn p5_cli_01_a_non_utf8_ancestor_yields_path_raw() {
    let env = TestEnv::new();
    let fixtures = env.fixtures();
    if !fixtures.built("non-utf8-names") {
        eprintln!("skipped: the filesystem refuses names that are not UTF-8");
        return;
    }
    let case = fs::canonicalize(fixtures.path("cases/bytes")).unwrap();
    // The fixture's directory that is not UTF-8, and the file in it whose
    // own name is.
    let dir = case.join(OsStr::from_bytes(b"dir-\xff"));
    let inside = dir.join("inside.txt");
    assert!(inside.is_file(), "{}", inside.display());
    let saved = json(
        &env,
        &[os("scan"), case.as_os_str(), os("--save"), os("--json")],
    );
    let id = saved["scan"]["id"].as_str().unwrap().to_owned();
    checked(&saved, &dir);

    // top --files: the file's name is UTF-8 and has no name_raw; its path
    // is not, and has path_raw, which names the file.
    let top = json(
        &env,
        &[
            os("top"),
            os("--files"),
            os("--no-scan"),
            os("--json"),
            case.as_os_str(),
        ],
    );
    assert_eq!(top["scan"]["id"], id.as_str());
    let rows = top["entries"].as_array().unwrap();
    let row = rows
        .iter()
        .find(|row| row["path"] == lossy(&inside).as_str())
        .unwrap_or_else(|| panic!("no row for {}: {rows:?}", inside.display()));
    assert_eq!(row["name"], "inside.txt");
    assert!(row.get("name_raw").is_none(), "{row}");
    assert_eq!(row["path_raw"], raw(&inside));
    checked(&top, &inside);
    // A row whose path is UTF-8 carries no path_raw at all.
    let link = case.join("link-to-latin1");
    let plain_row = rows
        .iter()
        .find(|row| row["path"] == lossy(&link).as_str())
        .unwrap();
    assert!(plain_row.get("path_raw").is_none(), "{plain_row}");

    // The decoded bytes are the path to pass back: explain reads it.
    let decoded = PathBuf::from(OsString::from_vec(unbase64(
        row["path_raw"].as_str().unwrap(),
    )));
    assert_eq!(decoded, inside);
    let explain = json(&env, &[os("explain"), decoded.as_os_str(), os("--json")]);
    assert_eq!(explain["scan"]["id"], id.as_str());
    assert_eq!(explain["result"]["path_raw"], raw(&inside));
    assert_eq!(explain["query"]["path"], lossy(&inside).as_str());

    // The streamed rows (ndjson), --fields and --plain --detail.
    let out = env
        .cmd()
        .args(["top", "--files", "--no-scan", "--format", "ndjson"])
        .arg(&case)
        .output()
        .unwrap();
    let lines: Vec<Value> = out
        .stdout
        .split(|&b| b == b'\n')
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_slice(l).unwrap())
        .collect();
    assert!(
        lines
            .iter()
            .any(|l| l["type"] == "entry" && l["path_raw"] == raw(&inside)),
        "{lines:?}"
    );
    for line in &lines {
        checked_any(line);
    }
    let fields = json(
        &env,
        &[
            os("top"),
            os("--files"),
            os("--no-scan"),
            os("--json"),
            os("--fields"),
            os("path,path_raw"),
            case.as_os_str(),
        ],
    );
    let cut = fields["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["path"] == lossy(&inside).as_str())
        .unwrap();
    assert_eq!(
        cut,
        &serde_json::json!({"path": lossy(&inside), "path_raw": raw(&inside)})
    );
    let out = env
        .cmd()
        .args([
            "top",
            "--files",
            "--no-scan",
            "--plain",
            "--header",
            "--detail",
        ])
        .arg(&case)
        .output()
        .unwrap();
    let header = out.stdout.split(|&b| b == b'\n').next().unwrap();
    assert!(
        header.ends_with(b"\tflags\tpath_raw"),
        "{}",
        String::from_utf8_lossy(header)
    );
    let record = out
        .stdout
        .split(|&b| b == b'\n')
        .find(|r| r.starts_with(inside.as_os_str().as_bytes()))
        .unwrap();
    assert!(record.ends_with(raw(&inside).as_bytes()));

    // tree, by-type and summary of the directory, read from the saved scan.
    for verb in [&["tree", "--depth", "3"][..], &["by-type"][..]] {
        let mut args: Vec<&OsStr> = verb.iter().map(|a| os(a)).collect();
        args.extend([os("--no-scan"), os("--json"), case.as_os_str()]);
        let envelope = json(&env, &args);
        assert_eq!(envelope["ok"], true, "{verb:?}: {envelope}");
        let mut seen = Vec::new();
        check_raw(&envelope, &mut seen);
        assert!(
            seen.iter().any(|p| p.contains('\u{fffd}')),
            "{verb:?}: {envelope}"
        );
    }
    let summary = json(
        &env,
        &[
            os("summary"),
            os("--no-scan"),
            os("--json"),
            dir.as_os_str(),
        ],
    );
    assert_eq!(summary["result"]["path_raw"], raw(&dir));
    checked(&summary, &dir);
}

/// [`check_raw`] of `value` alone.
fn checked_any(value: &Value) {
    check_raw(value, &mut Vec::new());
}

#[test]
// One tree, and each verb that writes a path in turn.
#[allow(clippy::too_many_lines)]
fn p5_cli_01_every_verbs_rows_carry_path_raw() {
    let env = TestEnv::new();
    let Some(tree) = bytes_tree(&env) else {
        eprintln!("skipped: the filesystem refuses names that are not UTF-8");
        return;
    };
    let odd = tree.join(OsStr::from_bytes(b"caf\xe9"));
    let cache = odd.join("node_modules");
    let notes = odd.join("notes.txt");
    let first = json(
        &env,
        &[os("scan"), tree.as_os_str(), os("--save"), os("--json")],
    );
    let first_id = first["scan"]["id"].as_str().unwrap().to_owned();
    // caches, by project too: the cache's path and its project's.
    let caches = json(
        &env,
        &[
            os("caches"),
            os("--no-scan"),
            os("--min-size"),
            os("0"),
            os("--json"),
            tree.as_os_str(),
        ],
    );
    let row = &caches["entries"][0];
    assert_eq!(row["path_raw"], raw(&cache), "{caches}");
    assert_eq!(row["project_root_raw"], raw(&odd), "{caches}");
    checked(&caches, &cache);
    let projects = json(
        &env,
        &[
            os("caches"),
            os("--by-project"),
            os("--no-scan"),
            os("--min-size"),
            os("0"),
            os("--json"),
            tree.as_os_str(),
        ],
    );
    assert_eq!(projects["entries"][0]["project_root_raw"], raw(&odd));
    checked(&projects, &odd);
    // stale: the test clock is years after the files were written.
    let stale = json(
        &env,
        &[
            os("stale"),
            os("--no-scan"),
            os("--older-than"),
            os("1d"),
            os("--min-size"),
            os("0"),
            os("--json"),
            tree.as_os_str(),
        ],
    );
    checked(&stale, &notes);
    // diff: a file added under the directory, between two saved scans.
    let added = odd.join("added.bin");
    fs::write(&added, vec![0u8; 30_000]).unwrap();
    let second = json(
        &env,
        &[os("scan"), tree.as_os_str(), os("--save"), os("--json")],
    );
    let second_id = second["scan"]["id"].as_str().unwrap().to_owned();
    let diff = json(
        &env,
        &[
            os("diff"),
            os(&first_id),
            os(&second_id),
            os("--min-delta"),
            os("0"),
            os("--json"),
        ],
    );
    let change = diff["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["path"] == lossy(&added).as_str())
        .unwrap_or_else(|| panic!("{diff}"));
    assert_eq!(change["path_raw"], raw(&added));
    checked(&diff, &added);
    // clean's plan, a dry run: the item it would move.
    let clean = json(&env, &[os("clean"), notes.as_os_str(), os("--json")]);
    let item = &clean["result"]["plan"]["items"][0];
    assert_eq!(item["path_raw"], raw(&notes), "{clean}");
    checked(&clean, &notes);
    // explain of the directory, and explain of a path no scan holds.
    let explain = json(&env, &[os("explain"), odd.as_os_str(), os("--json")]);
    assert_eq!(explain["result"]["path_raw"], raw(&odd));
    let outside = env.work_dir().join(OsStr::from_bytes(b"x\xfey"));
    fs::write(&outside, b"x").unwrap();
    let outside = fs::canonicalize(&outside).unwrap();
    let unindexed = json(&env, &[os("explain"), outside.as_os_str(), os("--json")]);
    assert!(unindexed["scan"].is_null(), "{unindexed}");
    assert_eq!(unindexed["result"]["path_raw"], raw(&outside));
    // A scan run for the verb, where no saved scan is: the arena's rows.
    let empty = env.tmp_dir().join("no-scans");
    fs::create_dir_all(&empty).unwrap();
    let fresh = json(
        &env,
        &[
            os("top"),
            os("--files"),
            os("--no-save"),
            os("--index-dir"),
            empty.as_os_str(),
            os("--json"),
            odd.as_os_str(),
        ],
    );
    assert!(fresh["scan"]["id"].is_null(), "{fresh}");
    checked(&fresh, &notes);
}
