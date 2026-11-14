//! P5-CLI-02: `scan.excluded` and `scan.volumes` within the path read
//! (decision E). Every verb names only the excluded directories inside the
//! subtree its result was read at; a list names the first 20 of them, in
//! the scan's order, and the warning `excluded_truncated` counts the rest;
//! `summary` and `explain` name them all. Of the volumes, a verb names the
//! one the path lies on, by its device, and those mounted at or under it;
//! `summary`'s shares name every volume holding bytes under the path. Over
//! MCP, where a scan rooted above a root is read at the root, a result names
//! no path outside it but the scan's root and the mount point of the volume
//! the root lies on.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::ffi::OsStr;
use std::fs::{self, DirBuilder};
use std::io::Write;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use duscope_testkit::{TestEnv, golden, replace_values};
use rusqlite::{Connection, params};
use serde_json::{Value, json};

/// A valid `CACHEDIR.TAG`.
const TAG: &[u8] = b"Signature: 8a477f597d28d172789f06886806bc55\n";

/// `duscope ARGS`: its stdout as JSON, whatever the exit code.
fn run(env: &TestEnv, args: &[&OsStr]) -> Value {
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

/// `root/in/c00 … c24` and `root/out/d0 … d4`, each a cache that its
/// `CACHEDIR.TAG` marks, beside a file of its own; canonical.
fn tree(env: &TestEnv) -> PathBuf {
    let root = fs::canonicalize(env.tmp_dir()).unwrap().join("tree");
    let dirs = (0..25)
        .map(|i| format!("in/c{i:02}"))
        .chain((0..5).map(|i| format!("out/d{i}")));
    for dir in dirs {
        let dir = root.join(dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("CACHEDIR.TAG"), TAG).unwrap();
        fs::write(dir.join("blob"), vec![0u8; 5_000]).unwrap();
    }
    fs::write(root.join("in/kept"), vec![0u8; 9_000]).unwrap();
    fs::write(root.join("out/kept"), vec![0u8; 9_000]).unwrap();
    root
}

/// The paths `envelope`'s `scan.excluded` names.
fn excluded(envelope: &Value) -> Vec<String> {
    envelope["scan"]["excluded"]
        .as_array()
        .unwrap_or_else(|| panic!("{envelope}"))
        .iter()
        .map(|e| e["path"].as_str().unwrap().to_owned())
        .collect()
}

/// The paths of `names` under `dir`.
fn under(dir: &Path, names: impl IntoIterator<Item = String>) -> Vec<String> {
    names
        .into_iter()
        .map(|n| dir.join(n).display().to_string())
        .collect()
}

/// `excluded_truncated` among `warnings`: its count, and its hint.
fn truncated(warnings: &Value) -> Option<(u64, String)> {
    warnings
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["code"] == "excluded_truncated")
        .map(|w| {
            (
                w["count"].as_u64().unwrap(),
                w["hint"].as_str().unwrap().to_owned(),
            )
        })
}

