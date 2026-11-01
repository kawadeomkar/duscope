//! P2-CLI-03: `import` of ncdu's and gdu's JSON and of `GrandPerspective`'s
//! `.gpscan`: an export imported again holds the scan's directory totals;
//! gdu's cumulative sizes are added up again from the entries; an import
//! is read where `--scan` names it, not in place of a scan; the errors.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

use duscope_testkit::{TestEnv, golden, normalize};
use rusqlite::Connection;
use serde_json::Value;

fn run(env: &TestEnv, args: &[&str]) -> Output {
    env.cmd().args(args).output().unwrap()
}

fn ok(out: &Output) -> Vec<u8> {
    assert!(
        out.status.success(),
        "exit {:?}: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout.clone()
}

fn json(out: &Output) -> Value {
    serde_json::from_slice(&ok(out)).unwrap()
}

/// A tree with a hard link, a symbolic link, an empty file, nested
/// directories and a name that needs escaping.
fn tree(env: &TestEnv) -> PathBuf {
    let root = env.tmp_dir().join("tree");
    let files: [(&str, usize); 6] = [
        ("a.bin", 50_000),
        ("empty", 0),
        ("say \"hi\" & bye\tnow.txt", 300),
        ("sub dir/x.txt", 1),
        ("sub dir/deeper/y.bin", 9000),
        ("other/z", 70_000),
    ];
    for (path, size) in files {
        let file = root.join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, vec![b'x'; size]).unwrap();
    }
    fs::create_dir_all(root.join("void")).unwrap();
    fs::hard_link(root.join("a.bin"), root.join("other/a-link.bin")).unwrap();
    symlink("a.bin", root.join("lnk")).unwrap();
    fs::canonicalize(root).unwrap()
}

/// The saved scan with `id`.
fn index(env: &TestEnv, id: &str) -> Connection {
    Connection::open(env.index_dir().join("scans").join(format!("{id}.db"))).unwrap()
}

/// A directory's id, name, depth and totals.
type DirRow = (u32, Vec<u8>, u32, i64, i64, i64, i64);

/// Every directory's totals, in pre-order.
fn dirs(db: &Connection) -> Vec<DirRow> {
    let mut stmt = db
        .prepare(
            "SELECT id, name, depth, cum_allocated, cum_apparent, n_files, n_dirs \
             FROM dirs ORDER BY id",
        )
        .unwrap();
    stmt.query_map([], |r| {
        Ok((
            r.get(0)?,
            r.get(1)?,
            r.get(2)?,
            r.get(3)?,
            r.get(4)?,
            r.get(5)?,
            r.get(6)?,
        ))
    })
    .unwrap()
    .collect::<Result<_, _>>()
    .unwrap()
}

fn meta(db: &Connection, key: &str) -> String {
    db.query_row(
        "SELECT CAST(value AS TEXT) FROM scan_meta WHERE key = ?1",
        [key],
        |r| r.get(0),
    )
    .unwrap()
}

/// Saves a scan of `root` and exports it as `format` into `file`; the
/// scan's id.
fn scan_and_export(env: &TestEnv, root: &Path, format: &str, file: &Path) -> String {
    let path = root.to_str().unwrap();
    let scan = json(&run(
        env,
        &["scan", path, "--save", "--json", "--threads", "4"],
    ));
    ok(&run(
        env,
        &[
            "export",
            path,
            "--format",
            format,
            "-o",
            file.to_str().unwrap(),
        ],
    ));
    scan["scan"]["id"].as_str().unwrap().to_owned()
}

