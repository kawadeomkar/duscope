//! P2-IDX-02: `scan --save` writes the index after the output, atomically,
//! under the index directory.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Output;

use duscope_testkit::TestEnv;
use rusqlite::Connection;

fn run(env: &TestEnv, args: &[&str], vars: &[(&str, &str)]) -> Output {
    env.cmd()
        .args(args)
        .envs(vars.iter().copied())
        .output()
        .unwrap()
}

fn envelope(out: &Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "{e}: stdout {} stderr {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

/// The files under `dir/scans`, by name.
fn scans(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir.join("scans"))
        .map(|entries| {
            entries
                .map(|e| e.unwrap().file_name().into_string().unwrap())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

fn is_root(env: &TestEnv) -> bool {
    let uid = env.command("id").arg("-u").output().unwrap();
    String::from_utf8_lossy(&uid.stdout).trim() == "0"
}

#[test]
fn p2_idx_02_save_prints_the_id_and_writes_the_index() {
    let env = TestEnv::new();
    let root = env.fixtures().root();
    let out = run(
        &env,
        &["scan", root.to_str().unwrap(), "--save", "--json"],
        &[],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let envelope = envelope(&out);
    let id = envelope["scan"]["id"].as_str().unwrap().to_owned();
    assert_eq!(id.len(), 12);
    assert!(id.bytes().all(|b| b.is_ascii_hexdigit()));
    // Under DUSCOPE_INDEX_DIR, which the test environment sets.
    assert_eq!(scans(&env.index_dir()), [format!("{id}.db")]);
    let db = Connection::open(env.index_dir().join("scans").join(format!("{id}.db"))).unwrap();
    let (total, files): (i64, i64) = db
        .query_row(
            "SELECT cum_allocated, n_files FROM dirs WHERE id = 0",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        total,
        envelope["scan"]["total_allocated_bytes"].as_i64().unwrap()
    );
    assert_eq!(files, envelope["scan"]["file_count"].as_i64().unwrap());
    let stored: String = db
        .query_row(
            "SELECT value FROM scan_meta WHERE key = 'scan_id'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(stored, id);
    // The fixture's hard links are one group each, charged once.
    let links: i64 = db
        .query_row("SELECT COUNT(*) FROM hardlinks", [], |r| r.get(0))
        .unwrap();
    assert!(links > 0, "the fixture tree has hard links");
    // For people the id goes to stderr, never into --plain's records.
    let out = run(
        &env,
        &["scan", root.to_str().unwrap(), "--save", "--plain"],
        &[],
    );
    assert!(out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("saved as "), "{err}");
    assert!(!String::from_utf8_lossy(&out.stdout).contains("saved as"));
    assert_eq!(scans(&env.index_dir()).len(), 2);
}

#[test]
fn p2_idx_02_a_kill_between_write_and_rename_leaves_no_db() {
    let env = TestEnv::new();
    let root = env.fixtures().case_root("extensions");
    let out = run(
        &env,
        &["scan", root.to_str().unwrap(), "--save", "--json"],
        &[("DUSCOPE_TEST_KILL_BEFORE_RENAME", "1")],
    );
    assert!(!out.status.success(), "the process was killed");
    let names = scans(&env.index_dir());
    let is_db = |n: &String| {
        Path::new(n)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("db"))
    };
    assert!(!names.iter().any(is_db), "{names:?}");
    assert_eq!(
        names.len(),
        1,
        "the temporary file stays for scans --prune: {names:?}"
    );
    assert!(names[0].contains(".db.tmp-"), "{names:?}");
}

#[test]
fn p2_idx_02_a_write_failure_after_the_output_exits_1() {
    let env = TestEnv::new();
    if is_root(&env) {
        duscope_testkit::skip("root writes into a read-only directory");
        return;
    }
    let root = env.fixtures().case_root("extensions");
    let dir = env.tmp_dir().join("read-only");
    fs::create_dir_all(&dir).unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).unwrap();
    let dir_arg = dir.to_str().unwrap();
    let out = run(
        &env,
        &[
            "scan",
            root.to_str().unwrap(),
            "--save",
            "--json",
            "--index-dir",
            dir_arg,
        ],
        &[],
    );
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(out.status.code(), Some(1));
    // The output stands: the envelope, whole, and nothing after it.
    let envelope = envelope(&out);
    assert_eq!(envelope["ok"], true);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("cannot write the index"), "{err}");
    assert!(scans(&dir).is_empty());
}

#[test]
fn p2_idx_02_the_index_dir_is_the_flag_then_the_variable_then_the_file() {
    let env = TestEnv::new();
    let root = env.fixtures().case_root("extensions");
    let root = root.to_str().unwrap();
    let flag = env.tmp_dir().join("flag");
    let var = env.tmp_dir().join("var");
    let file = env.tmp_dir().join("file");
    let config = env.xdg_config_home().join("duscope");
    fs::create_dir_all(&config).unwrap();
    fs::write(
        config.join("config.toml"),
        format!("[defaults]\nindex_dir = \"{}\"\n", file.display()),
    )
    .unwrap();
    let save = |extra: &[&str], vars: &[(&str, &str)]| {
        let mut args = vec!["scan", root, "--save", "--json"];
        args.extend(extra);
        let out = run(&env, &args, vars);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    save(
        &["--index-dir", flag.to_str().unwrap()],
        &[("DUSCOPE_INDEX_DIR", var.to_str().unwrap())],
    );
    assert_eq!(scans(&flag).len(), 1);
    save(&[], &[("DUSCOPE_INDEX_DIR", var.to_str().unwrap())]);
    assert_eq!(scans(&var).len(), 1);
    // Without the flag and the variable (which the test environment sets).
    let out = env
        .cmd()
        .args(["scan", root, "--save", "--json"])
        .env_remove("DUSCOPE_INDEX_DIR")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(scans(&file).len(), 1);
    // Without any of them: XDG_CACHE_HOME/duscope.
    let out = env
        .cmd()
        .args(["scan", root, "--save", "--json", "--config", "none"])
        .env_remove("DUSCOPE_INDEX_DIR")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(scans(&env.xdg_cache_home().join("duscope")).len(), 1);
}

#[test]
fn p2_idx_02_v_reports_the_write() {
    let env = TestEnv::new();
    let root = env.fixtures().case_root("extensions");
    let out = run(
        &env,
        &["scan", root.to_str().unwrap(), "--save", "--json", "-v"],
        &[],
    );
    assert!(out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("duscope: write: "), "{err}");
    assert!(err.contains(" µs/entry"), "{err}");
}

/// Mounts a 64 KiB tmpfs at `point` for the life of the guard.
#[cfg(target_os = "linux")]
struct Tmpfs<'e> {
    env: &'e TestEnv,
    point: std::path::PathBuf,
}

#[cfg(target_os = "linux")]
impl Drop for Tmpfs<'_> {
    fn drop(&mut self) {
        let _ = self.env.command("umount").arg(&self.point).status();
    }
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "mounts a small tmpfs as root; DUSCOPE_TEST_PLATFORM=1"]
fn p2_idx_02_a_full_tmpfs_fails_the_write_after_the_output() {
    let env = TestEnv::new();
    if std::env::var_os("DUSCOPE_TEST_PLATFORM").is_none_or(|v| v != "1") {
        return;
    }
    if !is_root(&env) {
        duscope_testkit::skip("mounting needs root");
        return;
    }
    let point = env.tmp_dir().join("full");
    fs::create_dir_all(&point).unwrap();
    let mounted = env
        .command("mount")
        .args(["-t", "tmpfs", "-o", "size=64k", "tmpfs"])
        .arg(&point)
        .status()
        .unwrap();
    assert!(mounted.success(), "mount -t tmpfs");
    let _guard = Tmpfs {
        env: &env,
        point: point.clone(),
    };
    let root = env.fixtures().root();
    let out = run(
        &env,
        &[
            "scan",
            root.to_str().unwrap(),
            "--save",
            "--json",
            "--index-dir",
            point.to_str().unwrap(),
        ],
        &[],
    );
    assert_eq!(
        out.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(envelope(&out)["ok"], true, "the output stands");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("cannot write the index"), "{err}");
    assert!(
        scans(&point).is_empty(),
        "no temporary file is left: {:?}",
        scans(&point)
    );
}
