//! P1-WALK-06: mounts on real filesystems. These tests mount filesystems,
//! so they are ignored by default and run with `DUSCOPE_TEST_PLATFORM=1`
//! (`just check-full`; the CI platform job runs them under sudo). On Linux
//! they need root: without it they skip, except in CI, where a missing
//! prerequisite fails the test (`duscope_testkit::skip`).
//!
//! The file selects tests with `cfg(target_os)`, which the scanner keeps
//! inside `src/platform/` elsewhere: building and mounting a filesystem
//! takes different commands on each platform (`truncate`, `mkfs` and
//! `mount` against `hdiutil`), and each test exercises one platform's
//! mount table.

#![cfg(any(target_os = "linux", target_os = "macos"))]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use duscope_testkit::TestEnv;
use duscope_walk::{
    DirId, ScanEvent, Skip, VolumeClass, VolumeInfo, WalkOptions, WalkSummary, walk,
};

/// What a walk of `root` read and reported.
struct Seen {
    paths: Vec<PathBuf>,
    volumes: Vec<VolumeInfo>,
    skipped: BTreeMap<PathBuf, Skip>,
    summary: WalkSummary,
}

fn walk_root(root: &Path, opts: &WalkOptions) -> Seen {
    let mut dirs: BTreeMap<DirId, PathBuf> = BTreeMap::new();
    let (mut paths, mut volumes, mut skipped) = (Vec::new(), Vec::new(), BTreeMap::new());
    let summary = walk(root, opts, |event| match event {
        ScanEvent::Entry(entry) => {
            let path = if entry.dir == Some(DirId::ROOT) {
                root.to_owned()
            } else {
                dirs[&entry.parent].join(entry.name.as_os_str())
            };
            if let Some(id) = entry.dir {
                dirs.insert(id, path.clone());
            }
            paths.push(path);
        }
        ScanEvent::Volume(volume) => volumes.push(volume),
        ScanEvent::Skipped { dir, reason } => {
            skipped.insert(dirs[&dir].clone(), reason);
        }
        _ => {}
    })
    .unwrap();
    Seen {
        paths,
        volumes,
        skipped,
        summary,
    }
}

fn platform_tests() -> bool {
    let on = std::env::var_os("DUSCOPE_TEST_PLATFORM").is_some_and(|v| v == "1");
    if !on {
        eprintln!("skipped: set DUSCOPE_TEST_PLATFORM=1");
    }
    on
}

/// Runs a command of the test environment, failing the test on an error.
fn run(env: &TestEnv, program: &str, args: &[&dyn AsRef<std::ffi::OsStr>]) {
    let out = env.command(program).args(args).output().unwrap();
    assert!(
        out.status.success(),
        "{program} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Unmounts what a test mounted, innermost first, however the test ends.
struct Mounted<'a> {
    env: &'a TestEnv,
    points: Vec<PathBuf>,
}

impl Drop for Mounted<'_> {
    fn drop(&mut self) {
        for point in self.points.iter().rev() {
            let program = if cfg!(target_os = "macos") {
                "hdiutil"
            } else {
                "umount"
            };
            let mut cmd = self.env.command(program);
            if cfg!(target_os = "macos") {
                cmd.arg("detach").arg("-force");
            }
            let _ = cmd.arg(point).output();
        }
    }
}