#[test]
fn p2_cli_03_an_ncdu_export_imports_to_the_scans_totals() {
    let env = TestEnv::new();
    let root = tree(&env);
    let file = env.tmp_dir().join("tree.json");
    let scan_id = scan_and_export(&env, &root, "ncdu", &file);
    let imported = json(&run(&env, &["import", file.to_str().unwrap(), "--json"]));
    assert_eq!(imported["query"]["command"], "import");
    assert_eq!(imported["scan"]["root"], root.to_str().unwrap());
    let import_id = imported["scan"]["id"].as_str().unwrap();
    let (scan, import) = (index(&env, &scan_id), index(&env, import_id));
    // Every directory, at the same id, with the same totals.
    assert_eq!(dirs(&import), dirs(&scan));
    assert_eq!(meta(&import, "source"), "import:ncdu");
    assert_eq!(meta(&scan, "source"), "scan");
    // The time the tree was read is the scan's.
    assert_eq!(
        meta(&import, "started_at").parse::<i64>().unwrap() / 1_000_000_000,
        meta(&scan, "started_at").parse::<i64>().unwrap() / 1_000_000_000
    );
    // Gzipped and from stdin, as the file shows it.
    let mut child = env
        .cmd()
        .args(["import", "-", "--json"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let gz = flate(&fs::read(&file).unwrap());
    std::io::Write::write_all(child.stdin.as_mut().unwrap(), &gz).unwrap();
    drop(child.stdin.take());
    let piped: Value = serde_json::from_slice(&ok(&child.wait_with_output().unwrap())).unwrap();
    let piped_id = piped["scan"]["id"].as_str().unwrap();
    assert_eq!(dirs(&index(&env, piped_id)), dirs(&scan));
}

/// `bytes` gzipped by the system's `gzip`.
fn flate(bytes: &[u8]) -> Vec<u8> {
    let mut child = std::process::Command::new("gzip")
        .arg("-c")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    std::io::Write::write_all(child.stdin.as_mut().unwrap(), bytes).unwrap();
    drop(child.stdin.take());
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    out.stdout
}

#[test]
fn p2_cli_03_a_gpscan_imports_to_the_files_it_holds() {
    let env = TestEnv::new();
    let root = tree(&env);
    let file = env.tmp_dir().join("tree.gpscan");
    let scan_id = scan_and_export(&env, &root, "gpscan", &file);
    let imported = json(&run(&env, &["import", file.to_str().unwrap(), "--json"]));
    let import_id = imported["scan"]["id"].as_str().unwrap();
    let (scan, import) = (index(&env, &scan_id), index(&env, import_id));
    assert_eq!(meta(&import, "source"), "import:gpscan");
    // The .gpscan holds each charged file of some size, at its bytes on
    // disk, and no folder sizes: the import's totals are those files'.
    let (bytes, files): (i64, i64) = scan
        .query_row(
            "SELECT SUM(allocated), COUNT(*) FROM files WHERE charged AND allocated > 0",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    let root_row = &dirs(&import)[0];
    // The index names its root row by the root's path.
    assert_eq!(root_row.1, root.as_os_str().as_encoded_bytes());
    assert_eq!((root_row.3, root_row.4, root_row.5), (bytes, bytes, files));
    assert_eq!(imported["scan"]["root"], root.to_str().unwrap());
}

/// gdu's export of a tree: its directories carry their cumulative sizes and
/// an item count.
const GDU: &str = r#"[1,2,{"progname":"gdu","progver":"v5.31.0","timestamp":2114380800},
[{"name":"/srv/data","asize":1000000,"dsize":1000000,"items":5,"mtime":2114380000},
{"name":"big.iso","asize":700000,"dsize":704512,"mtime":2114370000},
{"name":"note.txt","asize":12,"dsize":4096,"mtime":2114370000},
[{"name":"logs","asize":500000,"dsize":500000,"items":3,"mtime":2114370000},
{"name":"a.log","asize":3000,"dsize":4096,"ino":42,"hlnkc":true,"mtime":2114370000},
{"name":"b.log","asize":3000,"dsize":4096,"ino":42,"hlnkc":true,"mtime":2114370000},
{"name":"current","notreg":true}]]]
"#;

#[test]
fn p2_cli_03_gdu_totals_are_added_up_from_the_entries() {
    let env = TestEnv::new();
    let file = env.tmp_dir().join("gdu.json");
    fs::create_dir_all(env.tmp_dir()).unwrap();
    fs::write(&file, GDU).unwrap();
    let out = run(&env, &["import", file.to_str().unwrap(), "--json"]);
    let imported = json(&out);
    // The cumulative sizes gdu wrote are not counted again.
    assert_eq!(
        imported["scan"]["total_apparent_bytes"],
        700_000 + 12 + 3000
    );
    assert_eq!(
        imported["scan"]["total_allocated_bytes"],
        704_512 + 4096 + 4096
    );
    assert_eq!(imported["scan"]["file_count"], 5);
    let text = String::from_utf8(out.stdout).unwrap();
    golden(
        "import_json_gdu.snap",
        &normalize(&text, Path::new("/srv/data")),
    );
    let import_id = imported["scan"]["id"].as_str().unwrap();
    assert_eq!(meta(&index(&env, import_id), "source"), "import:gdu");
}

#[test]
fn p2_cli_03_an_import_is_read_where_scan_names_it() {
    let env = TestEnv::new();
    let root = tree(&env);
    let path = root.to_str().unwrap();
    let file = env.tmp_dir().join("tree.json");
    let scan_id = scan_and_export(&env, &root, "ncdu", &file);
    let imported = json(&run(&env, &["import", file.to_str().unwrap(), "--json"]));
    let import_id = imported["scan"]["id"].as_str().unwrap().to_owned();
    // The import starts when the scan did, but the newest scan of the
    // path is the scan; with the scan gone, nothing answers --no-scan.
    let top = json(&run(&env, &["top", path, "--json", "--no-scan"]));
    assert_eq!(top["scan"]["id"], scan_id.as_str());
    fs::remove_file(env.index_dir().join("scans").join(format!("{scan_id}.db"))).unwrap();
    let out = run(&env, &["top", path, "--json", "--no-scan"]);
    assert_eq!(
        out.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let named = json(&run(&env, &["top", path, "--json", "--scan", &import_id]));
    assert_eq!(named["scan"]["id"], import_id.as_str());
    // diff sets it against a scan of the tree here.
    let fresh = json(&run(&env, &["scan", path, "--save", "--json"]));
    let fresh_id = fresh["scan"]["id"].as_str().unwrap();
    let diff = json(&run(&env, &["diff", &import_id, fresh_id, "--json"]));
    assert_eq!(diff["query"]["command"], "diff");
    // An import has no scan options to scan with again.
    let out = run(&env, &["rescan", "--scan", &import_id, "--json"]);
    assert_eq!(out.status.code(), Some(2));
    let error: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("is an import"),
        "{error}"
    );
}

#[test]
fn p2_cli_03_what_cannot_be_imported() {
    let env = TestEnv::new();
    fs::create_dir_all(env.tmp_dir()).unwrap();
    let garbage = env.tmp_dir().join("garbage.json");
    fs::write(&garbage, b"[1,2,{},[{\"name\":\"/x\"},{\"name\":\"a/b\"}]]").unwrap();
    let path = garbage.to_str().unwrap();
    // Not a tree: a usage error that names the file and what is wrong.
    let out = run(&env, &["import", path, "--json"]);
    assert_eq!(out.status.code(), Some(2));
    let error: Value = serde_json::from_slice(&out.stdout).unwrap();
    let message = error["error"]["message"].as_str().unwrap();
    assert!(
        message.starts_with(path) && message.contains("slash"),
        "{message}"
    );
    // A format import does not read, another verb's format.
    for format in ["csv", "json", "qdirstat"] {
        let out = run(&env, &["import", path, "--format", format, "--json"]);
        assert_eq!(out.status.code(), Some(2), "{format}");
    }
    // gdu is import's alone.
    let out = run(&env, &["top", path, "--format", "gdu"]);
    assert_eq!(out.status.code(), Some(2));
    let out = run(&env, &["export", path, "--format", "gdu"]);
    assert_eq!(out.status.code(), Some(2));
    // A file that is not there.
    let out = run(&env, &["import", "/nonexistent/x.json", "--json"]);
    assert_eq!(out.status.code(), Some(1));
    let error: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(error["error"]["code"], "io_error");
    // Nothing was saved.
    let saved = fs::read_dir(env.index_dir().join("scans")).map_or(0, Iterator::count);
    assert_eq!(saved, 0);
}