#[test]
// One saved scan, read by each kind of verb in turn.
#[allow(clippy::too_many_lines)]
fn p5_cli_02_a_list_names_the_first_20_excluded_directories_under_its_path() {
    let env = TestEnv::new();
    let root = tree(&env);
    let inside = root.join("in");
    let outside = root.join("out");
    let first_20 = under(&inside, (0..20).map(|i| format!("c{i:02}")));
    let all_in = under(&inside, (0..25).map(|i| format!("c{i:02}")));
    let all_out = under(&outside, (0..5).map(|i| format!("d{i}")));

    // A scan run now: the first 20 of 30, in the scan's order, and the
    // warning for the other 10, whose hint names summary of the saved scan.
    let scanned = run(
        &env,
        &[
            os("scan"),
            root.as_os_str(),
            os("--exclude-caches"),
            os("--save"),
            os("--json"),
        ],
    );
    let id = scanned["scan"]["id"].as_str().unwrap().to_owned();
    assert_eq!(scanned["scan"]["excluded_count"], 30);
    assert_eq!(excluded(&scanned), first_20);
    let (count, hint) = truncated(&scanned["warnings"]).unwrap();
    assert_eq!(count, 10);
    assert!(
        hint.contains("duscope summary") && hint.contains(&id),
        "{hint}"
    );

    // The saved scan read at its root lists the same 20.
    let list = |verb: &[&str], path: &Path| {
        let mut args: Vec<&OsStr> = verb.iter().map(|a| os(a)).collect();
        args.extend([
            os("--no-scan"),
            os("--exclude-caches"),
            os("--json"),
            path.as_os_str(),
        ]);
        run(&env, &args)
    };
    let top = list(&["top"], &root);
    assert_eq!(top["scan"]["id"], id.as_str());
    assert_eq!(excluded(&top), first_20);
    assert_eq!(truncated(&top["warnings"]).unwrap().0, 10);
    assert_eq!(top["scan"]["excluded_count"], 30, "the whole scan's");
    // At a subdirectory: only what is under it, and the cap again.
    let top_in = list(&["top"], &inside);
    assert_eq!(excluded(&top_in), first_20);
    assert_eq!(truncated(&top_in["warnings"]).unwrap().0, 5);
    let top_out = list(&["top"], &outside);
    assert_eq!(excluded(&top_out), all_out);
    assert_eq!(truncated(&top_out["warnings"]), None);
    // Each list verb (caches refuses --exclude-caches, whose directories
    // it lists: below, by the scan's id).
    for verb in [
        &["tree"][..],
        &["stale", "--older-than", "1d"],
        &["by-type"],
    ] {
        let envelope = list(verb, &outside);
        assert_eq!(excluded(&envelope), all_out, "{verb:?}");
        let envelope = list(verb, &root);
        assert_eq!(excluded(&envelope), first_20, "{verb:?}");
        assert_eq!(truncated(&envelope["warnings"]).unwrap().0, 10, "{verb:?}");
    }
    let caches = run(
        &env,
        &[
            os("caches"),
            os("--scan"),
            os(&id),
            os("--json"),
            outside.as_os_str(),
        ],
    );
    assert_eq!(excluded(&caches), all_out, "{caches}");
    // ndjson: the header carries the block, the trailer the warning.
    let out = env
        .cmd()
        .args(["top", "--no-scan", "--exclude-caches", "--format", "ndjson"])
        .arg(&inside)
        .output()
        .unwrap();
    let lines: Vec<Value> = out
        .stdout
        .split(|&b| b == b'\n')
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_slice(l).unwrap())
        .collect();
    assert_eq!(excluded(&lines[0]), first_20);
    assert_eq!(truncated(&lines.last().unwrap()["warnings"]).unwrap().0, 5);

    // summary and explain: every one under the path, uncapped.
    let summary = list(&["summary"], &root);
    let mut every = all_in.clone();
    every.extend(all_out.clone());
    assert_eq!(excluded(&summary), every);
    assert_eq!(truncated(&summary["warnings"]), None);
    assert_eq!(excluded(&list(&["summary"], &outside)), all_out);
    let explain = run(&env, &[os("explain"), inside.as_os_str(), os("--json")]);
    assert_eq!(explain["scan"]["id"], id.as_str());
    assert_eq!(excluded(&explain), all_in);
    assert_eq!(truncated(&explain["warnings"]), None);

    // explain --errors and query read the whole scan: a list, capped.
    let errors = run(
        &env,
        &[os("explain"), os("--errors"), os(&id), os("--json")],
    );
    assert_eq!(excluded(&errors), first_20);
    assert_eq!(truncated(&errors["warnings"]).unwrap().0, 10);
    let sql = run(
        &env,
        &[
            os("query"),
            os("--no-scan"),
            os("--exclude-caches"),
            os("--sql"),
            os("SELECT 1"),
            os("--json"),
            outside.as_os_str(),
        ],
    );
    assert_eq!(excluded(&sql), all_out);

    // diff of two saved scans under a path.
    let again = run(
        &env,
        &[
            os("scan"),
            root.as_os_str(),
            os("--exclude-caches"),
            os("--save"),
            os("--json"),
        ],
    );
    let again_id = again["scan"]["id"].as_str().unwrap().to_owned();
    let diff = run(
        &env,
        &[
            os("diff"),
            os(&id),
            os(&again_id),
            os("--under"),
            outside.as_os_str(),
            os("--json"),
        ],
    );
    assert_eq!(excluded(&diff), all_out);
    let diff = run(&env, &[os("diff"), os(&id), os(&again_id), os("--json")]);
    assert_eq!(excluded(&diff), first_20);
    assert_eq!(truncated(&diff["warnings"]).unwrap().0, 10);

    // clean: the excluded directories within its items.
    let item = outside.join("d0");
    let clean = run(
        &env,
        &[
            os("clean"),
            os("--scan"),
            os(&id),
            item.as_os_str(),
            os("--json"),
        ],
    );
    assert_eq!(excluded(&clean), [item.display().to_string()], "{clean}");
}