/// Builds `root` under `work`: a plain directory, a loop-mounted ext4
/// image, a tmpfs, an exFAT image where the tools and the driver allow, and
/// the root bind-mounted below itself. Returns the mounts' guard and
/// whether exFAT was mounted.
#[cfg(target_os = "linux")]
fn mount_linux<'a>(env: &'a TestEnv, work: &Path, root: &Path) -> (Mounted<'a>, bool) {
    for dir in ["plain", "ext4", "tmpfs", "exfat", "sub/cycle"] {
        fs::create_dir_all(root.join(dir)).unwrap();
    }
    fs::write(root.join("plain/file"), b"plain").unwrap();
    let mut mounted = Mounted {
        env,
        points: Vec::new(),
    };

    let ext4 = work.join("ext4.img");
    run(env, "truncate", &[&"-s", &"16M", &ext4]);
    run(env, "mkfs.ext4", &[&"-q", &"-F", &ext4]);
    run(env, "mount", &[&"-o", &"loop", &ext4, &root.join("ext4")]);
    mounted.points.push(root.join("ext4"));
    fs::write(root.join("ext4/inside"), vec![7u8; 8192]).unwrap();

    run(
        env,
        "mount",
        &[
            &"-t",
            &"tmpfs",
            &"-o",
            &"size=4m",
            &"tmpfs",
            &root.join("tmpfs"),
        ],
    );
    mounted.points.push(root.join("tmpfs"));
    fs::write(root.join("tmpfs/inside"), b"memory").unwrap();

    // exFAT needs exfatprogs and the kernel's exfat driver.
    let exfat = work.join("exfat.img");
    run(env, "truncate", &[&"-s", &"16M", &exfat]);
    let failed = |step: &str, out: std::io::Result<std::process::Output>| match out {
        Ok(out) if out.status.success() => None,
        Ok(out) => Some(format!(
            "{step}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )),
        Err(e) => Some(format!("{step}: {e}")),
    };
    let why = failed("mkfs.exfat", env.command("mkfs.exfat").arg(&exfat).output()).or_else(|| {
        failed(
            "mount -o loop",
            env.command("mount")
                .args([
                    &"-o" as &dyn AsRef<std::ffi::OsStr>,
                    &"loop",
                    &exfat,
                    &root.join("exfat"),
                ])
                .output(),
        )
    });
    let has_exfat = match why {
        None => {
            mounted.points.push(root.join("exfat"));
            fs::write(root.join("exfat/inside"), b"foreign").unwrap();
            true
        }
        Some(why) => {
            duscope_testkit::skip(&format!("no exFAT here, so that part is left out ({why})"));
            false
        }
    };

    // The root bind-mounted below itself: entering it would loop.
    run(env, "mount", &[&"--bind", &root, &root.join("sub/cycle")]);
    mounted.points.push(root.join("sub/cycle"));
    (mounted, has_exfat)
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "mounts loop devices, tmpfs and a bind mount as root; DUSCOPE_TEST_PLATFORM=1"]
fn p1_walk_06_linux_mounts_are_crossed_confined_and_cycles_stopped() {
    let env = TestEnv::new();
    if !platform_tests() {
        return;
    }
    let uid = env.command("id").arg("-u").output().unwrap();
    if String::from_utf8_lossy(&uid.stdout).trim() != "0" {
        duscope_testkit::skip("mounting needs root");
        return;
    }
    let work = env.tmp_dir().join("platform");
    let root = work.join("root");
    let (_mounted, has_exfat) = mount_linux(&env, &work, &root);

    let seen = walk_root(&root, &WalkOptions::default());
    for path in ["plain/file", "ext4/inside", "tmpfs/inside"] {
        assert!(seen.paths.contains(&root.join(path)), "{path} was read");
    }
    assert_eq!(
        seen.skipped.get(&root.join("sub/cycle")),
        Some(&Skip::BindCycle)
    );
    assert!(
        !seen
            .paths
            .iter()
            .any(|p| p.starts_with(root.join("sub/cycle")) && p != &root.join("sub/cycle")),
        "nothing is counted twice through the cycle"
    );
    let volume = |name: &str| {
        seen.volumes
            .iter()
            .find(|v| v.mount_point == root.join(name))
            .unwrap()
    };
    assert_eq!(
        (volume("ext4").class, volume("ext4").entered()),
        (VolumeClass::Local, true)
    );
    assert_eq!(volume("ext4").fs_type, "ext4");
    assert_eq!(
        (volume("tmpfs").class, volume("tmpfs").entered()),
        (VolumeClass::Memory, true)
    );
    if has_exfat {
        let exfat = volume("exfat");
        assert_eq!(
            (exfat.class, exfat.entered(), exfat.case_sensitive),
            (VolumeClass::Foreign, true, Some(false))
        );
    }
    assert_eq!(seen.summary.mounts_excluded, 1, "the bind cycle");

    let opts = WalkOptions {
        one_file_system: true,
        ..WalkOptions::default()
    };
    let confined = walk_root(&root, &opts);
    assert!(confined.paths.contains(&root.join("plain/file")));
    for name in ["ext4", "tmpfs"] {
        assert_eq!(
            confined.skipped.get(&root.join(name)),
            Some(&Skip::OneFileSystem),
            "{name}"
        );
        assert!(!confined.paths.contains(&root.join(name).join("inside")));
    }
    let mounts = if has_exfat { 4 } else { 3 };
    assert_eq!(
        confined.summary.mounts_excluded, mounts,
        "-x: the mounts_excluded count"
    );
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "attaches a disk image; DUSCOPE_TEST_PLATFORM=1"]
fn p1_walk_06_macos_an_exfat_image_is_crossed_and_confined() {
    let env = TestEnv::new();
    if !platform_tests() {
        return;
    }
    let work = env.tmp_dir().join("platform");
    let root = work.join("root");
    fs::create_dir_all(root.join("exfat")).unwrap();
    fs::write(root.join("plain"), b"plain").unwrap();
    let image = work.join("exfat.dmg");
    run(
        &env,
        "hdiutil",
        &[
            &"create",
            &"-quiet",
            &"-size",
            &"16m",
            &"-fs",
            &"ExFAT",
            &"-volname",
            &"DUSCOPE",
            &image,
        ],
    );
    let mut mounted = Mounted {
        env: &env,
        points: Vec::new(),
    };
    run(
        &env,
        "hdiutil",
        &[
            &"attach",
            &"-quiet",
            &"-nobrowse",
            &"-mountpoint",
            &root.join("exfat"),
            &image,
        ],
    );
    mounted.points.push(root.join("exfat"));
    fs::write(root.join("exfat/inside"), b"foreign").unwrap();

    let seen = walk_root(&root, &WalkOptions::default());
    assert!(seen.paths.contains(&root.join("exfat/inside")));
    let exfat = seen.volumes.iter().find(|v| v.fs_type == "exfat").unwrap();
    assert_eq!(
        (exfat.class, exfat.entered(), exfat.case_sensitive),
        (VolumeClass::Foreign, true, Some(false))
    );
    assert_eq!(seen.summary.mounts_excluded, 0);
    // --clones is on by default and exFAT has no clone ids.
    assert!(seen.summary.clones_unavailable, "--clones on exFAT");

    let opts = WalkOptions {
        one_file_system: true,
        ..WalkOptions::default()
    };
    let confined = walk_root(&root, &opts);
    assert_eq!(
        confined.skipped.get(&root.join("exfat")),
        Some(&Skip::OneFileSystem)
    );
    assert!(!confined.paths.contains(&root.join("exfat/inside")));
    assert_eq!(confined.summary.mounts_excluded, 1);
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "bind-mounts and remounts as root; DUSCOPE_TEST_PLATFORM=1"]
fn p1_walk_06_linux_mount_ids_hold_for_same_device_binds_and_reused_ids() {
    let env = TestEnv::new();
    if !platform_tests() {
        return;
    }
    let uid = env.command("id").arg("-u").output().unwrap();
    if String::from_utf8_lossy(&uid.stdout).trim() != "0" {
        duscope_testkit::skip("mounting needs root");
        return;
    }
    let root = env.tmp_dir().join("platform-ids");
    for dir in ["src", "plain", "tmpfs"] {
        fs::create_dir_all(root.join(dir)).unwrap();
    }
    fs::write(root.join("src/inside"), b"bound").unwrap();
    run(
        &env,
        "mount",
        &[&"-t", &"tmpfs", &"tmpfs", &root.join("tmpfs")],
    );
    let _tmpfs = Mounted {
        env: &env,
        points: vec![root.join("tmpfs")],
    };
    let seen = walk_root(&root, &WalkOptions::default());
    assert!(
        seen.volumes
            .iter()
            .any(|v| v.mount_point == root.join("tmpfs"))
    );

    // A sibling bind-mounted on the same device is a mount of its own, also
    // where statx reports no mount id: the fallback provider looks it up by
    // path, as kernels before 5.8 need.
    fs::create_dir_all(root.join("plain/twin")).unwrap();
    run(
        &env,
        "mount",
        &[&"--bind", &root.join("src"), &root.join("plain/twin")],
    );
    let _twin = Mounted {
        env: &env,
        points: vec![root.join("plain/twin")],
    };
    for fallback in [false, true] {
        let opts = WalkOptions {
            one_file_system: true,
            fallback,
            ..WalkOptions::default()
        };
        let seen = walk_root(&root, &opts);
        assert_eq!(
            seen.skipped.get(&root.join("plain/twin")),
            Some(&Skip::OneFileSystem),
            "fallback {fallback}"
        );
        assert!(
            !seen.paths.contains(&root.join("plain/twin/inside")),
            "fallback {fallback}"
        );
    }

    // A mount id freed by an unmount goes to the next mount at once; a later
    // walk in the same process classifies the new mount, not the old one.
    run(&env, "umount", &[&root.join("tmpfs")]);
    fs::create_dir_all(root.join("kernel")).unwrap();
    run(
        &env,
        "mount",
        &[&"-t", &"proc", &"proc", &root.join("kernel")],
    );
    let _proc = Mounted {
        env: &env,
        points: vec![root.join("kernel")],
    };
    let seen = walk_root(&root, &WalkOptions::default());
    assert_eq!(seen.skipped.get(&root.join("kernel")), Some(&Skip::Kernel));
    let kernel = seen
        .volumes
        .iter()
        .find(|v| v.mount_point == root.join("kernel"))
        .unwrap();
    assert_eq!(
        (kernel.fs_type.as_str(), kernel.class),
        ("proc", VolumeClass::Kernel)
    );
}
