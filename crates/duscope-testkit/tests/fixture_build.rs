//! P0-TEST-01: the reference tree builds within the 30-second budget, and a
//! second build from the same manifest produces an identical listing (and
//! identical `find -printf` output where GNU find exists).
//!
//! Both tests share one build in a private output directory under
//! `target/fixtures-tests.noindex/`, taken in turn under a lock so the timed build
//! never competes with another build of this binary.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use duscope_testkit::TestEnv;
use mktree::{OutDir, Plan, Record};

/// The acceptance budget for building the tree.
const BUDGET: Duration = Duration::from_secs(30);

/// The fields of the Rust listing, as `find -printf` writes them.
const FIND_FORMAT: &str = "%P\t%y\t%s\t%m\t%T@\t%n\t%l\n";

/// A private output directory, removed (permissions restored) on drop.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = mktree::remove_tree(&self.0);
        if let Some(parent) = self.0.parent() {
            let _ = std::fs::remove_dir(parent);
        }
    }
}

struct First {
    _dir: Scratch,
    out: OutDir,
    plan: Plan,
    record: Record,
    elapsed: Duration,
    listing: String,
}

static FIRST: Mutex<Option<First>> = Mutex::new(None);
static FINISHED: AtomicUsize = AtomicUsize::new(0);

/// Runs `f` on the first build (building it if no test has yet), holding
/// the lock; the second test to finish removes the build.
fn with_first_build(f: impl FnOnce(&mut First)) {
    let mut first = FIRST.lock().unwrap_or_else(PoisonError::into_inner);
    let first_ref = first.get_or_insert_with(build_first);
    f(first_ref);
    if FINISHED.fetch_add(1, Ordering::SeqCst) + 1 == 2 {
        *first = None;
    }
}

fn build_first() -> First {
    let base = OutDir::workspace_default()
        .path()
        .with_file_name("fixtures-tests.noindex");
    let dir = base.join("p0-test-01");
    mktree::remove_tree(&dir).unwrap();
    let out = OutDir::new(&dir);
    let plan = Plan::embedded().unwrap();
    let started = Instant::now();
    let (record, built) = out.ensure(&plan, false).unwrap();
    let elapsed = started.elapsed();
    assert!(built);
    let listing = mktree::listing(&out.tree()).unwrap();
    First {
        _dir: Scratch(dir),
        out,
        plan,
        record,
        elapsed,
        listing,
    }
}

/// `find <tree> -printf ...` output with its lines sorted bytewise (raw bytes:
/// Linux names need not be UTF-8) when the system has GNU find (`find` on
/// Linux, `gfind` on macOS with findutils), else `None`. find lists in readdir
/// order, which belongs to the filesystem, not to the generator: two builds of
/// the 100,000-entry directory have listed in different orders on ext4.
fn gnu_find(env: &TestEnv, tree: &Path) -> Option<Vec<u8>> {
    for program in ["find", "gfind"] {
        let gnu = env
            .command(program)
            .arg("--version")
            .output()
            .is_ok_and(|o| {
                o.status.success() && String::from_utf8_lossy(&o.stdout).contains("GNU findutils")
            });
        if gnu {
            let out = env
                .command(program)
                .arg(tree)
                .arg("-printf")
                .arg(FIND_FORMAT)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{program}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let mut lines: Vec<&[u8]> = out
                .stdout
                .split(|&c| c == b'\n')
                .filter(|line| !line.is_empty())
                .collect();
            lines.sort_unstable();
            let mut sorted = lines.join(&b'\n');
            sorted.push(b'\n');
            return Some(sorted);
        }
    }
    None
}

