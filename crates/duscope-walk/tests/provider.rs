//! The platform provider on the fixture tree: totals equal `du`'s.
//!
//! `du` counts every entry once, a hard-linked non-directory only at its
//! first link. For disk usage both `du`s sum `st_blocks`. For the apparent
//! size, Apple's `du -A` sums each entry's size rounded up to 512-byte
//! blocks, and GNU's `--apparent-size` sums bytes; GNU coreutils 9.4 (the
//! version on Ubuntu 24.04) counts a directory's apparent size as 0, where
//! older releases count its `st_size`. Both print KiB rounded up. The walk's
//! entries are summed the same way.
//!
//! This file holds the one `cfg(target_os)` outside `src/platform/` that the
//! scanner's tests need: the facts it selects on are about the two `du`
//! implementations it compares against, not about the scanner.

#![cfg(any(target_os = "linux", target_os = "macos"))]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::collections::{HashMap, HashSet};
use std::path::Path;

use duscope_testkit::TestEnv;
use duscope_walk::{Kind, ScanEvent, WalkOptions, walk};

/// The walk's totals in KiB, as `du -k` prints them: (disk usage, apparent).
/// Apple's `du` leaves out a directory it cannot read, its own size
/// included (`FTS_DNR`); GNU `du` counts that directory's own size.
fn walk_totals(root: &Path, dir_apparent: bool) -> (u64, u64) {
    let apple = cfg!(target_os = "macos");
    // Apparent sizes in Apple's 512-byte blocks or in GNU's bytes.
    let apparent_unit = |bytes: u64| if apple { bytes.div_ceil(512) } else { bytes };
    let (mut blocks, mut apparent) = (0u64, 0u64);
    let mut seen = HashSet::new();
    let mut dirs = HashMap::new();
    walk(root, &WalkOptions::default(), |event| match event {
        ScanEvent::Entry(entry) => {
            if entry.kind != Kind::Dir && entry.nlink > 1 && !seen.insert((entry.dev, entry.ino)) {
                return;
            }
            let size = if entry.kind == Kind::Dir && !dir_apparent {
                0
            } else {
                entry.apparent
            };
            let own = (entry.allocated.div_ceil(512), apparent_unit(size));
            if let Some(id) = entry.dir {
                dirs.insert(id, own);
            }
            blocks += own.0;
            apparent += own.1;
        }
        ScanEvent::DirClosed {
            dir,
            errno: Some(_),
        } if apple => {
            let own = dirs[&dir];
            blocks -= own.0;
            apparent -= own.1;
        }
        _ => {}
    })
    .unwrap();
    let apparent_kib = if apple {
        apparent.div_ceil(2)
    } else {
        apparent.div_ceil(1024)
    };
    (blocks.div_ceil(2), apparent_kib)
}

/// The total `du` prints for `root` with these flags, in KiB. `du` exits 1
/// over the fixture's unreadable directories but still prints the total.
fn du(env: &TestEnv, flags: &[&str], root: &Path) -> u64 {
    let out = env.command("du").args(flags).arg(root).output().unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();
    let total = stdout.split_whitespace().next().unwrap_or_else(|| {
        panic!(
            "du printed no total: {}",
            String::from_utf8_lossy(&out.stderr)
        )
    });
    total.parse().unwrap()
}

#[cfg(target_os = "macos")]
#[test]
fn p1_walk_03_fixture_totals_equal_apple_du() {
    let env = TestEnv::new();
    let root = env.fixtures().root();
    let (allocated, apparent) = walk_totals(&root, true);
    assert_eq!(allocated, du(&env, &["-s", "-k"], &root), "du -sk");
    assert_eq!(apparent, du(&env, &["-s", "-k", "-A"], &root), "du -sk -A");
}

#[cfg(target_os = "linux")]
#[test]
fn p1_walk_02_fixture_totals_equal_gnu_du() {
    let env = TestEnv::new();
    let root = env.fixtures().root();
    // Whether this GNU du counts a directory's own apparent size: an empty
    // directory's is 0 in coreutils 9.4 and its st_size before.
    let empty = env.tmp_dir().join("empty-dir");
    std::fs::create_dir(&empty).unwrap();
    let out = env
        .command("du")
        .args(["-s", "-b"])
        .arg(&empty)
        .output()
        .unwrap();
    let probe = String::from_utf8(out.stdout).unwrap();
    let dir_apparent = probe.split_whitespace().next() != Some("0");
    let (allocated, apparent) = walk_totals(&root, dir_apparent);
    assert_eq!(allocated, du(&env, &["-s", "-k"], &root), "du -sk");
    assert_eq!(
        apparent,
        du(&env, &["-s", "-k", "--apparent-size"], &root),
        "du -sk --apparent-size"
    );
}