#[test]
fn p5_cli_02_a_scan_and_its_saved_index_name_the_same_first_20() {
    let env = TestEnv::new();
    // Twenty under `a`, and `a-b` beside it: bytewise, `a-b` sorts before
    // `a/x00` (`-` before `/`), but the scan reaches it after `a`'s
    // subtree, and the first 20 are the scan's.
    let root = fs::canonicalize(env.tmp_dir()).unwrap().join("tree");
    let dirs = (0..20)
        .map(|i| format!("a/x{i:02}"))
        .chain(["a-b".to_owned()]);
    for dir in dirs {
        let dir = root.join(dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("CACHEDIR.TAG"), TAG).unwrap();
    }
    let scanned = run(
        &env,
        &[
            os("scan"),
            root.as_os_str(),
            os("--exclude-caches"),
            os("--save"),
            os("--json"),
        ],
    );
    let first_20 = under(&root.join("a"), (0..20).map(|i| format!("x{i:02}")));
    assert_eq!(excluded(&scanned), first_20);
    let top = run(
        &env,
        &[
            os("top"),
            os("--no-scan"),
            os("--exclude-caches"),
            os("--json"),
            root.as_os_str(),
        ],
    );
    assert_eq!(top["scan"]["id"], scanned["scan"]["id"]);
    assert_eq!(excluded(&top), first_20);
    assert_eq!(truncated(&top["warnings"]).unwrap().0, 1);
}

#[test]
fn p5_cli_02_summary_of_the_scan_root_is_unchanged() {
    let env = TestEnv::new();
    let root = fs::canonicalize(env.fixtures().path("home/Projects")).unwrap();
    let out = env
        .cmd()
        .args([
            "summary",
            "--json",
            "--threads",
            "4",
            "--no-save",
            "--exclude-caches",
        ])
        .arg(&root)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    let envelope: Value = serde_json::from_str(&stdout).unwrap();
    // More than a list names: summary names them all.
    assert!(envelope["scan"]["excluded"].as_array().unwrap().len() > 20);
    let mut text = duscope_testkit::normalize(&stdout, &root);
    replace_values(&mut text, "\"duration_ms\":", ',', "<ms>");
    golden(
        &format!("summary_json_excludes@{}.snap", std::env::consts::OS),
        &text,
    );
}

/// What a server wrote to stdout, a message a line.
fn serve(env: &TestEnv, root: &Path, requests: &[Value]) -> Vec<Value> {
    let mut child = env
        .cmd()
        .arg("mcp")
        .arg("--roots")
        .arg(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    for request in requests {
        writeln!(stdin, "{request}").unwrap();
    }
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn call(id: u64, tool: &str, arguments: &Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": {
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientCapabilities": {},
            },
            "name": tool,
            "arguments": arguments,
        },
    })
}

/// Every string in `value`, an object's keys among them, each with the
/// JSON pointer of where it is under `at` (a key's, its object's).
fn strings<'v>(at: &str, value: &'v Value, out: &mut Vec<(String, &'v str)>) {
    match value {
        Value::String(s) => out.push((at.to_owned(), s)),
        Value::Array(items) => {
            for (i, v) in items.iter().enumerate() {
                strings(&format!("{at}/{i}"), v, out);
            }
        }
        Value::Object(map) => {
            for (key, v) in map {
                out.push((at.to_owned(), key));
                strings(&format!("{at}/{key}"), v, out);
            }
        }
        _ => {}
    }
}

