//! P2-CLI-02: `export` in each format: goldens of a tree with a hard link,
//! a symbolic link, an empty file and names that need escaping; ncdu
//! reading its export back (`ncdu -f`, on Linux in CI); the rules of the
//! .gpscan; and the errors.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fs::{self, File, FileTimes, Permissions};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, SystemTime};

use duscope_core::query::Regex;
use duscope_testkit::{TestEnv, golden};

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

/// A tree whose every time and mode is fixed: a hard-linked file, a
/// symbolic link, an empty file, a name with a quote, an ampersand and a
/// tab, and a directory with a blank in its name.
fn tree(env: &TestEnv) -> PathBuf {
    let root = env.tmp_dir().join("tree");
    let files: [(&str, usize); 4] = [
        ("a.bin", 5000),
        ("empty", 0),
        ("say \"hi\" & bye\tnow.txt", 300),
        ("sub dir/x.txt", 1),
    ];
    for (path, size) in files {
        let file = root.join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, vec![b'x'; size]).unwrap();
    }
    fs::hard_link(root.join("a.bin"), root.join("a-link.bin")).unwrap();
    symlink("a.bin", root.join("lnk")).unwrap();
    let when = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
    let times = FileTimes::new().set_modified(when).set_accessed(when);
    for path in [
        "a.bin",
        "empty",
        "say \"hi\" & bye\tnow.txt",
        "sub dir/x.txt",
    ] {
        let file = File::options().write(true).open(root.join(path)).unwrap();
        file.set_times(times).unwrap();
        fs::set_permissions(root.join(path), Permissions::from_mode(0o644)).unwrap();
    }
    for dir in ["sub dir", ""] {
        let path = root.join(dir);
        fs::set_permissions(&path, Permissions::from_mode(0o755)).unwrap();
        File::open(&path).unwrap().set_times(times).unwrap();
    }
    fs::canonicalize(root).unwrap()
}

/// Saves a scan of `root`, with `--attrs full` for the owner and
/// permissions a `qdirstat` export needs.
fn save(env: &TestEnv, root: &Path) {
    ok(&run(
        env,
        &[
            "scan",
            root.to_str().unwrap(),
            "--save",
            "--json",
            "--threads",
            "4",
            "--attrs",
            "full",
        ],
    ));
}

