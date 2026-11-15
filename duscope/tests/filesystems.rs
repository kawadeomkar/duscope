//! P5-WALK-02: the verbs on Linux filesystems the scan sees more of than
//! `du`: `explain --clones` sizing a reflink's shared extents on btrfs and
//! XFS, `summary`'s sidebar of btrfs subvolumes, and `clean` refusing an
//! item on a read-only squashfs image; P5-CORE-03: `clean --permanent`
//! stopping at a btrfs subvolume, with a hint of its own. Platform tests:
//! they mount images as root, so they are ignored by default and run with
//! `DUSCOPE_TEST_PLATFORM=1` (`just platform-tests`; CI's platform job runs
//! them under sudo). A filesystem that cannot be mounted fails a test in CI
//! unless the platform job found the kernel has no driver for it and named
//! it (`duscope_testkit::skip_fs`).
//!
//! The file selects its tests with `cfg(target_os)`, as the walker's
//! platform tests do: the filesystems are Linux's, and so are the commands
//! that build and mount them.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
#![cfg(target_os = "linux")]

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use duscope_testkit::TestEnv;
use serde_json::Value;

fn duscope(env: &TestEnv, args: &[&dyn AsRef<OsStr>]) -> Output {
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

/// Runs a command of the test environment; `Err` with its error output
/// when it fails, for a prerequisite this machine may lack.
fn attempt(env: &TestEnv, program: &str, args: &[&dyn AsRef<OsStr>]) -> Result<(), String> {
    match env.command(program).args(args).output() {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => Err(format!(
            "{program}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )),
        Err(e) => Err(format!("{program}: {e}")),
    }
}

fn run(env: &TestEnv, program: &str, args: &[&dyn AsRef<OsStr>]) {
    if let Err(why) = attempt(env, program, args) {
        panic!("{why}");
    }
}

/// Unmounts what a test mounted, innermost first, however it ends.
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

/// Whether the platform tests run here as root, as mounting needs.
fn as_root(env: &TestEnv) -> bool {
    if std::env::var_os("DUSCOPE_TEST_PLATFORM").is_none_or(|v| v != "1") {
        eprintln!("skipped: set DUSCOPE_TEST_PLATFORM=1");
        return false;
    }
    let uid = env.command("id").arg("-u").output().unwrap();
    if String::from_utf8_lossy(&uid.stdout).trim() != "0" {
        duscope_testkit::skip("mounting needs root");
        return false;
    }
    true
}

/// Makes a `size` image with `mkfs` and loop-mounts it at `point`.
fn loop_mount(
    env: &TestEnv,
    mounted: &mut Mounted<'_>,
    size: &str,
    mkfs: &[&str],
    point: &Path,
) -> Result<(), String> {
    fs::create_dir_all(point).unwrap();
    let image = point.with_extension("img");
    attempt(env, "truncate", &[&"-s", &size, &image])?;
    let (program, args) = mkfs.split_first().unwrap();
    let mut mkfs_args: Vec<&dyn AsRef<OsStr>> =
        args.iter().map(|a| a as &dyn AsRef<OsStr>).collect();
    mkfs_args.push(&image);
    attempt(env, program, &mkfs_args)?;
    attempt(env, "mount", &[&"-o", &"loop", &image, &point])?;
    mounted.points.push(point.to_owned());
    Ok(())
}

/// Writes `len` bytes to `path` and flushes them.
fn write_synced(path: &Path, len: usize) {
    use std::io::Write;
    let mut file = fs::File::create(path).unwrap();
    let data: Vec<u8> = (0..len).map(|i| u8::try_from(i % 251).unwrap()).collect();
    file.write_all(&data).unwrap();
    file.sync_all().unwrap();
}

/// `explain --clones` on a reflink pair under `dir`, mounted from a
/// filesystem that has them: each file of the pair shares all its extents,
/// so deleting it frees nothing for sure, a lower bound; the directory's
/// evidence counts both; without `--clones` nothing is said.
fn clones_on_a_reflink(env: &TestEnv, dir: &Path) {
    write_synced(&dir.join("orig"), 1024 * 1024);
    write_synced(&dir.join("plain"), 64 * 1024);
    run(
        env,
        "cp",
        &[&"--reflink=always", &dir.join("orig"), &dir.join("copy")],
    );
    run(env, "sync", &[&"-f", &dir.join("copy")]);
    ok(&duscope(env, &[&"scan", &dir, &"--save", &"--json"]));
    let explain = |path: &Path, clones: bool| {
        let mut args: Vec<&dyn AsRef<OsStr>> = vec![&"explain", &path, &"--json"];
        if clones {
            args.push(&"--clones");
        }
        ok(&duscope(env, &args))["result"].clone()
    };
    for name in ["orig", "copy"] {
        let facts = explain(&dir.join(name), true);
        assert_eq!(facts["is_clone"], true, "{name}: {facts}");
        assert_eq!(facts["freeable_kind"], "lower_bound", "{name}: {facts}");
        assert_eq!(facts["freeable_now_bytes"], 0, "{name}: {facts}");
        assert_eq!(facts["clone_group_size"], Value::Null);
        assert_eq!(facts["confidence"], "medium");
        let evidence = facts["evidence"].to_string();
        assert!(
            evidence.contains("fiemap: 1 of 1 files read now share 1048576 bytes"),
            "{name}: {evidence}"
        );
        // Without --clones, a Linux scan knows no clones.
        let plain = explain(&dir.join(name), false);
        assert_eq!(plain["is_clone"], Value::Null);
        assert_eq!(plain["freeable_kind"], "exact");
    }
    let plain = explain(&dir.join("plain"), true);
    assert_eq!(plain["is_clone"], false);
    assert_eq!(plain["freeable_kind"], "exact");
    let all = explain(dir, true);
    let evidence = all["evidence"].to_string();
    assert!(
        evidence.contains("fiemap: 2 of 3 files read now share 2097152 bytes"),
        "{evidence}"
    );
    assert_eq!(all["freeable_kind"], "lower_bound");
}

#[test]
#[ignore = "mounts btrfs and XFS images as root; DUSCOPE_TEST_PLATFORM=1"]
fn p5_walk_02_linux_explain_clones_reports_shared_extents_on_reflinks() {
    let env = TestEnv::new();
    if !as_root(&env) {
        return;
    }
    let work = fs::canonicalize(env.tmp_dir()).unwrap().join("reflinks");
    let mut mounted = Mounted {
        env: &env,
        points: Vec::new(),
    };
    for (name, size, mkfs) in [
        ("btrfs", "256M", &["mkfs.btrfs", "-q"][..]),
        ("xfs", "320M", &["mkfs.xfs", "-q", "-m", "reflink=1"][..]),
    ] {
        let point = work.join(name);
        match loop_mount(&env, &mut mounted, size, mkfs, &point) {
            Ok(()) => clones_on_a_reflink(&env, &point),
            Err(why) => duscope_testkit::skip_fs(name, &format!("no {name} here ({why})")),
        }
    }
}

#[test]
#[ignore = "mounts a btrfs image as root; DUSCOPE_TEST_PLATFORM=1"]
fn p5_walk_02_linux_summary_lists_btrfs_subvolumes() {
    let env = TestEnv::new();
    if !as_root(&env) {
        return;
    }
    let point = fs::canonicalize(env.tmp_dir()).unwrap().join("btrfs");
    let mut mounted = Mounted {
        env: &env,
        points: Vec::new(),
    };
    if let Err(why) = loop_mount(&env, &mut mounted, "256M", &["mkfs.btrfs", "-q"], &point) {
        duscope_testkit::skip_fs("btrfs", &format!("no btrfs here ({why})"));
        return;
    }
    for sub in ["home", "home/nested"] {
        run(
            &env,
            "btrfs",
            &[&"-q", &"subvolume", &"create", &point.join(sub)],
        );
    }
    fs::create_dir(point.join("plain")).unwrap();
    write_synced(&point.join("home/a.bin"), 512 * 1024);
    write_synced(&point.join("home/nested/b.bin"), 256 * 1024);
    write_synced(&point.join("plain/c.bin"), 128 * 1024);
    let facts = ok(&duscope(
        &env,
        &[&"summary", &point, &"--json", &"--no-save"],
    ));
    let result = &facts["result"];
    let subvolumes = result["subvolumes"].as_array().unwrap();
    let row = |rel: &str| {
        let path = point.join(rel);
        subvolumes
            .iter()
            .find(|s| s["path"] == path.to_str().unwrap())
            .unwrap_or_else(|| panic!("{rel} is not in {subvolumes:?}"))
    };
    assert_eq!(subvolumes.len(), 2, "{subvolumes:?}");
    let (home, nested) = (row("home"), row("home/nested"));
    for s in [home, nested] {
        assert_eq!(s["fs_type"], "btrfs");
        assert_eq!(
            s["referenced_bytes"],
            Value::Null,
            "btrfs's is the filesystem's"
        );
    }
    // Each holds its own files, the nested one's left out of home's.
    let bytes = |s: &Value| s["apparent_bytes"].as_u64().unwrap();
    assert!(
        (512 * 1024..256 * 1024 + 512 * 1024).contains(&bytes(home)),
        "{home}"
    );
    assert!(bytes(nested) >= 256 * 1024, "{nested}");
    // The volume holds them all.
    assert_eq!(result["volumes"][0]["mount_point"], point.to_str().unwrap());
    assert!(
        result["volumes"][0]["apparent_bytes"].as_u64().unwrap()
            >= bytes(home) + bytes(nested) + 128 * 1024
    );
    // A subvolume is flagged in the listings too.
    let tree = ok(&duscope(
        &env,
        &[&"tree", &point, &"--json", &"--no-save", &"--depth", &"1"],
    ));
    let home_row = tree["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["path"] == point.join("home").to_str().unwrap())
        .unwrap()
        .clone();
    assert!(
        home_row["flags"]
            .as_array()
            .unwrap()
            .contains(&Value::from("subvolume")),
        "{home_row}"
    );
}

#[test]
#[ignore = "mounts a squashfs image as root; DUSCOPE_TEST_PLATFORM=1"]
fn p5_walk_02_linux_clean_refuses_an_item_on_a_read_only_image() {
    let env = TestEnv::new();
    if !as_root(&env) {
        return;
    }
    let work = fs::canonicalize(env.tmp_dir()).unwrap().join("squashfs");
    let root = work.join("root");
    let source = work.join("source");
    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(root.join("image")).unwrap();
    fs::write(source.join("file"), vec![5u8; 4096]).unwrap();
    let image = work.join("image.sqfs");
    let made = attempt(
        &env,
        "mksquashfs",
        &[&source, &image, &"-quiet", &"-noappend"],
    )
    .and_then(|()| {
        attempt(
            &env,
            "mount",
            &[&"-o", &"loop", &image, &root.join("image")],
        )
    });
    if let Err(why) = made {
        duscope_testkit::skip_fs("squashfs", &format!("no squashfs here ({why})"));
        return;
    }
    let _mounted = Mounted {
        env: &env,
        points: vec![root.join("image")],
    };
    let saved = ok(&duscope(&env, &[&"scan", &root, &"--save", &"--json"]));
    let volume = saved["scan"]["volumes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["mount_point"] == root.join("image").to_str().unwrap())
        .unwrap()
        .clone();
    assert_eq!(volume["fs_type"], "squashfs");
    assert_eq!(volume["read_only"], true, "{volume}");
    let file = root.join("image/file");
    let out = duscope(&env, &[&"clean", &file, &"--yes", &"--json"]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let value: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["error"]["code"], "refused_path", "{value}");
    let items = value["result"]["plan"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(
        (&items[0]["action"], &items[0]["reason"]),
        (&Value::from("refused"), &Value::from("read_only_volume"))
    );
    assert!(file.exists());
}

/// Runs `clean` with `args` and the scan `id`; its envelope, which a run
/// that refuses or fails an item ends with exit 1 and `refused_path`.
fn refused(env: &TestEnv, args: &[&dyn AsRef<OsStr>], id: &str) -> Value {
    let mut all: Vec<&dyn AsRef<OsStr>> = vec![&"clean"];
    all.extend_from_slice(args);
    all.extend_from_slice(&[&"--scan", &id, &"--json"]);
    let out = duscope(env, &all);
    let value: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(out.status.code(), Some(1), "{value}");
    assert_eq!(value["error"]["code"], "refused_path", "{value}");
    value
}

/// `clean --permanent` stops at a btrfs subvolume under an item, as at a
/// mount point, and leaves it with the folders above it (`refused_path`);
/// the hint names what removes it: `btrfs subvolume delete` for a
/// subvolume that is no mount's root, `umount` for a mount point, a bind
/// mount of the same filesystem or a subvolume mounted on its own (another
/// device and a mount's root both). A subvolume named as an item is refused
/// in the plan (`mount_point`), and that hint names no `umount` either.
#[test]
#[ignore = "mounts a btrfs image, a subvolume of it and a bind mount as root; \
            DUSCOPE_TEST_PLATFORM=1"]
fn p5_core_03_linux_a_subvolume_is_left_to_btrfs_subvolume_delete_and_a_mount_to_umount() {
    let env = TestEnv::new();
    if !as_root(&env) {
        return;
    }
    let point = fs::canonicalize(env.tmp_dir()).unwrap().join("btrfs");
    let mut mounted = Mounted {
        env: &env,
        points: Vec::new(),
    };
    if let Err(why) = loop_mount(&env, &mut mounted, "256M", &["mkfs.btrfs", "-q"], &point) {
        duscope_testkit::skip_fs("btrfs", &format!("no btrfs here ({why})"));
        return;
    }
    // A subvolume in a folder; a folder holding a bind mount of another
    // folder of the filesystem; a folder holding a subvolume mounted on
    // its own.
    let (data, bound, holder) = (
        point.join("data"),
        point.join("bound"),
        point.join("holder"),
    );
    let (sub, bind, own) = (data.join("sub"), bound.join("m"), holder.join("mnt"));
    for dir in [&data, &bind, &own, &point.join("outside")] {
        fs::create_dir_all(dir).unwrap();
    }
    for subvolume in [&sub, &point.join("own")] {
        run(&env, "btrfs", &[&"-q", &"subvolume", &"create", subvolume]);
    }
    for (file, len) in [
        (data.join("a.bin"), 3000),
        (sub.join("b.bin"), 5000),
        (bound.join("c.bin"), 2000),
        (point.join("outside/target.bin"), 4000),
        (point.join("own/d.bin"), 1000),
    ] {
        write_synced(&file, len);
    }
    let source = env
        .command("findmnt")
        .args(["-n", "-o", "SOURCE"])
        .arg(&point)
        .output()
        .unwrap();
    let source = String::from_utf8_lossy(&source.stdout).trim().to_owned();
    run(&env, "mount", &[&"--bind", &point.join("outside"), &bind]);
    mounted.points.push(bind.clone());
    run(&env, "mount", &[&"-o", &"subvol=/own", &source, &own]);
    mounted.points.push(own.clone());
    let saved = ok(&duscope(&env, &[&"scan", &point, &"--save", &"--json"]));
    let id = saved["scan"]["id"].as_str().unwrap().to_owned();

    // The subvolume named as an item: refused in the plan.
    let value = refused(&env, &[&sub, &"--permanent"], &id);
    let item = &value["result"]["plan"]["items"][0];
    assert_eq!(
        (&item["action"], &item["reason"]),
        (&Value::from("refused"), &Value::from("mount_point")),
        "{value}"
    );
    let hint = value["error"]["hint"].as_str().unwrap();
    assert!(!hint.contains("umount"), "{hint}");

    // Under an item: the deletion stops there, it and its folder stay
    // (what is beside it goes only if it came first), and the hint names
    // the command that deletes a subvolume.
    let value = refused(&env, &[&data, &"--permanent", &"--yes"], &id);
    let error = &value["result"]["plan"]["items"][0]["error"];
    assert_eq!(error["code"], "refused_path", "{value}");
    let message = error["message"].as_str().unwrap();
    assert!(
        message.starts_with(&format!("{} is a btrfs subvolume", sub.display())),
        "{message}"
    );
    let delete = format!(
        "Run btrfs subvolume delete {} first, then duscope clean again",
        sub.display()
    );
    assert_eq!(error["hint"], delete.as_str(), "{value}");
    assert_eq!(value["error"]["hint"], delete.as_str(), "{value}");
    assert_eq!(fs::read(sub.join("b.bin")).unwrap().len(), 5000);

    // A mount point under an item, a bind mount or a subvolume mounted on
    // its own: the hint is umount's.
    for (folder, mount) in [(&bound, &bind), (&holder, &own)] {
        let value = refused(&env, &[folder, &"--permanent", &"--yes"], &id);
        let umount = format!(
            "Run umount {} first, then duscope clean again",
            mount.display()
        );
        assert_eq!(value["error"]["hint"], umount.as_str(), "{value}");
        assert!(mount.is_dir(), "{}", mount.display());
    }
    assert_eq!(
        fs::read(point.join("outside/target.bin")).unwrap().len(),
        4000
    );
    assert_eq!(fs::read(own.join("d.bin")).unwrap().len(), 1000);
    drop(mounted);
}