#[test]
fn p5_cli_02_a_tool_names_no_excluded_directory_outside_the_roots() {
    let env = TestEnv::new();
    let root = tree(&env);
    let inside = root.join("in");
    let outside = root.join("out");
    let scanned = run(
        &env,
        &[
            os("scan"),
            root.as_os_str(),
            os("--exclude-caches"),
            os("--save"),
            os("--json"),
        ],
    );
    let id = scanned["scan"]["id"].as_str().unwrap().to_owned();
    // The scan holds excluded directories outside the root to be served.
    let whole = run(
        &env,
        &[
            os("summary"),
            os("--scan"),
            os(&id),
            os("--json"),
            root.as_os_str(),
        ],
    );
    assert!(
        excluded(&whole)
            .iter()
            .any(|p| Path::new(p).starts_with(&outside)),
        "{whole}"
    );
    // It met one volume, the one the root lies on, named at the scan's
    // root whatever its mount point (on macOS, the Data volume's at
    // /System/Volumes/Data, which holds the root through a firmlink).
    let mounts = mount_points(&whole["scan"]);
    assert_eq!(mounts.len(), 1, "{whole}");
    let replies = serve(
        &env,
        &inside,
        &[
            call(1, "duscope_summary", &json!({"scan_id": id})),
            call(2, "duscope_explain", &json!({"scan_id": id})),
            call(3, "duscope_top", &json!({"scan_id": id})),
            call(
                4,
                "duscope_caches",
                &json!({"scan_id": id, "min_size": "0"}),
            ),
        ],
    );
    for id in 1..=4 {
        let reply = replies
            .iter()
            .find(|r| r["id"] == id)
            .unwrap_or_else(|| panic!("no reply {id}: {replies:?}"));
        let result = &reply["result"];
        assert_eq!(result["isError"], false, "{reply}");
        // Every path the result names lies within the root, but the scan's
        // root, above it, and the mount point of the volume the root lies
        // on, which a volume names (`scan.volumes`, and `summary`'s
        // `result.volumes`) wherever it is mounted.
        let mut texts = Vec::new();
        strings("", result, &mut texts);
        for (at, path) in named_paths(&texts) {
            let within = Path::new(path).starts_with(&inside);
            let scan_root = matches!(
                at,
                "/structuredContent/scan/root" | "/structuredContent/scan/root_as_given"
            ) && inside.starts_with(path);
            let mount = volume_mount_point(at) && mounts.iter().any(|m| m == path);
            assert!(
                within || scan_root || mount,
                "{id}: {at} names {path}, outside {}",
                inside.display()
            );
        }
        // The excluded directories inside the root, and no others.
        let listed = excluded(&result["structuredContent"]);
        assert!(!listed.is_empty(), "{reply}");
        assert!(
            listed.iter().all(|p| Path::new(p).starts_with(&inside)),
            "{listed:?}"
        );
    }
}

/// The paths among `texts` (from [`strings`]), each with its pointer: a
/// string of the envelope that starts with `/`, and in any other string (the
/// text block, a hint, a message) each word that does, less a trailing `,`,
/// `:` or `;`. The test's paths hold no space.
fn named_paths<'t>(texts: &'t [(String, &'t str)]) -> Vec<(&'t str, &'t str)> {
    let mut paths = Vec::new();
    for (at, text) in texts {
        if at.starts_with("/structuredContent/") && text.starts_with('/') {
            paths.push((at.as_str(), *text));
        } else {
            paths.extend(
                text.split_whitespace()
                    .filter(|word| word.starts_with('/'))
                    .map(|word| (at.as_str(), word.trim_end_matches([',', ':', ';']))),
            );
        }
    }
    paths
}

/// Whether `at` points at a volume's mount point: `scan.volumes[i]`'s, or
/// `summary`'s `result.volumes[i]`'s.
fn volume_mount_point(at: &str) -> bool {
    let parts: Vec<&str> = at.split('/').collect();
    matches!(
        parts.as_slice(),
        ["", "structuredContent", "scan" | "result", "volumes", i, "mount_point"]
            if i.parse::<usize>().is_ok()
    )
}

/// A volume row of [`firmlinked`]'s scan: its device, its mount's id, its
/// mount point, whether it is read-only, and why the scan did not enter it.
type VolumeRow<'a> = (u32, u32, Vec<u8>, bool, Option<&'a str>);

