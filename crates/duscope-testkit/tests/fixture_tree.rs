//! P0-TEST-01: the shared fixture tree holds every case of the Testing
//! strategy, a project marker for every cache rule and every by-type
//! directory rule, matches its manifest, and its unreadable directories carry
//! their test modes only while a test holds the tree.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::process::Command;

use duscope_testkit::{Status, TestEnv};
use mktree::{Kind, OutDir, Plan};

/// The caches.toml rules of the proposal ("Query verb definitions"): the
/// directory names and the markers any of which, in an ancestor, makes them
/// caches.
const CACHE_RULES: &[(&[&str], &[&str])] = &[
    (
        &["node_modules", ".next", ".nuxt", ".turbo", ".parcel-cache"],
        &["package.json"],
    ),
    (&["target"], &["Cargo.toml"]),
    (
        &[
            ".venv",
            "venv",
            "__pycache__",
            ".mypy_cache",
            ".pytest_cache",
            ".ruff_cache",
            ".tox",
        ],
        &["pyproject.toml", "setup.py", "requirements.txt"],
    ),
    (
        &["build", ".gradle"],
        &["build.gradle", "build.gradle.kts", "CMakeLists.txt"],
    ),
    (&["Pods"], &["Podfile"]),
    (&["vendor"], &["composer.json"]),
];

/// The cases that may be skipped on some platform or filesystem.
const PLATFORM_DEPENDENT: &[&str] = &[
    "clones",
    "sparse",
    "compressed",
    "non-utf8-names",
    "special-files",
    "unreadable",
    "permissions",
];

fn name(path: &[u8]) -> &[u8] {
    path.rsplit(|&b| b == b'/').next().unwrap_or(path)
}

fn ancestors(path: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut rest = Some(path);
    std::iter::from_fn(move || {
        let p = rest?;
        let parent = match p.iter().rposition(|&b| b == b'/') {
            Some(i) => &p[..i],
            None if p.is_empty() => {
                rest = None;
                return None;
            }
            None => &[][..],
        };
        rest = Some(parent);
        Some(parent)
    })
}

fn join(dir: &[u8], name: &str) -> Vec<u8> {
    if dir.is_empty() {
        name.as_bytes().to_vec()
    } else {
        [dir, b"/", name.as_bytes()].concat()
    }
}

fn is_file(plan: &Plan, path: &[u8]) -> bool {
    plan.get(path).is_some_and(|e| e.kind != Kind::Dir)
}

/// Directories named `dir_name`.
fn dirs_named<'p>(plan: &'p Plan, dir_name: &str) -> impl Iterator<Item = &'p [u8]> {
    plan.entries
        .iter()
        .filter(move |e| e.kind == Kind::Dir && name(&e.path) == dir_name.as_bytes())
        .map(|e| e.path.as_slice())
}

/// Whether some regular file lies below `dir`.
fn holds_a_file(plan: &Plan, dir: &[u8]) -> bool {
    let prefix = [dir, b"/"].concat();
    plan.entries
        .iter()
        .any(|e| e.path.starts_with(&prefix) && matches!(e.kind, Kind::File { .. }))
}

/// The markers found in the ancestors of `dir`.
fn markers_above<'a>(plan: &Plan, dir: &[u8], markers: &[&'a str]) -> Vec<&'a str> {
    ancestors(dir)
        .flat_map(|a| markers.iter().filter(move |m| is_file(plan, &join(a, m))))
        .copied()
        .collect()
}

