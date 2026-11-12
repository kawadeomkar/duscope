//! P1-WALK-06: mounts on real filesystems; P5-WALK-02: btrfs subvolumes
//! and reflinks, XFS reflinks, overlayfs, exFAT, FAT, NTFS and squashfs on
//! Linux; P5-WALK-06: automount points on Linux.
//! These tests mount filesystems,
//! so they are ignored by default and run with `DUSCOPE_TEST_PLATFORM=1`
//! (`just check-full`; the CI platform job runs them under sudo). On Linux
//! they need root: without it they skip, except in CI, where a missing
//! prerequisite fails the test (`duscope_testkit::skip`), unless it is a
//! filesystem driver the platform job found the kernel lacks and named
//! (`duscope_testkit::skip_fs`).
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
    DirId, Entry, ScanEvent, Skip, VolumeClass, VolumeInfo, WalkOptions, WalkSummary, walk,
};

/// What a walk of `root` read and reported.
struct Seen {
    paths: Vec<PathBuf>,
    // Read by the Linux tests of filesystems' entries.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    entries: BTreeMap<PathBuf, Entry>,
    volumes: Vec<VolumeInfo>,
    skipped: BTreeMap<PathBuf, Skip>,
    summary: WalkSummary,
}

fn walk_root(root: &Path, opts: &WalkOptions) -> Seen {
    let mut dirs: BTreeMap<DirId, PathBuf> = BTreeMap::new();
    let (mut paths, mut volumes, mut skipped) = (Vec::new(), Vec::new(), BTreeMap::new());
    let mut entries = BTreeMap::new();
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
            paths.push(path.clone());
            entries.insert(path, entry);
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
        entries,
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

/// Runs a command of the test environment, failing the test on an error
/// (macOS's tests run `hdiutil` through [`TestEnv::hdiutil`]).
#[cfg(target_os = "linux")]
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
    env.hdiutil(&[
        &"create",
        &"-ov",
        &"-size",
        &"16m",
        &"-fs",
        &"ExFAT",
        &"-volname",
        &"DUSCOPE",
        &image,
    ]);
    let mut mounted = Mounted {
        env: &env,
        points: Vec::new(),
    };
    env.hdiutil(&[
        &"attach",
        &"-nobrowse",
        &"-mountpoint",
        &root.join("exfat"),
        &image,
    ]);
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

/// The types of the filesystems mounted at `point`, in mount order (the one
/// on top last), from `/proc/self/mountinfo`.
#[cfg(target_os = "linux")]
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

#[cfg(target_os = "linux")]
#[test]
#[ignore = "mounts debugfs as root; DUSCOPE_TEST_PLATFORM=1"]
fn p5_walk_06_linux_an_untriggered_automount_is_not_entered() {
    let env = TestEnv::new();
    if !as_root(&env) {
        return;
    }
    // Every mount of debugfs shows the one tree, whose `tracing` is an
    // automount point for tracefs (an S_AUTOMOUNT inode): untriggered in a
    // fresh mount, mounted on as soon as something opens it.
    let root = env.tmp_dir().join("debugfs");
    let tracing = root.join("tracing");
    fs::create_dir_all(&root).unwrap();
    if let Err(why) = attempt(&env, "mount", &[&"-t", &"debugfs", &"debugfs", &root]) {
        duscope_testkit::skip(&format!("debugfs cannot be mounted here ({why})"));
        return;
    }
    // Unmounted innermost first: the tracefs a walk mounted, then debugfs.
    let _mounted = Mounted {
        env: &env,
        points: vec![root.clone(), tracing.clone()],
    };
    // Listed, not read: the standard library's metadata calls statx without
    // AT_NO_AUTOMOUNT, which would mount tracefs on it.
    let listed = fs::read_dir(&root)
        .unwrap()
        .any(|e| e.is_ok_and(|e| e.file_name() == "tracing"));
    if !listed {
        duscope_testkit::skip("this kernel's debugfs has no tracing directory");
        return;
    }
    assert!(mounted_at(&tracing).is_empty(), "triggered before the walk");
    for (one_file_system, include_network, reason) in [
        (false, false, Skip::Network),
        (true, false, Skip::OneFileSystem),
        (true, true, Skip::OneFileSystem),
    ] {
        let flags = format!("-x {one_file_system}, --include-network {include_network}");
        let opts = WalkOptions {
            one_file_system,
            include_network,
            ..WalkOptions::default()
        };
        let seen = walk_root(&root, &opts);
        assert_eq!(seen.skipped.get(&tracing), Some(&reason), "{flags}");
        assert!(
            mounted_at(&tracing).is_empty(),
            "{flags}: the walk mounted tracefs"
        );
        // No volume row: the mount it carries is the root's, reported once.
        assert_eq!(seen.volumes.len(), 1, "{flags}: {:?}", seen.volumes);
        assert_eq!(seen.summary.mounts_excluded, 0, "{flags}");
    }
    // Asked to, the walk enters it, which mounts tracefs on it.
    let opts = WalkOptions {
        include_network: true,
        ..WalkOptions::default()
    };
    let seen = walk_root(&root, &opts);
    assert_eq!(seen.skipped.get(&tracing), None);
    assert_eq!(mounted_at(&tracing), ["tracefs"]);
    assert!(
        seen.paths.iter().any(|p| p.parent() == Some(&tracing)),
        "tracefs's entries were read"
    );
}

/// Stops a transient automount, and the mount under it, however the test
/// ends.
#[cfg(target_os = "linux")]
struct Automount<'a> {
    env: &'a TestEnv,
    point: PathBuf,
}

