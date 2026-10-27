//! The platform provider on the fixture tree: totals equal `du`'s.
//!
//! `du` counts every entry once, a hard-linked non-directory only at its
//! first link, and sums 512-byte blocks: `st_blocks` for disk usage, the
//! apparent size rounded up to whole blocks for `-A` (macOS) and
//! `--apparent-size` (GNU). The walk's entries are summed the same way.

// Linux has no provider until P1-WALK-02, which adds its `du` check here.
#![cfg(target_os = "macos")]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::collections::{HashMap, HashSet};
use std::path::Path;

use duscope_testkit::TestEnv;
use duscope_walk::{Kind, ScanEvent, WalkOptions, walk};

/// The walk's totals in KiB, as `du -k` prints them: (disk usage, apparent).
/// Apple's `du` leaves out a directory it cannot read, its own size
/// included (`FTS_DNR`); GNU `du` counts that directory's own size.
fn walk_totals(root: &Path) -> (u64, u64) {
    let (mut blocks, mut apparent) = (0u64, 0u64);
    let mut seen = HashSet::new();
    let mut dirs = HashMap::new();
    walk(root, &WalkOptions::default(), |event| match event {
        ScanEvent::Entry(entry) => {
            if entry.kind != Kind::Dir && entry.nlink > 1 && !seen.insert((entry.dev, entry.ino)) {
                return;
            }
            let own = (entry.allocated.div_ceil(512), entry.apparent.div_ceil(512));
            if let Some(id) = entry.dir {
                dirs.insert(id, own);
            }
            blocks += own.0;
            apparent += own.1;
        }
        ScanEvent::DirClosed {
            dir,
            errno: Some(_),
        } if cfg!(target_os = "macos") => {
            let own = dirs[&dir];
            blocks -= own.0;
            apparent -= own.1;
        }
        _ => {}
    })
    .unwrap();
    (blocks.div_ceil(2), apparent.div_ceil(2))
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

#[test]
fn p1_walk_03_fixture_totals_equal_apple_du() {
    let env = TestEnv::new();
    let root = env.fixtures().root();
    let (allocated, apparent) = walk_totals(&root);
    assert_eq!(allocated, du(&env, &["-s", "-k"], &root), "du -sk");
    assert_eq!(apparent, du(&env, &["-s", "-k", "-A"], &root), "du -sk -A");
}