#[test]
fn p0_test_01_tree_has_every_listed_case() {
    let plan = Plan::embedded().unwrap();
    let kinds: BTreeSet<&str> = plan.entries.iter().map(|e| e.kind.name()).collect();
    for kind in [
        "dir",
        "file",
        "text",
        "sparse",
        "compressed",
        "hardlink",
        "clone",
        "symlink",
        "fifo",
        "socket",
    ] {
        assert!(kinds.contains(kind), "no {kind} entry");
    }

    // Hard links: one inode from three directories.
    let links = plan.link_counts();
    let (target, n) = links.iter().max_by_key(|(_, n)| **n).unwrap();
    assert!(*n >= 3, "most-linked file has {n} extra links");
    let dirs: BTreeSet<&[u8]> = plan
        .entries
        .iter()
        .filter(|e| {
            e.kind
                == (Kind::Hardlink {
                    target: target.to_vec(),
                })
        })
        .map(mktree::Entry::parent)
        .collect();
    assert!(
        dirs.len() >= 2,
        "links of one file in {} directories",
        dirs.len()
    );

    // Clones, a hole-only sparse file and one with data, compressed files.
    assert!(
        plan.entries
            .iter()
            .filter(|e| matches!(e.kind, Kind::Clone { .. }))
            .count()
            >= 2
    );
    assert!(
        plan.entries
            .iter()
            .any(|e| matches!(&e.kind, Kind::Sparse { data, .. } if data.is_empty()))
    );
    assert!(
        plan.entries
            .iter()
            .any(|e| matches!(&e.kind, Kind::Sparse { data, .. } if data.len() >= 2))
    );

    // Unreadable directories: mode 000 with content beneath.
    assert!(
        plan.entries
            .iter()
            .any(|e| e.test_mode == Some(0) && holds_a_file(&plan, &e.path))
    );

    // Names that are not valid UTF-8.
    assert!(plan.entries.iter().filter(|e| !e.utf8()).count() >= 5);

    // A 200-level path.
    let depth = |p: &[u8]| p.split(|&b| b == b'/').count() - 1;
    let deepest = plan.entries.iter().map(|e| depth(&e.path)).max().unwrap();
    assert!(deepest >= 200, "deepest path has {deepest} levels");

    // A 100,000-entry directory.
    let mut children: BTreeMap<&[u8], usize> = BTreeMap::new();
    for e in &plan.entries {
        if !e.path.is_empty() {
            *children.entry(e.parent()).or_default() += 1;
        }
    }
    assert!(children.values().any(|&n| n >= 100_000));

    // Symlink cycles: a link to its own directory, one to an ancestor, a
    // two-link loop and a self-loop.
    let target_of = |p: &[u8]| match &plan.get(p).unwrap().kind {
        Kind::Symlink { target } => target.clone(),
        other => panic!("{other:?}"),
    };
    let symlinks: Vec<&[u8]> = plan
        .entries
        .iter()
        .filter(|e| matches!(e.kind, Kind::Symlink { .. }))
        .map(|e| e.path.as_slice())
        .collect();
    assert!(symlinks.iter().any(|p| target_of(p) == b"."));
    assert!(symlinks.iter().any(|p| target_of(p) == b".."));
    assert!(symlinks.iter().any(|p| target_of(p) == name(p)));
    assert!(symlinks.iter().any(|p| {
        let sibling = [&p[..p.len() - name(p).len()], target_of(p).as_slice()].concat();
        sibling.as_slice() != *p
            && plan.get(&sibling).is_some_and(
                |o| matches!(&o.kind, Kind::Symlink { target } if target.as_slice() == name(p)),
            )
    }));

    // A valid CACHEDIR.TAG.
    assert!(plan.entries.iter().any(|e| name(&e.path) == b"CACHEDIR.TAG"
        && matches!(&e.kind, Kind::File { text: Some(t), .. } if t.starts_with(b"Signature: 8a477f597d28d172789f06886806bc55"))));

    // About 50,000 entries beside the 100,000-entry directory.
    let wide: usize = children.values().copied().max().unwrap();
    let rest = plan.entries.len() - wide;
    assert!(
        (45_000..=55_000).contains(&rest),
        "{rest} entries outside the wide directory"
    );
    assert_eq!(plan.instant_text(), duscope_testkit::test_now());
}

#[test]
fn p0_test_01_tree_has_a_marker_for_every_cache_rule() {
    let plan = Plan::embedded().unwrap();
    for (names, markers) in CACHE_RULES {
        let mut seen_markers = BTreeSet::new();
        for dir_name in *names {
            let mut matched = false;
            for dir in dirs_named(&plan, dir_name) {
                let found = markers_above(&plan, dir, markers);
                if !found.is_empty() && holds_a_file(&plan, dir) {
                    matched = true;
                    seen_markers.extend(found);
                }
            }
            assert!(
                matched,
                "no {dir_name} with content below one of {markers:?}"
            );
        }
        for marker in *markers {
            assert!(
                seen_markers.contains(marker),
                "no cache directory below a {marker}"
            );
        }
    }
    // DerivedData under Library/Developer/Xcode.
    assert!(
        dirs_named(&plan, "DerivedData")
            .any(|d| d.ends_with(b"/Library/Developer/Xcode/DerivedData") && holds_a_file(&plan, d))
    );
    // A target with no Cargo.toml in any ancestor (absent unless --loose).
    assert!(
        dirs_named(&plan, "target").any(|d| markers_above(&plan, d, &["Cargo.toml"]).is_empty())
    );
}

