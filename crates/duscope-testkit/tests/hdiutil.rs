//! P0-TEST-01: the testkit's `hdiutil` helper counts a `detach` done once
//! nothing is attached at its target, as when an earlier try detached the
//! image while reporting an error, and still fails, with every try's
//! output, a `detach` that leaves its image attached.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
#![cfg(target_os = "macos")]

use std::fs;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::{Duration, Instant};

use duscope_testkit::TestEnv;

/// `hdiutil detach` of a directory nothing is mounted on fails with "No
/// such file or directory", as every try after a detach that finished
/// while reporting an error does: done at the first try, not tried again.
#[test]
fn p0_test_01_a_detach_with_nothing_attached_is_done() {
    let env = TestEnv::new();
    let empty = fs::canonicalize(env.tmp_dir()).unwrap().join("not-mounted");
    fs::create_dir_all(&empty).unwrap();
    let started = Instant::now();
    env.hdiutil(&[&"detach", &"-force", &empty]);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "tried again: {:?}",
        started.elapsed()
    );
}

/// A file held open on the volume and no `-force`: every try is "Resource
/// busy" and leaves the image attached, so the detach still fails, with
/// each try's output.
#[test]
#[ignore = "attaches a disk image; DUSCOPE_TEST_PLATFORM=1"]
fn p0_test_01_macos_a_detach_that_leaves_the_image_attached_fails() {
    let env = TestEnv::new();
    if std::env::var_os("DUSCOPE_TEST_PLATFORM").is_none_or(|v| v != "1") {
        eprintln!("skipped: set DUSCOPE_TEST_PLATFORM=1");
        return;
    }
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
        &"BUSY",
        &image,
    ]);
    env.hdiutil(&[&"attach", &"-nobrowse", &"-mountpoint", &mount, &image]);
    let held = fs::File::create(mount.join("held")).unwrap();
    let failed = catch_unwind(AssertUnwindSafe(|| env.hdiutil(&[&"detach", &mount])));
    drop(held);
    env.hdiutil(&[&"detach", &"-force", &mount]);
    let message = failed
        .expect_err("a detach of a busy volume succeeded")
        .downcast::<String>()
        .unwrap();
    for each in ["try 1", "try 2", "try 3", "Resource busy"] {
        assert!(message.contains(each), "{message}");
    }
}
