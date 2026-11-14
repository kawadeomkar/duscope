//! P0-TEST-03: Spotlight keeps no volume for a disk image attached where
//! and as the image tests attach theirs, below the test's temporary
//! directory, browsable or with `-nobrowse`: `mds` has no store on it to
//! index into or to close before the image is detached.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
#![cfg(target_os = "macos")]

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use duscope_testkit::TestEnv;

/// What `mdutil -s` answers for a mount `mds` keeps no volume for.
const NO_VOLUME: &str = "Error: unknown indexing state.";

/// How long the volumes are watched: `mds` takes a volume it keeps, or
/// gives it up, within a second of its mount.
const WATCHED: Duration = Duration::from_secs(10);

/// The last line of `mdutil -s` for `mount`: how Spotlight treats the
/// volume mounted there.
fn spotlight_state(env: &TestEnv, mount: &Path) -> String {
    let out = env.command("mdutil").arg("-s").arg(mount).output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines().last().unwrap_or_default().trim().to_owned()
}

/// Two images made as the image tests make theirs and attached below the
/// test's temporary directory, one browsable, as the `FSEvents` tests need
/// theirs, one with `-nobrowse`, as the others attach theirs, files written
/// to both: for ten seconds `mdutil -s` answers that Spotlight has no
/// volume at either mount, and neither is given a `.Spotlight-V100` store.
/// Mounted elsewhere, the browsable image is given a store ("Indexing
/// disabled." for one a user attached), as an image is in `/Volumes`.
#[test]
#[ignore = "attaches a disk image; DUSCOPE_TEST_PLATFORM=1"]
fn p0_test_03_macos_spotlight_keeps_no_volume_for_the_images_the_tests_attach() {
    let env = TestEnv::new();
    if std::env::var_os("DUSCOPE_TEST_PLATFORM").is_none_or(|v| v != "1") {
        eprintln!("skipped: set DUSCOPE_TEST_PLATFORM=1");
        return;
    }
    let work = fs::canonicalize(env.tmp_dir()).unwrap().join("images");
    let mut mounts = Vec::new();
    for (name, browsable) in [("BROWSABLE", true), ("HIDDEN", false)] {
        let mount = work.join(name).join("mnt");
        fs::create_dir_all(&mount).unwrap();
        let image = work.join(name).join("image.dmg");
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
        if browsable {
            env.hdiutil(&[&"attach", &"-mountpoint", &mount, &image]);
        } else {
            env.hdiutil(&[&"attach", &"-nobrowse", &"-mountpoint", &mount, &image]);
        }
        fs::create_dir_all(mount.join("dir")).unwrap();
        fs::write(mount.join("dir/file.txt"), b"words to index").unwrap();
        mounts.push((name, mount));
    }
    let mut seen = Vec::new();
    let started = Instant::now();
    while seen.is_empty() && started.elapsed() < WATCHED {
        for (name, mount) in &mounts {
            let state = spotlight_state(&env, mount);
            let store = mount.join(".Spotlight-V100").exists();
            if state != NO_VOLUME || store {
                seen.push(format!(
                    "{name} after {:?}: {state}, store: {store}",
                    started.elapsed()
                ));
            }
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    for (_, mount) in mounts.iter().rev() {
        env.hdiutil(&[&"detach", &"-force", mount]);
    }
    assert!(seen.is_empty(), "Spotlight took a volume: {seen:?}");
}
