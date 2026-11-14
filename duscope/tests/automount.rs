//! P5-WALK-06: a scan that leaves an automount point alone counts it in the
//! `mounts_excluded` warning and flags its row `automount`, run now and read
//! again from the saved scan. A platform test: it mounts debugfs as root,
//! so it is ignored by default and runs with `DUSCOPE_TEST_PLATFORM=1`
//! (`just platform-tests`).

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
#![cfg(target_os = "linux")]

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use duscope_testkit::TestEnv;
use serde_json::Value;

fn duscope(env: &TestEnv, args: &[&dyn AsRef<OsStr>]) -> Value {
    let out: Output = env.cmd().args(args).output().unwrap();
    assert!(
        out.status.success(),
        "exit {:?}: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

/// Unmounts what the test mounted, innermost first, however it ends.
struct Mounted<'a> {
    env: &'a TestEnv,
    points: Vec<PathBuf>,
}

impl Drop for Mounted<'_> {
    fn drop(&mut self) {
        for point in self.points.iter().rev() {
            let _ = self.env.command("umount").arg(point).output();
        }
    }
}

/// The types of the filesystems mounted at `point`, from
/// `/proc/self/mountinfo`.
fn mounted_at(point: &Path) -> Vec<String> {
    fs::read_to_string("/proc/self/mountinfo")
        .unwrap()
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split(' ').collect();
            let fs_type = fields.iter().skip_while(|&&f| f != "-").nth(1)?;
            (fields.get(4).map(Path::new) == Some(point)).then(|| (*fs_type).to_owned())
        })
        .collect()
}

/// The envelope's `mounts_excluded` warning: its count and message.
fn mounts_excluded(envelope: &Value) -> Option<(u64, String)> {
    let warnings = envelope["warnings"].as_array().unwrap();
    let found: Vec<&Value> = warnings
        .iter()
        .filter(|w| w["code"] == "mounts_excluded")
        .collect();
    assert!(found.len() <= 1, "{warnings:?}");
    found.first().map(|w| {
        (
            w["count"].as_u64().unwrap(),
            w["message"].as_str().unwrap().to_owned(),
        )
    })
}

#[test]
#[ignore = "mounts debugfs as root; DUSCOPE_TEST_PLATFORM=1"]
fn p5_walk_06_linux_a_skipped_automount_point_counts_in_mounts_excluded() {
    let env = TestEnv::new();
    if std::env::var_os("DUSCOPE_TEST_PLATFORM").is_none_or(|v| v != "1") {
        eprintln!("skipped: set DUSCOPE_TEST_PLATFORM=1");
        return;
    }
    let uid = env.command("id").arg("-u").output().unwrap();
    if String::from_utf8_lossy(&uid.stdout).trim() != "0" {
        duscope_testkit::skip("mounting needs root");
        return;
    }
    // debugfs's `tracing` is an automount point for tracefs, untriggered
    // in a fresh mount (P5-WALK-06's walk test).
    let root = env.tmp_dir().join("debugfs");
    let tracing = root.join("tracing");
    fs::create_dir_all(&root).unwrap();
    let out = env
        .command("mount")
        .args([OsStr::new("-t"), "debugfs".as_ref(), "debugfs".as_ref()])
        .arg(&root)
        .output()
        .unwrap();
    if !out.status.success() {
        let why = String::from_utf8_lossy(&out.stderr);
        duscope_testkit::skip(&format!("debugfs cannot be mounted here ({why})"));
        return;
    }
    let _mounted = Mounted {
        env: &env,
        points: vec![root.clone(), tracing.clone()],
    };
    let listed = fs::read_dir(&root)
        .unwrap()
        .any(|e| e.is_ok_and(|e| e.file_name() == "tracing"));
    if !listed {
        duscope_testkit::skip("this kernel's debugfs has no tracing directory");
        return;
    }
    assert!(mounted_at(&tracing).is_empty(), "triggered before the scan");
    let point = Some((
        1,
        "1 automount points the kernel had not mounted (each flagged automount) were not \
         entered"
            .to_owned(),
    ));
    for flag in [None, Some("-x")] {
        let mut args: Vec<&dyn AsRef<OsStr>> = vec![&"scan", &root, &"--save", &"--json"];
        args.extend(flag.as_ref().map(|f| f as &dyn AsRef<OsStr>));
        let scanned = duscope(&env, &args);
        assert_eq!(mounts_excluded(&scanned), point, "{flag:?}");
        assert!(
            mounted_at(&tracing).is_empty(),
            "{flag:?}: the scan mounted it"
        );
        // The saved scan, read at its root, says the same, and the point's
        // row carries the flag.
        let mut args: Vec<&dyn AsRef<OsStr>> = vec![
            &"tree", &root, &"--depth", &"1", &"--glob", &"tracing", &"--json",
        ];
        args.extend(flag.as_ref().map(|f| f as &dyn AsRef<OsStr>));
        let tree = duscope(&env, &args);
        assert_eq!(tree["scan"]["id"], scanned["scan"]["id"], "{flag:?}");
        assert_eq!(mounts_excluded(&tree), point, "{flag:?}");
        let rows = tree["entries"].as_array().unwrap();
        let row = rows
            .iter()
            .find(|r| r["path"] == tracing.to_str().unwrap())
            .unwrap_or_else(|| panic!("{flag:?}: no row for tracing in {rows:?}"));
        assert!(
            row["flags"]
                .as_array()
                .unwrap()
                .contains(&Value::from("automount")),
            "{flag:?}: {row}"
        );
        assert!(mounted_at(&tracing).is_empty(), "{flag:?}: tree mounted it");
    }
    // Entered under --include-network, the point is no longer one. Entering
    // it mounts tracefs, a kernel filesystem, whose directories the scan
    // then meets as a mount it does not enter: the warning may count that
    // volume, never a point.
    let entered = duscope(&env, &[&"scan", &root, &"--include-network", &"--json"]);
    if let Some((_, message)) = mounts_excluded(&entered) {
        assert!(!message.contains("automount"), "{message}");
    }
    assert_eq!(mounted_at(&tracing), ["tracefs"]);
}