/// A saved scan shaped as macOS's scan of `/`, made by a scan of `vol` and
/// given its devices and volumes after: `vol` and `vol/sys` on the System
/// volume (device 1, mounted at `/`, read-only); `vol/data` on the Data
/// volume (device 2), reached as through a firmlink, whose mount point
/// `vol/sys/Volumes/Data` no path under `vol/data` is under, and so is
/// `vol/sys/usr/local`, through a firmlink below `vol/sys/usr`;
/// `vol/data/proj/nas`, a network mount (device 3) the scan did not enter;
/// and `vol/data/loop`, a bind mount of the Data volume it did not enter, a
/// cycle. The scan is saved in an index directory of its own and copied
/// into the test's, where it is changed: an index is never edited in place.
/// The root, and the scan's id.
fn firmlinked(env: &TestEnv) -> (PathBuf, String) {
    let root = fs::canonicalize(env.tmp_dir()).unwrap().join("vol");
    for dir in [
        "sys/usr/bin",
        "sys/usr/local/lib",
        "sys/Volumes/Data",
        "data/proj/src",
        "data/proj/nas",
        "data/loop",
    ] {
        fs::create_dir_all(root.join(dir)).unwrap();
    }
    fs::write(root.join("sys/usr/bin/tool"), vec![1u8; 8_192]).unwrap();
    fs::write(root.join("sys/usr/local/lib/libx"), vec![4u8; 12_000]).unwrap();
    fs::write(root.join("data/proj/src/main.rs"), vec![2u8; 20_000]).unwrap();
    fs::write(root.join("data/proj/README"), vec![3u8; 3_000]).unwrap();
    let staging = env.tmp_dir().join("staging");
    let scanned = run(
        env,
        &[
            os("scan"),
            root.as_os_str(),
            os("--save"),
            os("--index-dir"),
            staging.as_os_str(),
            os("--json"),
        ],
    );
    let id = scanned["scan"]["id"].as_str().unwrap().to_owned();
    let scans = env.index_dir().join("scans");
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&scans)
        .unwrap();
    let file = scans.join(format!("{id}.db"));
    fs::copy(staging.join("scans").join(format!("{id}.db")), &file).unwrap();
    let db = Connection::open(&file).unwrap();
    // A directory's id and last id, by its path below the root.
    let dir = |path: &str| -> (u32, u32) {
        path.split('/').fold((0, 0), |(parent, _), name| {
            db.query_row(
                "SELECT id, last_id FROM dirs WHERE parent_id = ?1 AND name = ?2",
                params![parent, name.as_bytes()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap()
        })
    };
    let (data, data_last) = dir("data");
    let (mount, _) = dir("sys/Volumes/Data");
    let (local, local_last) = dir("sys/usr/local");
    let (nas, _) = dir("data/proj/nas");
    db.execute("UPDATE dirs SET dev = 1", []).unwrap();
    db.execute(
        "UPDATE dirs SET dev = 2 WHERE id BETWEEN ?1 AND ?2 OR id BETWEEN ?3 AND ?4 OR id = ?5",
        [data, data_last, local, local_last, mount],
    )
    .unwrap();
    db.execute("UPDATE dirs SET dev = 3 WHERE id = ?1", [nas])
        .unwrap();
    // The volumes, each a copy of the one the walk met but for its device,
    // mount, mount point, whether it is read-only and why it was not
    // entered.
    db.execute_batch(
        "CREATE TEMP TABLE walked AS SELECT * FROM scan_volumes; DELETE FROM scan_volumes;",
    )
    .unwrap();
    let walked: u32 = db
        .query_row("SELECT count(*) FROM walked", [], |row| row.get(0))
        .unwrap();
    assert_eq!(walked, 1, "the scan met one volume");
    let at = |path: &str| root.join(path).into_os_string().into_vec();
    let volumes: [VolumeRow<'_>; 4] = [
        (1, 1, b"/".to_vec(), true, None),
        (2, 2, at("sys/Volumes/Data"), false, None),
        (3, 3, at("data/proj/nas"), false, Some("network")),
        (2, 4, at("data/loop"), false, Some("bind_cycle")),
    ];
    for (dev, mnt_id, mount_point, read_only, reason) in volumes {
        db.execute(
            "INSERT INTO scan_volumes SELECT ?1, ?2, ?3, fs_type, ?4, ?5 IS NULL, ?5, \
             case_sensitive, normalization, fsevents_uuid, fsevents_event_id, os_build, \
             total_bytes, free_bytes, options FROM walked",
            params![dev, mnt_id, mount_point, read_only, reason],
        )
        .unwrap();
    }
    (root, id)
}

/// The mount points of the volumes `block` names (a scan block, or a
/// summary's result).
fn mount_points(block: &Value) -> Vec<String> {
    block["volumes"]
        .as_array()
        .unwrap_or_else(|| panic!("{block}"))
        .iter()
        .map(|v| v["mount_point"].as_str().unwrap().to_owned())
        .collect()
}

/// The count of the warning `code` among `warnings`.
fn counted(warnings: &Value, code: &str) -> Option<u64> {
    warnings
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["code"] == code)
        .map(|w| w["count"].as_u64().unwrap())
}