#[cfg(target_os = "linux")]
impl Drop for Automount<'_> {
    fn drop(&mut self) {
        let _ = self
            .env
            .command("systemd-mount")
            .arg("--umount")
            .arg(&self.point)
            .output();
    }
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "starts a transient systemd automount as root; DUSCOPE_TEST_PLATFORM=1"]
fn p5_walk_06_linux_a_systemd_automount_is_not_triggered() {
    let env = TestEnv::new();
    if !as_root(&env) {
        return;
    }
    let root = env.tmp_dir().join("autofs");
    let point = root.join("auto");
    fs::create_dir_all(root.join("plain")).unwrap();
    fs::write(root.join("plain/file"), b"plain").unwrap();
    // systemd mounts autofs at the point now, as a direct map, and the tmpfs
    // on it when something opens it.
    let started = attempt(
        &env,
        "systemd-mount",
        &[
            &"--automount=yes",
            &"--collect",
            &"-t",
            &"tmpfs",
            &"-o",
            &"size=1m",
            &"tmpfs",
            &point,
        ],
    );
    if let Err(why) = started {
        duscope_testkit::skip(&format!("no transient automount here ({why})"));
        return;
    }
    let _unit = Automount {
        env: &env,
        point: point.clone(),
    };
    assert_eq!(mounted_at(&point), ["autofs"]);
    // A mount whose class is known: network before `-x`, as for any mount.
    for (one_file_system, include_network, reason) in [
        (false, false, Skip::Network),
        (true, false, Skip::Network),
        (true, true, Skip::OneFileSystem),
    ] {
        let flags = format!("-x {one_file_system}, --include-network {include_network}");
        let opts = WalkOptions {
            one_file_system,
            include_network,
            ..WalkOptions::default()
        };
        let seen = walk_root(&root, &opts);
        assert_eq!(seen.skipped.get(&point), Some(&reason), "{flags}");
        assert_eq!(
            mounted_at(&point),
            ["autofs"],
            "{flags}: the walk triggered it"
        );
        // autofs marks the trigger on its dentry, which statx does not
        // report: the point reads as the root of an autofs mount, a network
        // volume of its own.
        assert!(
            seen.entries[&point]
                .flags
                .contains(duscope_walk::Flags::MOUNT_ROOT)
        );
        let volume = seen
            .volumes
            .iter()
            .find(|v| v.mount_point == point)
            .unwrap();
        assert_eq!(
            (volume.fs_type.as_str(), volume.class, volume.entered()),
            ("autofs", VolumeClass::Network, false),
            "{flags}"
        );
    }
    let opts = WalkOptions {
        include_network: true,
        ..WalkOptions::default()
    };
    let seen = walk_root(&root, &opts);
    assert_eq!(seen.skipped.get(&point), None);
    assert_eq!(mounted_at(&point), ["autofs", "tmpfs"]);
}