#[test]
fn p0_test_01_tree_has_every_class_directory_rule() {
    let plan = Plan::embedded().unwrap();
    for dir_name in [
        "node_modules",
        ".venv",
        "site-packages",
        "IndexedDB",
        "DerivedData",
    ] {
        assert!(
            dirs_named(&plan, dir_name).any(|d| holds_a_file(&plan, d)),
            "no {dir_name} with content"
        );
    }
    let dirs = || {
        plan.entries
            .iter()
            .filter(|e| e.kind == Kind::Dir)
            .map(|e| e.path.as_slice())
    };
    assert!(dirs().any(|d| d.ends_with(b".git/objects") && holds_a_file(&plan, d)));
    assert!(dirs().any(|d| name(d).ends_with(b".framework") && holds_a_file(&plan, d)));
    assert!(dirs().any(|d| {
        d.ends_with(b"/Contents/MacOS")
            && name(&d[..d.len() - b"/Contents/MacOS".len()]).ends_with(b".app")
            && holds_a_file(&plan, d)
    }));
    for ext in [".tar.gz", ".tar.bz2", ".tar.xz", ".tar.zst", ".tar.lz4"] {
        assert!(
            plan.entries
                .iter()
                .any(|e| name(&e.path).ends_with(ext.as_bytes())),
            "no {ext}"
        );
    }
    assert!(plan.entries.iter().any(|e| {
        matches!(e.kind, Kind::File { .. }) && e.mode & 0o111 != 0 && !name(&e.path).contains(&b'.')
    }));
}

#[test]
fn p0_test_01_tree_matches_the_manifest() {
    let env = TestEnv::new();
    let fixtures = env.fixtures();
    let plan = fixtures.plan();
    assert_eq!(
        fixtures.hash(),
        plan.hash(),
        "the tree was built from another manifest"
    );
    assert_eq!(fixtures.instant(), env.test_now());
    let report = mktree::verify(&fixtures.root(), plan, fixtures.record()).unwrap();
    if !report.read_after_build.is_empty() {
        eprintln!(
            "p0_test_01: {} entries were read after the build (atime moved), e.g. {}",
            report.read_after_build.len(),
            report.read_after_build[0]
        );
    }
    let problems = report.problems;
    assert!(
        problems.is_empty(),
        "{} differences, the first:\n{}",
        problems.len(),
        problems[..problems.len().min(40)].join("\n")
    );
    let recorded: Vec<&str> = fixtures
        .record()
        .cases
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    let planned: Vec<&str> = plan.cases.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(
        recorded, planned,
        "every case is recorded, built or skipped"
    );
    for case in fixtures.skipped() {
        assert!(
            PLATFORM_DEPENDENT.contains(&case.name.as_str()),
            "{} skipped: {}",
            case.name,
            case.detail
        );
        assert!(
            !case.detail.is_empty() && !case.fallback.is_empty(),
            "{case:?}"
        );
        eprintln!(
            "p0_test_01: skipped here: {}: {} (instead: {})",
            case.name, case.detail, case.fallback
        );
    }
    for case in &fixtures.record().cases {
        if case.status == Status::Built {
            assert!(case.absent.is_empty(), "{case:?}");
        }
    }
}

/// The record says a case was built only when its files really are stored
/// compressed: a phase-1 test gates allocated < apparent on it.
#[test]
fn p0_test_01_compressed_files_recorded_built_take_less_than_their_size() {
    let env = TestEnv::new();
    let fixtures = env.fixtures();
    let case = fixtures.case("compressed");
    if case.status != Status::Built {
        eprintln!(
            "p0_test_01: compressed is skipped here: {} (instead: {})",
            case.detail, case.fallback
        );
        return;
    }
    let root = fixtures.root();
    let files: Vec<_> = fixtures
        .plan()
        .entries
        .iter()
        .filter(|e| matches!(e.kind, Kind::Compressed { .. }))
        .collect();
    assert!(!files.is_empty());
    for e in files {
        let meta = fs::symlink_metadata(mktree::build::abs(&root, &e.path)).unwrap();
        assert!(
            meta.blocks() * 512 < meta.len(),
            "{} is recorded compressed but takes {} of its {} bytes ({})",
            e.display(),
            meta.blocks() * 512,
            meta.len(),
            case.detail
        );
    }
}

