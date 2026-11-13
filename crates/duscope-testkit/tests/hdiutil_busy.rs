//! P0-TEST-03: the testkit's `hdiutil` helper tries a `detach` six times
//! over about half a minute, so a disk busy for longer than the first few
//! seconds is detached all the same, and a `detach` that still fails says
//! what holds the image open and what state the disks are in.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
#![cfg(target_os = "macos")]

use std::fs;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use duscope_testkit::TestEnv;

/// An image attached without Finder at a mount point under the test's
/// temporary directory, with a file on it: the mount point and the file.
fn attached(env: &TestEnv, name: &str) -> (PathBuf, PathBuf) {
    let work = fs::canonicalize(env.tmp_dir()).unwrap().join("image");
    let mount = work.join("mnt");
    fs::create_dir_all(&mount).unwrap();
    let image = work.join("image.dmg");
    env.hdiutil(&[
        &"create",
        &"-ov",
        &"-size",
        &"16m",
        &"-fs",
        &"APFS",
        &"-volname",
        &name,
        &image,
    ]);
    env.hdiutil(&[&"attach", &"-nobrowse", &"-mountpoint", &mount, &image]);
    let held = mount.join("held");
    (mount, held)
}

fn platform() -> bool {
    if std::env::var_os("DUSCOPE_TEST_PLATFORM").is_none_or(|v| v != "1") {
        eprintln!("skipped: set DUSCOPE_TEST_PLATFORM=1");
        return false;
    }
    true
}

/// A file held open on the volume for six seconds, past the four that three
/// tries two seconds apart used to wait, and no `-force`: the tries during
/// those seconds are "Resource busy", and a later one detaches the image.
#[test]
#[ignore = "attaches a disk image; DUSCOPE_TEST_PLATFORM=1"]
fn p0_test_03_macos_a_detach_busy_for_six_seconds_succeeds() {
    let env = TestEnv::new();
    if !platform() {
        return;
    }
    let (mount, held) = attached(&env, "BUSY6");
    let file = fs::File::create(&held).unwrap();
    let release = thread::spawn(move || {
        thread::sleep(Duration::from_secs(6));
        drop(file);
    });
    let started = Instant::now();
    let detached = catch_unwind(AssertUnwindSafe(|| env.hdiutil(&[&"detach", &mount])));
    release.join().unwrap();
    if detached.is_err() {
        env.hdiutil(&[&"detach", &"-force", &mount]);
    }
    detached.expect("a detach busy for six seconds failed");
    assert!(
        started.elapsed() >= Duration::from_secs(6),
        "detached while the file was held: {:?}",
        started.elapsed()
    );
    assert!(!is_mounted(&mount), "{} is still mounted", mount.display());
}

/// A file held open on the volume throughout and no `-force`: every try is
/// "Resource busy", six of them, and the message names this process as
/// what holds the volume, as `lsof` lists it, then gives the disks' state:
/// the images attached, the volumes mounted, the image's whole disk, the
/// processes at work on disks, and `diskutil eject` naming this process as
/// the one that refuses the eject.
#[test]
#[ignore = "attaches a disk image; DUSCOPE_TEST_PLATFORM=1"]
fn p0_test_03_macos_a_detach_that_stays_busy_names_what_holds_it() {
    let env = TestEnv::new();
    if !platform() {
        return;
    }
    let (mount, held) = attached(&env, "BUSY");
    let file = fs::File::create(&held).unwrap();
    let failed = catch_unwind(AssertUnwindSafe(|| env.hdiutil(&[&"detach", &mount])));
    drop(file);
    env.hdiutil(&[&"detach", &"-force", &mount]);
    let message = failed
        .expect_err("a detach of a busy volume succeeded")
        .downcast::<String>()
        .unwrap();
    let pid = format!(" {} ", std::process::id());
    let dissenter = format!("dissented by PID {} ", std::process::id());
    let mounted = format!(" on {} (", mount.display());
    for each in [
        "failed 6 times",
        "try 6 on /dev/disk",
        "Resource busy",
        "held open, as lsof lists /dev/disk",
        pid.as_str(),
        "/held",
        "\n--- hdiutil info:\n",
        "image-path",
        "\n--- mount:\n",
        mounted.as_str(),
        "\n--- diskutil info:\n",
        "Device Node:",
        "\n--- processes at work on disks:\n",
        "diskarbitrationd",
        "\n--- diskutil eject:\n",
        dissenter.as_str(),
    ] {
        assert!(message.contains(each), "{each:?} not in: {message}");
    }
}

/// Whether `mount` is a mount point: its device differs from its parent's.
fn is_mounted(mount: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let parent = mount.parent().unwrap();
    fs::metadata(mount).unwrap().dev() != fs::metadata(parent).unwrap().dev()
}