#[test]
fn p5_cli_02_a_subtree_names_the_volume_its_device_is_on() {
    let env = TestEnv::new();
    let (root, id) = firmlinked(&env);
    let read = |verb: &str, path: &Path| {
        run(
            &env,
            &[
                os(verb),
                os("--scan"),
                os(&id),
                os("--json"),
                path.as_os_str(),
            ],
        )
    };
    let path = |rel: &str| root.join(rel).display().to_string();
    let (data, nas, bound) = (
        path("sys/Volumes/Data"),
        path("data/proj/nas"),
        path("data/loop"),
    );

    // Below the firmlink: the Data volume, by its device, though its mount
    // point is on no path under it, and the mount under the path.
    let proj = root.join("data/proj");
    let summary = read("summary", &proj);
    assert_eq!(
        mount_points(&summary["scan"]),
        [data.as_str(), nas.as_str()],
        "{summary}"
    );
    let result = &summary["result"];
    assert_eq!(
        mount_points(result),
        [data.as_str(), nas.as_str()],
        "{summary}"
    );
    assert!(result["allocated_bytes"].as_u64().unwrap() > 0);
    assert_eq!(
        result["volumes"][0]["allocated_bytes"], result["allocated_bytes"],
        "{summary}"
    );
    let top = read("top", &proj);
    assert_eq!(
        mount_points(&top["scan"]),
        [data.as_str(), nas.as_str()],
        "{top}"
    );
    assert_eq!(top["scan"]["volumes"][0]["read_only"], false);
    // The mount the scan did not enter under the path, and not the bind
    // mount of the path's device beside it.
    assert_eq!(counted(&top["warnings"], "mounts_excluded"), Some(1));
    // A file lies on its directory's device.
    let file = read("summary", &proj.join("src/main.rs"));
    assert_eq!(mount_points(&file["scan"]), [data.as_str()], "{file}");

    // Above both mounts the scan did not enter: both, by their mount
    // points.
    let above = read("top", &root.join("data"));
    assert_eq!(
        mount_points(&above["scan"]),
        [data.as_str(), nas.as_str(), bound.as_str()]
    );
    assert_eq!(counted(&above["warnings"], "mounts_excluded"), Some(2));

    // On the System volume: it, and the Data volume where it is mounted
    // under the path.
    let sys = read("top", &root.join("sys"));
    assert_eq!(mount_points(&sys["scan"]), ["/", data.as_str()], "{sys}");
    let bin = read("top", &root.join("sys/usr/bin"));
    assert_eq!(mount_points(&bin["scan"]), ["/"], "{bin}");
    assert_eq!(bin["scan"]["volumes"][0]["read_only"], true);
    assert_eq!(counted(&bin["warnings"], "mounts_excluded"), None);

    // At the scan's root, every one.
    let whole = read("summary", &root);
    assert_eq!(
        mount_points(&whole["scan"]),
        ["/", data.as_str(), nas.as_str(), bound.as_str()]
    );
}

#[test]
fn p5_cli_02_a_tool_names_the_volume_its_root_lies_on() {
    let env = TestEnv::new();
    let (root, id) = firmlinked(&env);
    let data = root.join("sys/Volumes/Data").display().to_string();
    let nas = root.join("data/proj/nas").display().to_string();
    let replies = serve(
        &env,
        &root.join("data/proj"),
        &[
            call(1, "duscope_summary", &json!({"scan_id": id})),
            call(2, "duscope_top", &json!({"scan_id": id})),
        ],
    );
    for id in 1..=2 {
        let reply = replies
            .iter()
            .find(|r| r["id"] == id)
            .unwrap_or_else(|| panic!("no reply {id}: {replies:?}"));
        assert_eq!(reply["result"]["isError"], false, "{reply}");
        let content = &reply["result"]["structuredContent"];
        assert_eq!(
            mount_points(&content["scan"]),
            [data.as_str(), nas.as_str()],
            "{reply}"
        );
    }
    let summary = &replies.iter().find(|r| r["id"] == 1).unwrap()["result"];
    let result = &summary["structuredContent"]["result"];
    assert_eq!(
        mount_points(result),
        [data.as_str(), nas.as_str()],
        "{summary}"
    );
}