#[test]
fn p0_test_01_unreadable_dirs_carry_their_test_mode_while_held() {
    let env = TestEnv::new();
    let fixtures = env.fixtures();
    let mode = |p: &std::path::Path| fs::symlink_metadata(p).unwrap().permissions().mode() & 0o7777;
    let closed = fixtures.case_root("unreadable").join("closed");
    let list_only = fixtures.case_root("permissions").join("list-only");
    assert_eq!(mode(&closed), 0o000);
    assert_eq!(
        mode(&fixtures.case_root("permissions").join("empty-closed")),
        0o000
    );
    assert_eq!(mode(&list_only), 0o444);
    assert_eq!(mode(&fixtures.case_root("unreadable").join("open")), 0o755);
    if fixtures.unreadable_effective() {
        assert!(fs::read_dir(&closed).is_err());
        assert!(fs::read_dir(&list_only).is_ok());
        assert!(fs::symlink_metadata(list_only.join("x.txt")).is_err());
    } else {
        assert!(!fixtures.built("unreadable"));
        eprintln!(
            "p0_test_01: mode 000 denies nothing to this process (root); skipped the access checks"
        );
    }
}

/// A tree holding only the directories that have a test mode, each with a
/// file inside.
fn mini_tree(out: &OutDir, plan: &Plan) {
    for e in plan.entries.iter().filter(|e| e.test_mode.is_some()) {
        let dir = mktree::build::abs(&out.tree(), &e.path);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("inside"), b"x").unwrap();
    }
}

fn modes(out: &OutDir, plan: &Plan) -> Vec<(u32, u32, u32)> {
    plan.entries
        .iter()
        .filter_map(|e| {
            let test_mode = e.test_mode?;
            let path = mktree::build::abs(&out.tree(), &e.path);
            let now = fs::symlink_metadata(path).unwrap().permissions().mode() & 0o7777;
            Some((now, e.mode, test_mode))
        })
        .collect()
}

#[test]
fn p0_test_01_test_modes_are_restored_and_trees_stay_removable() {
    let env = TestEnv::new();
    let plan = Plan::embedded().unwrap();
    let out = OutDir::new(env.root().join("out"));
    mini_tree(&out, &plan);
    assert!(!modes(&out, &plan).is_empty());

    // Held: the test modes apply. Released by the last holder: the modes at
    // rest return, and a plain recursive removal (what `cargo clean` does)
    // succeeds.
    out.hold(&plan).unwrap();
    assert!(modes(&out, &plan).iter().all(|(now, _, test)| now == test));
    out.release(&plan).unwrap();
    assert!(modes(&out, &plan).iter().all(|(now, rest, _)| now == rest));

    // Another live process holding the tree keeps it armed after this one
    // releases; once that process is gone, the next release restores it.
    let mut other = Command::new("sleep").arg("60").spawn().unwrap();
    fs::create_dir_all(out.path().join("holders")).unwrap();
    fs::write(out.path().join("holders").join(other.id().to_string()), b"").unwrap();
    out.hold(&plan).unwrap();
    out.release(&plan).unwrap();
    assert!(
        modes(&out, &plan).iter().all(|(now, _, test)| now == test),
        "released under a live holder"
    );
    other.kill().unwrap();
    other.wait().unwrap();
    out.hold(&plan).unwrap();
    out.release(&plan).unwrap();
    assert!(modes(&out, &plan).iter().all(|(now, rest, _)| now == rest));
    fs::remove_dir_all(out.tree()).unwrap();

    // An armed tree is still removed by mktree (as `just fixtures --force`
    // and test clean-up do), permissions restored first.
    mini_tree(&out, &plan);
    out.arm(&plan).unwrap();
    mktree::remove_tree(&out.tree()).unwrap();
    assert!(!out.tree().exists());

    // `mktree --disarm` forgets holders and restores the modes.
    mini_tree(&out, &plan);
    out.hold(&plan).unwrap();
    out.disarm(&plan).unwrap();
    assert!(modes(&out, &plan).iter().all(|(now, rest, _)| now == rest));
    assert!(!out.path().join("holders").exists());
}