/// `text` without what differs between machines and runs: the root, the
/// devices and inodes, the volume's figures, the owner, the times the
/// filesystem keeps for itself (creation and access times, and the
/// symbolic link's own, which the test cannot set).
fn mask(text: &str, root: &Path) -> String {
    let text = text.replace(root.to_str().unwrap(), "<root>");
    let every = [
        (r#""(dev|ino|uid|gid)":\d+"#, r#""$1":0"#),
        (r#"(volumeSize|freeSpace)="\d+""#, r#"$1="0""#),
        (r#"(created|accessed)="[^"]*""#, r#"$1="<time>""#),
        (r"\t\d+\t\d+\t(0\d{3})\t", "\t<uid>\t<gid>\t$1\t"),
    ];
    let symlink_only = [
        (r#""mtime":\d+"#, r#""mtime":0"#),
        (r#""mtime":"[^"]*""#, r#""mtime":"<time>""#),
        (r",\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ,", ",<time>,"),
        (r"\t0x[0-9a-f]+", "\t<mtime>"),
    ];
    let replace = |text: &str, (pattern, by): (&str, &str)| {
        let re = Regex::new(pattern).unwrap();
        String::from_utf8(re.replace_all(text.as_bytes(), by.as_bytes()).into_owned()).unwrap()
    };
    let mut masked = Vec::new();
    for line in text.split_inclusive('\n') {
        let mut line = every.iter().fold(line.to_owned(), |l, &r| replace(&l, r));
        // The symbolic link's line (not ncdu's "hlnkc").
        if ["lnk\"", "lnk\t", "lnk,"].iter().any(|l| line.contains(l)) {
            line = symlink_only.iter().fold(line, |l, &r| replace(&l, r));
        }
        masked.push(line);
    }
    masked.concat()
}

/// `bytes` gunzipped by the system's `gzip`.
fn gunzip(bytes: &[u8]) -> String {
    let mut child = Command::new("gzip")
        .args(["-dc"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    std::io::Write::write_all(child.stdin.as_mut().unwrap(), bytes).unwrap();
    drop(child.stdin.take());
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn p2_cli_02_goldens_of_every_format() {
    let env = TestEnv::new();
    let root = tree(&env);
    save(&env, &root);
    let os = std::env::consts::OS;
    let path = root.to_str().unwrap();
    for format in ["ncdu", "qdirstat", "ndjson", "csv"] {
        let out = ok(&run(&env, &["export", path, "--format", format]));
        let text = String::from_utf8_lossy(&out);
        golden(
            &format!("export_{format}_tree@{os}.snap"),
            &mask(&text, &root),
        );
    }
    let file = env.tmp_dir().join("tree.gpscan");
    ok(&run(
        &env,
        &[
            "export",
            path,
            "--format",
            "gpscan",
            "-o",
            file.to_str().unwrap(),
        ],
    ));
    let xml = gunzip(&fs::read(&file).unwrap());
    // The volume, and the root's path below it, are wherever the temporary
    // directory is.
    let volume = Regex::new(r#"volumePath="[^"]*""#).unwrap();
    let xml = volume.replace(xml.as_bytes(), &b"volumePath=\"<volume>\""[..]);
    let top = Regex::new(r#"<Folder name="[^"]*""#).unwrap();
    let xml = top.replace(&xml, &b"<Folder name=\"<root>\""[..]);
    let xml = String::from_utf8(xml.into_owned()).unwrap();
    golden(&format!("export_gpscan_tree@{os}.snap"), &mask(&xml, &root));
}

#[test]
fn p2_cli_02_the_gpscan_keeps_grandperspectives_rules() {
    let env = TestEnv::new();
    let root = tree(&env);
    save(&env, &root);
    let file = env.tmp_dir().join("tree.gpscan");
    let gpscan = |args: &[&str]| {
        let mut all = vec!["export", root.to_str().unwrap(), "--format", "gpscan"];
        all.extend(args);
        all.extend(["-o", file.to_str().unwrap()]);
        ok(&run(&env, &all));
        gunzip(&fs::read(&file).unwrap())
    };
    let physical = gpscan(&[]);
    assert!(physical.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n"));
    assert!(physical.contains("formatVersion=\"7\""));
    assert!(physical.contains("fileSizeMeasure=\"physical\""));
    // No zero-size entry; one link of the hard-linked file, flagged.
    assert!(!physical.contains("name=\"empty\""), "{physical}");
    assert_eq!(physical.matches(" flags=\"2\"").count(), 1, "{physical}");
    // Escaped as GrandPerspective writes names.
    assert!(
        physical.contains("name=\"say &quot;hi&quot; &amp; bye now.txt\""),
        "{physical}"
    );
    // Files before folders.
    let folder = physical.find("<Folder name=\"sub dir\"").unwrap();
    assert!(physical[..folder].contains("<File name=\"a"));
    assert!(!physical[folder..].contains("<File name=\"a"));
    // Every folder closed.
    assert_eq!(
        physical.matches("<Folder ").count(),
        physical.matches("</Folder>").count()
    );
    // Apparent bytes: both links.
    let logical = gpscan(&["--size", "apparent"]);
    assert!(logical.contains("fileSizeMeasure=\"logical\""));
    assert_eq!(logical.matches(" flags=\"2\"").count(), 2, "{logical}");
    // A .gpscan on a terminal is refused; on a pipe it is gzip.
    let out = run(
        &env,
        &["export", root.to_str().unwrap(), "--format", "gpscan"],
    );
    assert_eq!(&ok(&out)[..2], &[0x1f, 0x8b]);
}

#[test]
fn p2_cli_02_ncdu_reads_its_export_back() {
    let found = Command::new("ncdu").arg("--version").output().is_ok();
    if !found {
        // CI's Linux runners install ncdu; elsewhere it is optional.
        if cfg!(target_os = "linux") {
            duscope_testkit::skip("ncdu is not installed");
        }
        return;
    }
    let env = TestEnv::new();
    let root = tree(&env);
    save(&env, &root);
    let export = env.tmp_dir().join("tree.json");
    let out = ok(&run(
        &env,
        &["export", root.to_str().unwrap(), "--format", "ncdu"],
    ));
    fs::write(&export, out).unwrap();
    // Imported, then written out again to stdout; `-0` keeps ncdu from
    // starting curses, which a test has no terminal for.
    let read = Command::new("ncdu")
        .arg("-0")
        .arg("-f")
        .arg(&export)
        .args(["-o", "-"])
        .output()
        .unwrap();
    assert!(
        read.status.success(),
        "ncdu -f: {}",
        String::from_utf8_lossy(&read.stderr)
    );
    let text = String::from_utf8_lossy(&read.stdout);
    assert!(
        text.contains("\"a.bin\"") && text.contains("\"sub dir\""),
        "{text}"
    );
}

#[test]
fn p2_cli_02_export_wants_one_of_its_formats() {
    let env = TestEnv::new();
    let root = tree(&env);
    save(&env, &root);
    let path = root.to_str().unwrap();
    for args in [
        vec!["export", path],
        vec!["export", path, "--format", "json"],
    ] {
        let out = run(&env, &args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
    }
    // Another verb refuses export's formats.
    let out = run(&env, &["top", path, "--format", "ncdu"]);
    assert_eq!(out.status.code(), Some(2));
    // QDirStat needs the owner and permissions, which a basic scan lacks.
    let basic = env.tmp_dir().join("basic");
    fs::create_dir_all(&basic).unwrap();
    fs::write(basic.join("f"), b"x").unwrap();
    ok(&run(
        &env,
        &["scan", basic.to_str().unwrap(), "--save", "--json"],
    ));
    // stdout is the format's, so the error is for people, on stderr.
    let out = run(
        &env,
        &["export", basic.to_str().unwrap(), "--format", "qdirstat"],
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("needs --attrs full"), "{stderr}");
    // With --json the format would not reach stdout: the two exclude each
    // other.
    let out = run(&env, &["export", path, "--format", "csv", "--json"]);
    assert_eq!(out.status.code(), Some(2));
}