#[test]
fn p5_cli_02_summary_shares_out_a_volume_reached_below_the_path() {
    let env = TestEnv::new();
    let (root, id) = firmlinked(&env);
    let data = root.join("sys/Volumes/Data").display().to_string();
    let summary = |path: &Path| {
        run(
            &env,
            &[
                os("summary"),
                os("--scan"),
                os(&id),
                os("--json"),
                path.as_os_str(),
            ],
        )
    };
    let usr = summary(&root.join("sys/usr"));
    // The path's own volume: the Data volume holds sys/usr/local through a
    // firmlink below the path, and is mounted elsewhere.
    assert_eq!(mount_points(&usr["scan"]), ["/"], "{usr}");
    // Each volume's share of the bytes under the path names it all the
    // same, with the bytes found on it, and the shares add up to the total.
    let result = &usr["result"];
    assert_eq!(mount_points(result), ["/", data.as_str()], "{usr}");
    let local = summary(&root.join("sys/usr/local"));
    assert_eq!(
        result["volumes"][1]["allocated_bytes"],
        local["result"]["allocated_bytes"]
    );
    let shares: u64 = result["volumes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["allocated_bytes"].as_u64().unwrap())
        .sum();
    assert_eq!(shares, result["allocated_bytes"].as_u64().unwrap());
}

/// The lines of a gitignore file that leaves out every entry of `/` but
/// those on the way to `dir`, absolute: a scan of `/` with it reads `dir`
/// and the directories above it alone.
#[cfg(target_os = "macos")]
fn only(dir: &Path) -> String {
    let escaped = |name: &OsStr| {
        let mut out = String::new();
        for c in name.to_str().unwrap().chars() {
            if matches!(c, '\\' | '*' | '?' | '[' | ']') {
                out.push('\\');
            }
            out.push(c);
        }
        out
    };
    let mut lines = vec!["/*".to_owned()];
    let mut at = String::new();
    for name in dir.iter().skip(1) {
        if !at.is_empty() {
            lines.push(format!("{at}/*"));
        }
        at = format!("{at}/{}", escaped(name));
        lines.push(format!("!{at}/"));
    }
    lines.join("\n") + "\n"
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "scans /; DUSCOPE_TEST_PLATFORM=1"]
fn p5_cli_02_macos_a_scan_of_slash_names_the_data_volume_below_private() {
    const DATA: &str = "/System/Volumes/Data";
    if std::env::var_os("DUSCOPE_TEST_PLATFORM").is_none_or(|v| v != "1") {
        eprintln!("skipped: set DUSCOPE_TEST_PLATFORM=1");
        return;
    }
    let env = TestEnv::new();
    // The test's directory, under /private (/var/folders), which is the
    // Data volume's through a firmlink.
    let dir = fs::canonicalize(env.tmp_dir()).unwrap().join("slash");
    assert!(dir.starts_with("/private"), "{}", dir.display());
    fs::create_dir_all(dir.join("sub")).unwrap();
    fs::write(dir.join("sub/file"), vec![1u8; 20_000]).unwrap();
    // A scan of `/` that reads the way down to it alone.
    let ignore = env.tmp_dir().join("only.ignore");
    fs::write(&ignore, only(&dir)).unwrap();
    let scanned = run(
        &env,
        &[
            os("scan"),
            os("/"),
            os("--ignore-from"),
            ignore.as_os_str(),
            os("--save"),
            os("--json"),
        ],
    );
    assert_eq!(scanned["ok"], true, "{scanned}");
    let id = scanned["scan"]["id"].as_str().unwrap().to_owned();
    let met = mount_points(&scanned["scan"]);
    assert!(
        met.iter().any(|m| m == "/") && met.iter().any(|m| m == DATA),
        "{met:?}"
    );
    let read = |verb: &str| {
        run(
            &env,
            &[
                os(verb),
                os("--scan"),
                os(&id),
                os("--json"),
                dir.as_os_str(),
            ],
        )
    };
    // Read at the directory: the Data volume, not the read-only System
    // volume mounted at `/`.
    let summary = read("summary");
    assert_eq!(mount_points(&summary["scan"]), [DATA], "{summary}");
    let result = &summary["result"];
    assert_eq!(mount_points(result), [DATA], "{summary}");
    assert!(result["volumes"][0]["allocated_bytes"].as_u64().unwrap() > 0);
    let top = read("top");
    assert_eq!(mount_points(&top["scan"]), [DATA], "{top}");
    assert_eq!(top["scan"]["volumes"][0]["read_only"], false);
}