/// The first line where two listings differ, for a readable failure.
fn first_difference(a: &[u8], b: &[u8]) -> Option<String> {
    let (mut la, mut lb) = (a.split(|&c| c == b'\n'), b.split(|&c| c == b'\n'));
    for n in 1.. {
        match (la.next(), lb.next()) {
            (None, None) => return None,
            (x, y) if x == y => {}
            (x, y) => {
                let show = |l: Option<&[u8]>| l.map(|l| String::from_utf8_lossy(l).into_owned());
                return Some(format!(
                    "line {n}: first {:?}, second {:?}",
                    show(x),
                    show(y)
                ));
            }
        }
    }
    None
}

#[test]
fn p0_test_01_tree_builds_within_budget() {
    let _env = TestEnv::new();
    with_first_build(|first| {
        // Written past the test harness's capture, so every CI log shows how
        // far each runner is from the budget, not only a failing one.
        let _ = writeln!(
            std::io::stderr(),
            "p0_test_01: built {} entries in {:.2} s of the {} s budget on {} ({}), manifest hash {}",
            first.record.entries,
            first.elapsed.as_secs_f64(),
            BUDGET.as_secs(),
            first.record.platform,
            first.record.filesystem,
            first.record.hash
        );
        for case in first.record.skipped() {
            eprintln!(
                "p0_test_01: skipped {}: {} (instead: {})",
                case.name, case.detail, case.fallback
            );
        }
        assert!(
            first.elapsed < BUDGET,
            "the tree took {:.1} s to build, over the {} s budget",
            first.elapsed.as_secs_f64(),
            BUDGET.as_secs()
        );
        let absent: usize = first.record.cases.iter().map(|c| c.absent.len()).sum();
        assert_eq!(
            usize::try_from(first.record.entries).unwrap(),
            first.plan.entries.len() - absent
        );
        assert!(
            first.record.entries > 145_000,
            "{} entries",
            first.record.entries
        );
    });
}

#[test]
fn p0_test_01_rebuild_lists_identically() {
    let env = TestEnv::new();
    with_first_build(|first| {
        let report = mktree::verify(&first.out.tree(), &first.plan, &first.record).unwrap();
        assert!(
            report.problems.is_empty(),
            "the first build differs from the manifest:\n{}",
            report.problems.join("\n")
        );
        if !report.read_after_build.is_empty() {
            // On macOS, Spotlight reads new files (a `.png`, a `.pdf`) and the
            // system moves their atime even when it lies in the future.
            eprintln!(
                "p0_test_01: {} entries were read after the build (atime moved), e.g. {}",
                report.read_after_build.len(),
                report.read_after_build[0]
            );
        }
        let find_first = gnu_find(&env, &first.out.tree());

        // A second run with the same manifest, as `just fixtures --force`.
        let started = Instant::now();
        let (second, built) = first.out.ensure(&first.plan, true).unwrap();
        let elapsed = started.elapsed();
        assert!(built);
        eprintln!(
            "p0_test_01: rebuilt (removal included) in {:.2} s; build alone {:.2} s",
            elapsed.as_secs_f64(),
            Duration::from_millis(second.build_ms).as_secs_f64()
        );
        let listing = mktree::listing(&first.out.tree()).unwrap();
        if let Some(diff) = first_difference(first.listing.as_bytes(), listing.as_bytes()) {
            panic!("the second build lists differently: {diff}");
        }
        assert_eq!(listing.lines().count() as u64, second.entries);
        match (find_first, gnu_find(&env, &first.out.tree())) {
            (Some(a), Some(b)) => {
                if let Some(diff) = first_difference(&a, &b) {
                    panic!("find -printf lists the second build differently: {diff}");
                }
                eprintln!(
                    "p0_test_01: find -printf listings identical ({} lines, sorted)",
                    a.split(|&c| c == b'\n').count() - 1
                );
            }
            _ => eprintln!("p0_test_01: no GNU find here; compared the Rust listing only"),
        }
        assert_eq!(second.hash, first.record.hash);
        assert_eq!(
            second.cases, first.record.cases,
            "cases built or skipped differently"
        );
        assert_eq!(second.entries, first.record.entries);
    });
}