/// Runs a command of the test environment; `Err` with its error output (or
/// why it did not start) when it fails, for a prerequisite this machine
/// may lack: a `mkfs`, a kernel driver.
#[cfg(target_os = "linux")]
fn attempt(
    env: &TestEnv,
    program: &str,
    args: &[&dyn AsRef<std::ffi::OsStr>],
) -> Result<(), String> {
    match env.command(program).args(args).output() {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => Err(format!(
            "{program}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )),
        Err(e) => Err(format!("{program}: {e}")),
    }
}

/// Whether the platform tests run here as root, as mounting needs; notes
/// why not otherwise (`duscope_testkit::skip`, which fails in CI).
#[cfg(target_os = "linux")]
fn as_root(env: &TestEnv) -> bool {
    if !platform_tests() {
        return false;
    }
    let uid = env.command("id").arg("-u").output().unwrap();
    if String::from_utf8_lossy(&uid.stdout).trim() != "0" {
        duscope_testkit::skip("mounting needs root");
        return false;
    }
    true
}

/// Makes a `size` image at `image` with `mkfs` (the program and its
/// arguments before the image) and loop-mounts it at `point` with `opts`
/// (`-t ntfs3`, `-o subvol=…`), recording the mount in `mounted`; `Err`
/// with why, where a tool or a driver is missing.
#[cfg(target_os = "linux")]
fn loop_mount(
    env: &TestEnv,
    mounted: &mut Mounted<'_>,
    image: &Path,
    size: &str,
    mkfs: &[&str],
    opts: &[&str],
    point: &Path,
) -> Result<(), String> {
    fs::create_dir_all(point).unwrap();
    attempt(env, "truncate", &[&"-s", &size, &image])?;
    let (program, args) = mkfs.split_first().unwrap();
    let mut mkfs_args: Vec<&dyn AsRef<std::ffi::OsStr>> = args
        .iter()
        .map(|a| a as &dyn AsRef<std::ffi::OsStr>)
        .collect();
    mkfs_args.push(&image);
    attempt(env, program, &mkfs_args)?;
    let mut mount_args: Vec<&dyn AsRef<std::ffi::OsStr>> = vec![&"-o", &"loop"];
    mount_args.extend(opts.iter().map(|a| a as &dyn AsRef<std::ffi::OsStr>));
    mount_args.push(&image);
    mount_args.push(&point);
    attempt(env, "mount", &mount_args)?;
    mounted.points.push(point.to_owned());
    Ok(())
}

/// Writes `len` bytes of a repeating pattern to `path` and flushes them,
/// so the file's extents are allocated before its map is read.
#[cfg(target_os = "linux")]
fn write_synced(path: &Path, len: usize) {
    use std::io::Write;
    let mut file = fs::File::create(path).unwrap();
    let data: Vec<u8> = (0..len).map(|i| u8::try_from(i % 251).unwrap()).collect();
    file.write_all(&data).unwrap();
    file.sync_all().unwrap();
}

/// A reflink copy of `from` at `to`, flushed.
#[cfg(target_os = "linux")]
fn reflink(env: &TestEnv, from: &Path, to: &Path) {
    run(env, "cp", &[&"--reflink=always", &from, &to]);
    run(env, "sync", &[&"-f", &to]);
}

/// Asserts that the reflink pair `orig` and `copy` in `dir` shares every
/// extent, on both sides, and that the file `plain` shares none.
#[cfg(target_os = "linux")]
fn assert_reflinked(dir: &Path, plain: &Path) {
    for name in ["orig", "copy"] {
        let extents = duscope_walk::extents(&dir.join(name)).unwrap().unwrap();
        assert!(extents.shared >= 1024 * 1024, "{name}: {extents:?}");
        assert_eq!(extents.shared, extents.mapped, "{name}: {extents:?}");
    }
    let plain = duscope_walk::extents(plain).unwrap().unwrap();
    assert_eq!(plain.shared, 0, "{plain:?}");
    assert!(plain.mapped > 0, "{plain:?}");
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "mounts a btrfs image as root; DUSCOPE_TEST_PLATFORM=1"]
fn p5_walk_02_linux_btrfs_subvolumes_are_boundaries_and_reflinks_share_extents() {
    use duscope_walk::{Boundary, Flags};

    let env = TestEnv::new();
    if !as_root(&env) {
        return;
    }
    let work = env.tmp_dir().join("btrfs");
    let root = work.join("root");
    let fs_root = root.join("btrfs");
    let mut mounted = Mounted {
        env: &env,
        points: Vec::new(),
    };
    if let Err(why) = loop_mount(
        &env,
        &mut mounted,
        &work.join("btrfs.img"),
        "256M",
        &["mkfs.btrfs", "-q"],
        &[],
        &fs_root,
    ) {
        duscope_testkit::skip_fs("btrfs", &format!("no btrfs here ({why})"));
        return;
    }
    run(
        &env,
        "btrfs",
        &[&"-q", &"subvolume", &"create", &fs_root.join("sub")],
    );
    run(
        &env,
        "btrfs",
        &[&"-q", &"subvolume", &"create", &fs_root.join("sub/nested")],
    );
    fs::create_dir(fs_root.join("plain")).unwrap();
    write_synced(&fs_root.join("plain/file"), 256 * 1024);
    write_synced(&fs_root.join("sub/orig"), 1024 * 1024);
    reflink(&env, &fs_root.join("sub/orig"), &fs_root.join("sub/copy"));
    // The subvolume mounted on its own as well, as /home is on many
    // systems.
    let source = env
        .command("findmnt")
        .args(["-n", "-o", "SOURCE"])
        .arg(&fs_root)
        .output()
        .unwrap();
    let source = String::from_utf8_lossy(&source.stdout).trim().to_owned();
    fs::create_dir(fs_root.join("mnt")).unwrap();
    run(
        &env,
        "mount",
        &[&"-o", &"subvol=/sub", &source, &fs_root.join("mnt")],
    );
    mounted.points.push(fs_root.join("mnt"));

    let seen = walk_root(&root, &WalkOptions::default());
    let entry = |rel: &str| {
        seen.entries
            .get(&root.join(rel))
            .unwrap_or_else(|| panic!("{rel} was not read"))
    };
    for rel in ["btrfs/sub", "btrfs/sub/nested"] {
        assert_eq!(entry(rel).boundary, Boundary::Subvolume, "{rel}");
        assert!(entry(rel).flags.contains(Flags::SUBVOLUME), "{rel}");
    }
    assert_eq!(entry("btrfs/plain").boundary, Boundary::None);
    assert!(!entry("btrfs/plain").flags.contains(Flags::SUBVOLUME));
    // The btrfs filesystem entered from the test's own is a mount; its
    // subvolume mounted inside it is a subvolume, and a mount root.
    assert_eq!(entry("btrfs").boundary, Boundary::Mount);
    let mnt = entry("btrfs/mnt");
    assert_eq!(mnt.boundary, Boundary::Subvolume);
    assert!(mnt.flags.contains(Flags::SUBVOLUME | Flags::MOUNT_ROOT));
    assert!(seen.paths.contains(&root.join("btrfs/mnt/orig")));
    let volume = seen
        .volumes
        .iter()
        .find(|v| v.mount_point == fs_root)
        .unwrap();
    assert_eq!(
        (volume.fs_type.as_str(), volume.class),
        ("btrfs", VolumeClass::Local)
    );
    assert!(
        seen.volumes
            .iter()
            .any(|v| v.mount_point == fs_root.join("mnt") && v.entered()),
        "the subvolume mount has a volume row of its own"
    );

    // -x keeps a subvolume inside the mount, and leaves the one mounted on
    // its own out, as any mount.
    let opts = WalkOptions {
        one_file_system: true,
        ..WalkOptions::default()
    };
    let confined = walk_root(&fs_root, &opts);
    assert!(confined.paths.contains(&fs_root.join("sub/nested")));
    assert!(confined.paths.contains(&fs_root.join("sub/orig")));
    assert_eq!(
        confined.skipped.get(&fs_root.join("mnt")),
        Some(&Skip::OneFileSystem)
    );

    assert_reflinked(&fs_root.join("sub"), &fs_root.join("plain/file"));

    overlay_across_subvolumes(&env, &mut mounted, &fs_root, &work.join("merged"));
}

/// An overlay at `merged` with its lower layer in the btrfs subvolume
/// `sub` of `fs_root` and its upper layer in the top-level one: its
/// directories are the overlay's, and none is a subvolume, though from
/// Linux 6.10 the overlay passes its layers' subvolume ids through.
#[cfg(target_os = "linux")]
fn overlay_across_subvolumes(
    env: &TestEnv,
    mounted: &mut Mounted<'_>,
    fs_root: &Path,
    merged: &Path,
) {
    use duscope_walk::{Boundary, Flags};

    fs::create_dir_all(fs_root.join("sub/lower/only-below")).unwrap();
    for dir in ["plain/upper", "plain/work"] {
        fs::create_dir_all(fs_root.join(dir)).unwrap();
    }
    fs::create_dir_all(merged).unwrap();
    let options = format!(
        "lowerdir={},upperdir={},workdir={}",
        fs_root.join("sub/lower").display(),
        fs_root.join("plain/upper").display(),
        fs_root.join("plain/work").display()
    );
    if let Err(why) = attempt(
        env,
        "mount",
        &[&"-t", &"overlay", &"overlay", &"-o", &options, &merged],
    ) {
        duscope_testkit::skip_fs("overlay", &format!("no overlayfs here ({why})"));
        return;
    }
    mounted.points.push(merged.to_owned());
    fs::create_dir(merged.join("made-above")).unwrap();
    let seen = walk_root(merged, &WalkOptions::default());
    for name in ["only-below", "made-above"] {
        let entry = &seen.entries[&merged.join(name)];
        assert_eq!(entry.boundary, Boundary::None, "{name}");
        assert!(!entry.flags.contains(Flags::SUBVOLUME), "{name}");
    }
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "mounts an XFS image as root; DUSCOPE_TEST_PLATFORM=1"]
fn p5_walk_02_linux_xfs_reflinks_share_extents() {
    let env = TestEnv::new();
    if !as_root(&env) {
        return;
    }
    let work = env.tmp_dir().join("xfs");
    let point = work.join("mnt");
    let mut mounted = Mounted {
        env: &env,
        points: Vec::new(),
    };
    if let Err(why) = loop_mount(
        &env,
        &mut mounted,
        &work.join("xfs.img"),
        "320M",
        &["mkfs.xfs", "-q", "-m", "reflink=1"],
        &[],
        &point,
    ) {
        duscope_testkit::skip_fs("xfs", &format!("no XFS with reflinks here ({why})"));
        return;
    }
    write_synced(&point.join("orig"), 1024 * 1024);
    write_synced(&point.join("plain"), 64 * 1024);
    reflink(&env, &point.join("orig"), &point.join("copy"));
    assert_reflinked(&point, &point.join("plain"));
    let seen = walk_root(&point, &WalkOptions::default());
    let volume = seen.volumes.first().unwrap();
    assert_eq!(
        (volume.fs_type.as_str(), volume.class),
        ("xfs", VolumeClass::Local)
    );
    // A reflinked file reports its whole allocation: the scan counts it
    // twice, which is why explain --clones reads the extents.
    let entry = |name: &str| &seen.entries[&point.join(name)];
    assert_eq!(entry("orig").allocated, entry("copy").allocated);
    assert!(entry("copy").allocated >= 1024 * 1024);
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "mounts an overlay as root; DUSCOPE_TEST_PLATFORM=1"]
fn p5_walk_02_linux_an_overlay_is_entered_and_confined_by_its_own_mount() {
    let env = TestEnv::new();
    if !as_root(&env) {
        return;
    }
    let work = env.tmp_dir().join("overlay");
    let root = work.join("root");
    for dir in ["lower", "upper", "work", "root/merged"] {
        fs::create_dir_all(work.join(dir)).unwrap();
    }
    fs::write(work.join("lower/from-lower"), b"lower").unwrap();
    let options = format!(
        "lowerdir={},upperdir={},workdir={}",
        work.join("lower").display(),
        work.join("upper").display(),
        work.join("work").display()
    );
    if let Err(why) = attempt(
        &env,
        "mount",
        &[
            &"-t",
            &"overlay",
            &"overlay",
            &"-o",
            &options,
            &root.join("merged"),
        ],
    ) {
        duscope_testkit::skip_fs("overlay", &format!("no overlayfs here ({why})"));
        return;
    }
    let _mounted = Mounted {
        env: &env,
        points: vec![root.join("merged")],
    };
    fs::write(root.join("merged/from-upper"), b"upper").unwrap();
    let seen = walk_root(&root, &WalkOptions::default());
    for name in ["merged/from-lower", "merged/from-upper"] {
        assert!(seen.paths.contains(&root.join(name)), "{name}");
    }
    let overlay = seen
        .volumes
        .iter()
        .find(|v| v.mount_point == root.join("merged"))
        .unwrap();
    assert_eq!(
        (overlay.fs_type.as_str(), overlay.class, overlay.entered()),
        ("overlay", VolumeClass::Overlay, true)
    );
    let opts = WalkOptions {
        one_file_system: true,
        ..WalkOptions::default()
    };
    let confined = walk_root(&root, &opts);
    assert_eq!(
        confined.skipped.get(&root.join("merged")),
        Some(&Skip::OneFileSystem)
    );
    // Rooted inside the overlay, -x confines by the overlay's own mount.
    let inside = walk_root(&root.join("merged"), &opts);
    assert!(inside.paths.contains(&root.join("merged/from-lower")));
    assert_eq!(inside.summary.mounts_excluded, 0);
}

/// The full attributes of the entries a walk of `root` read, by path.
#[cfg(target_os = "linux")]
fn walk_full(root: &Path) -> Seen {
    let opts = WalkOptions {
        attrs: duscope_walk::Attrs::Full,
        ..WalkOptions::default()
    };
    walk_root(root, &opts)
}

/// The creation time std reads (with `statx`), for comparing the walk's:
/// `None` where the filesystem reports none.
#[cfg(target_os = "linux")]
fn created(path: &Path) -> Option<i64> {
    let time = fs::symlink_metadata(path).unwrap().created().ok()?;
    let since = time.duration_since(std::time::UNIX_EPOCH).unwrap();
    Some(i64::try_from(since.as_nanos()).unwrap())
}

/// On the exFAT or FAT volume mounted at `point`: a file cannot be linked
/// and reports one link, its creation time is what `statx` reports, and
/// the volume is foreign and case-insensitive.
#[cfg(target_os = "linux")]
fn no_links_and_no_case(point: &Path, fs_type: &str) {
    use duscope_walk::Share;

    fs::write(point.join("file"), b"foreign").unwrap();
    assert!(
        fs::hard_link(point.join("file"), point.join("link")).is_err(),
        "{fs_type} has no hard links"
    );
    let seen = walk_full(point);
    let file = &seen.entries[&point.join("file")];
    assert_eq!((file.nlink, file.share), (1, Share::None), "{fs_type}");
    assert_eq!(
        file.full.unwrap().btime_ns,
        created(&point.join("file")),
        "{fs_type}: btime where statx has it"
    );
    let volume = seen.volumes.first().unwrap();
    assert_eq!(
        (volume.fs_type.as_str(), volume.class, volume.case_sensitive),
        (fs_type, VolumeClass::Foreign, Some(false))
    );
    // Neither keeps an extent map: the file is not opened for one.
    let unread = duscope_walk::extents(&point.join("file")).unwrap_err();
    assert_eq!(
        unread.kind(),
        std::io::ErrorKind::Unsupported,
        "{fs_type}: {unread}"
    );
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "mounts exFAT, FAT and NTFS images as root; DUSCOPE_TEST_PLATFORM=1"]
fn p5_walk_02_linux_foreign_filesystems_report_links_btime_and_case_as_they_are() {
    use duscope_walk::Share;

    let env = TestEnv::new();
    if !as_root(&env) {
        return;
    }
    let work = env.tmp_dir().join("foreign");
    let mut mounted = Mounted {
        env: &env,
        points: Vec::new(),
    };

    // exFAT and FAT: no hard links, case-insensitive, btime as statx
    // reports it.
    for (name, mkfs, fs_type) in [
        ("exfat", "mkfs.exfat", "exfat"),
        ("fat", "mkfs.vfat", "vfat"),
    ] {
        let point = work.join(name);
        match loop_mount(
            &env,
            &mut mounted,
            &work.join(format!("{name}.img")),
            "16M",
            &[mkfs],
            &[],
            &point,
        ) {
            Ok(()) => no_links_and_no_case(&point, fs_type),
            Err(why) => duscope_testkit::skip_fs(fs_type, &format!("no {fs_type} here ({why})")),
        }
    }

    // NTFS through ntfs3: hard links as nlink reports them, a creation
    // time, case-sensitive unless mounted with nocase.
    let ntfs = work.join("ntfs");
    let image = work.join("ntfs.img");
    match loop_mount(
        &env,
        &mut mounted,
        &image,
        "32M",
        &["mkfs.ntfs", "-q", "-F", "-f"],
        &["-t", "ntfs3"],
        &ntfs,
    ) {
        Ok(()) => {
            fs::write(ntfs.join("a"), b"linked").unwrap();
            fs::hard_link(ntfs.join("a"), ntfs.join("b")).unwrap();
            let seen = walk_full(&ntfs);
            let (a, b) = (
                &seen.entries[&ntfs.join("a")],
                &seen.entries[&ntfs.join("b")],
            );
            assert_eq!((a.nlink, a.share), (2, Share::Hardlink));
            assert_eq!((b.nlink, b.share), (2, Share::Hardlink));
            assert_eq!((a.dev, a.ino), (b.dev, b.ino), "one inode");
            // ntfs3's extent map is read, and shares nothing.
            let extents = duscope_walk::extents(&ntfs.join("a")).unwrap().unwrap();
            assert_eq!(extents.shared, 0, "{extents:?}");
            let btime = a.full.unwrap().btime_ns;
            assert!(btime.is_some(), "ntfs3 reports a creation time");
            assert_eq!(btime, created(&ntfs.join("a")));
            let volume = seen.volumes.first().unwrap();
            assert_eq!(
                (volume.fs_type.as_str(), volume.class, volume.case_sensitive),
                ("ntfs3", VolumeClass::Foreign, Some(true))
            );
            mounted.points.pop();
            run(&env, "umount", &[&ntfs]);
            run(
                &env,
                "mount",
                &[&"-t", &"ntfs3", &"-o", &"loop,nocase", &image, &ntfs],
            );
            mounted.points.push(ntfs.clone());
            let volume = walk_root(&ntfs, &WalkOptions::default()).volumes[0].clone();
            assert_eq!(volume.case_sensitive, Some(false), "nocase");
        }
        Err(why) => duscope_testkit::skip_fs("ntfs3", &format!("no NTFS (ntfs3) here ({why})")),
    }
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "mounts a squashfs image as root; DUSCOPE_TEST_PLATFORM=1"]
fn p5_walk_02_linux_a_squashfs_image_is_read_only() {
    let env = TestEnv::new();
    if !as_root(&env) {
        return;
    }
    let work = env.tmp_dir().join("squashfs");
    let root = work.join("root");
    let source = work.join("source");
    fs::create_dir_all(source.join("dir")).unwrap();
    fs::create_dir_all(root.join("image")).unwrap();
    fs::write(source.join("dir/file"), vec![3u8; 8192]).unwrap();
    fs::write(root.join("plain"), b"writable").unwrap();
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
    let seen = walk_root(&root, &WalkOptions::default());
    assert!(seen.paths.contains(&root.join("image/dir/file")));
    let volume = seen
        .volumes
        .iter()
        .find(|v| v.mount_point == root.join("image"))
        .unwrap();
    assert_eq!(
        (
            volume.fs_type.as_str(),
            volume.class,
            volume.read_only,
            volume.entered()
        ),
        ("squashfs", VolumeClass::ReadOnlyImage, true, true)
    );
    // What clean asks before it moves an item.
    assert!(duscope_walk::read_only_volume(&root.join("image/dir/file")).unwrap());
    assert!(!duscope_walk::read_only_volume(&root.join("plain")).unwrap());
}
