//! P2-QRY-03: `summary` on the fixture tree (a golden), clones as `du`
//! counts them, a path inside a saved scan, and `unaccounted_bytes` on a
//! mounted image (platform tests, which mount and attach images).

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fs;
use std::path::{Path, PathBuf};
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

fn summary(env: &TestEnv, path: &Path, extra: &[&str]) -> Value {
    let mut args = vec!["summary", path.to_str().unwrap(), "--json"];
    args.extend(extra);
    ok(&run(env, &args))
}

#[test]
fn p2_qry_03_golden_on_the_fixture_tree() {
    let env = TestEnv::new();
    let home = fs::canonicalize(env.fixtures().path("home")).unwrap();
    let os = std::env::consts::OS;
    let out = run(
        &env,
        &[
            "summary",
            home.to_str().unwrap(),
            "--json",
            "--threads",
            "4",
            "--no-save",
        ],
    );
    ok(&out);
    let mut text = duscope_testkit::normalize(&String::from_utf8(out.stdout).unwrap(), &home);
    replace_values(&mut text, "\"duration_ms\":", ',', "<ms>");
    golden(&format!("summary_json_home@{os}.snap"), &text);
    // The totals are the scan's, and the one volume holds them all.
    let facts = summary(&env, &home, &["--no-save"]);
    let result = &facts["result"];
    assert_eq!(
        result["allocated_bytes"],
        facts["scan"]["total_allocated_bytes"]
    );
    assert!(result["unaccounted_bytes"].is_null());
    let volumes = result["volumes"].as_array().unwrap();
    let held: u64 = volumes
        .iter()
        .map(|v| v["allocated_bytes"].as_u64().unwrap())
        .sum();
    assert_eq!(held, result["allocated_bytes"].as_u64().unwrap());
}

#[test]
fn p2_qry_03_clones_count_each_time_as_du_counts_them() {
    let env = TestEnv::new();
    let clones = fs::canonicalize(env.fixtures().path("cases/clones")).unwrap();
    let result = summary(&env, &clones, &["--no-save"])["result"].clone();
    let allocated = result["allocated_bytes"].as_u64().unwrap();
    let du = result["du_compatible_bytes"].as_u64().unwrap();
    if env.fixtures().built("clones") && cfg!(target_os = "macos") {
        // Three members of one clone group: du counts the blocks three
        // times, the scan once.
        assert!(du > allocated, "{du} > {allocated}");
    } else {
        assert_eq!(du, allocated);
    }
}

#[test]
fn p2_qry_03_a_path_inside_a_saved_scan_is_its_subtree() {
    let env = TestEnv::new();
    let home = fs::canonicalize(env.fixtures().path("home")).unwrap();
    let saved = ok(&run(
        &env,
        &["scan", home.to_str().unwrap(), "--save", "--json"],
    ));
    let library = home.join("Library");
    let facts = summary(&env, &library, &["--no-scan"]);
    assert_eq!(facts["scan"]["id"], saved["scan"]["id"]);
    let result = &facts["result"];
    assert_eq!(result["path"], library.to_str().unwrap());
    assert!(result["unaccounted_bytes"].is_null());
    let top = ok(&run(
        &env,
        &["top", home.to_str().unwrap(), "--json", "--no-scan"],
    ));
    let row = top["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["path"] == library.to_str().unwrap())
        .unwrap();
    assert_eq!(result["allocated_bytes"], row["allocated_bytes"]);
    assert_eq!(result["file_count"], row["file_count"]);
}

#[test]
fn p2_qry_03_errors_under_a_path_count_its_entries_too() {
    let env = TestEnv::new();
    let fixtures = env.fixtures();
    if !fixtures.unreadable_effective() {
        return duscope_testkit::skip("root reads the unreadable directories");
    }
    let root = fs::canonicalize(fixtures.root()).unwrap();
    let saved = ok(&run(
        &env,
        &["scan", root.to_str().unwrap(), "--save", "--json"],
    ));
    let id = saved["scan"]["id"].as_str().unwrap();
    // What explain --errors lists under cases/, directories and entries.
    let errors = ok(&run(&env, &["explain", "--errors", id, "--json"]));
    let cases = root.join("cases");
    let under = errors["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| Path::new(e["path"].as_str().unwrap()).starts_with(&cases))
        .count() as u64;
    assert!(under > 0);
    let facts = summary(&env, &cases, &["--no-scan"]);
    assert_eq!(facts["result"]["error_count"], under);
}

/// Whether the platform tests run (`DUSCOPE_TEST_PLATFORM=1`).
#[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]
fn platform_tests() -> bool {
    let on = std::env::var_os("DUSCOPE_TEST_PLATFORM").is_some_and(|v| v == "1");
    if !on {
        eprintln!("skipped: set DUSCOPE_TEST_PLATFORM=1");
    }
    on
}

/// Runs a command of the test environment, failing the test on an error.
fn system(env: &TestEnv, program: &str, args: &[&dyn AsRef<std::ffi::OsStr>]) {
    let out = env.command(program).args(args).output().unwrap();
    assert!(
        out.status.success(),
        "{program} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Unmounts the image a test mounted, however the test ends.
struct Mounted<'a> {
    env: &'a TestEnv,
    point: PathBuf,
}

impl Drop for Mounted<'_> {
    fn drop(&mut self) {
        let mut cmd = if cfg!(target_os = "macos") {
            let mut cmd = self.env.command("hdiutil");
            cmd.args(["detach", "-force"]);
            cmd
        } else {
            self.env.command("umount")
        };
        let _ = cmd.arg(&self.point).output();
    }
}

/// `summary` of a freshly mounted volume at `mount` holding a file:
/// `unaccounted_bytes` is what the volume uses that the scan did not find
/// (its own metadata), an integer.
fn unaccounted_on_a_mount(env: &TestEnv, mount: &Path) {
    fs::write(mount.join("data.bin"), vec![7u8; 1 << 20]).unwrap();
    let mount = fs::canonicalize(mount).unwrap();
    let facts = summary(env, &mount, &["--no-save"]);
    let result = &facts["result"];
    let unaccounted = result["unaccounted_bytes"].as_u64();
    assert!(unaccounted.is_some(), "{result}");
    let volume = &result["volumes"][0];
    assert_eq!(volume["mount_point"], mount.to_str().unwrap());
    let used = volume["total_bytes"].as_u64().unwrap() - volume["free_bytes"].as_u64().unwrap();
    assert_eq!(
        unaccounted.unwrap(),
        used.saturating_sub(volume["allocated_bytes"].as_u64().unwrap())
    );
    assert!(result["allocated_bytes"].as_u64().unwrap() >= 1 << 20);
    // A saved scan of the mount point keeps the figure in its index.
    let saved = ok(&run(
        env,
        &["scan", mount.to_str().unwrap(), "--save", "--json"],
    ));
    let id = saved["scan"]["id"].as_str().unwrap();
    let listed = ok(&run(env, &["scans", "--json"]));
    let file = listed["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == id)
        .unwrap()["index_path"]
        .as_str()
        .unwrap()
        .to_owned();
    let db = rusqlite::Connection::open(file).unwrap();
    let stored: Option<i64> = db
        .query_row(
            "SELECT value FROM scan_meta WHERE key = 'unaccounted_bytes'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(stored.is_some());
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "mounts an ext4 image; DUSCOPE_TEST_PLATFORM=1, as root"]
fn p2_qry_03_linux_a_mounted_image_has_unaccounted_bytes() {
    let env = TestEnv::new();
    if !platform_tests() {
        return;
    }
    let work = env.tmp_dir().join("ext4");
    let mount = work.join("mnt");
    fs::create_dir_all(&mount).unwrap();
    let image = work.join("ext4.img");
    system(&env, "truncate", &[&"-s", &"32M", &image]);
    system(&env, "mkfs.ext4", &[&"-q", &"-F", &image]);
    system(&env, "mount", &[&"-o", &"loop", &image, &mount]);
    let _mounted = Mounted {
        env: &env,
        point: mount.clone(),
    };
    unaccounted_on_a_mount(&env, &mount);
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "attaches a disk image; DUSCOPE_TEST_PLATFORM=1"]
fn p2_qry_03_macos_a_mounted_image_has_unaccounted_bytes() {
    let env = TestEnv::new();
    if !platform_tests() {
        return;
    }
    let work = env.tmp_dir().join("hfs");
    let mount = work.join("mnt");
    fs::create_dir_all(&mount).unwrap();
    let image = work.join("image.dmg");
    system(
        &env,
        "hdiutil",
        &[
            &"create",
            &"-quiet",
            &"-size",
            &"32m",
            &"-fs",
            &"APFS",
            &"-volname",
            &"DUSCOPE",
            &image,
        ],
    );
    system(
        &env,
        "hdiutil",
        &[
            &"attach",
            &"-quiet",
            &"-nobrowse",
            &"-mountpoint",
            &mount,
            &image,
        ],
    );
    let _mounted = Mounted {
        env: &env,
        point: mount.clone(),
    };
    unaccounted_on_a_mount(&env, &mount);
}
