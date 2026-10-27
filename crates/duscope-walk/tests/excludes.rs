//! P1-WALK-09: excludes at scan time: globs, gitignore files with
//! negations, and cache directories.

#![cfg(any(target_os = "linux", target_os = "macos"))]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use duscope_testkit::TestEnv;
use duscope_walk::{
    CacheRule, DirId, IgnoreRules, MemProvider, ScanEvent, Skip, WalkOptions, WalkSummary, walk,
    walk_with,
};
use globset::{Glob, GlobBuilder, GlobSet, GlobSetBuilder};

/// Entries delivered and directories skipped, by path relative to the root.
fn walked(
    root: &Path,
    run: impl FnOnce(&mut dyn FnMut(ScanEvent)) -> WalkSummary,
) -> (Vec<String>, BTreeMap<String, Skip>, WalkSummary) {
    let mut dirs: HashMap<DirId, PathBuf> = HashMap::new();
    let mut entries = Vec::new();
    let mut skipped = BTreeMap::new();
    let summary = run(&mut |event| match event {
        ScanEvent::Entry(entry) => {
            let path = if entry.dir == Some(DirId::ROOT) {
                PathBuf::new()
            } else {
                dirs[&entry.parent].join(entry.name.as_os_str())
            };
            if let Some(id) = entry.dir {
                dirs.insert(id, path.clone());
            }
            entries.push(path.to_string_lossy().into_owned());
        }
        ScanEvent::Skipped { dir, reason } => {
            skipped.insert(dirs[&dir].to_string_lossy().into_owned(), reason);
        }
        _ => {}
    });
    let _ = root;
    entries.sort();
    (entries, skipped, summary)
}

fn mem_walk(
    tree: &MemProvider,
    opts: &WalkOptions,
) -> (Vec<String>, BTreeMap<String, Skip>, WalkSummary) {
    let root = tree.root_path().to_owned();
    walked(&root, |sink| walk_with(tree, &root, opts, sink).unwrap())
}

/// Globs as the binary compiles `--exclude`: `/` anchors, anything else
/// matches at every depth, `*` within a component.
fn globs(patterns: &[&str]) -> GlobSet {
    let mut set = GlobSetBuilder::new();
    for p in patterns {
        let glob = match p.strip_prefix('/') {
            Some(anchored) => anchored.to_owned(),
            None => format!("**/{p}"),
        };
        set.add(
            GlobBuilder::new(&glob)
                .literal_separator(true)
                .build()
                .unwrap(),
        );
    }
    set.build().unwrap()
}

fn tree() -> MemProvider {
    let mut tree = MemProvider::new("/t");
    tree.file("a.tmp", 10)
        .file("keep.log", 10)
        .file("drop.log", 10)
        .file("src/main.rs", 10)
        .file("src/old.tmp", 10)
        .file("build/out.o", 10)
        .file("src/build/gen.rs", 10)
        .file("app/package.json", 10)
        .file("app/node_modules/x/index.js", 10)
        .file("lib/node_modules/y/index.js", 10);
    tree
}

#[test]
fn p1_walk_09_excluded_globs_are_left_out() {
    let opts = WalkOptions {
        excludes: globs(&["*.tmp", "/build"]),
        ..WalkOptions::default()
    };
    let (entries, skipped, summary) = mem_walk(&tree(), &opts);
    // Files that match are left out, at every depth.
    let tmp = |e: &String| Path::new(e).extension().is_some_and(|x| x == "tmp");
    assert!(!entries.iter().any(tmp), "{entries:?}");
    // A directory that matches is listed, not entered: only the anchored
    // build/ at the root.
    assert_eq!(
        skipped,
        BTreeMap::from([("build".to_owned(), Skip::Excluded)])
    );
    assert!(entries.contains(&"build".to_owned()));
    assert!(!entries.contains(&"build/out.o".to_owned()));
    assert!(entries.contains(&"src/build/gen.rs".to_owned()));
    assert_eq!(summary.excluded, 3);
}

#[test]
fn p1_walk_09_ignore_files_honour_negations() {
    let mut ignore = IgnoreRules::new();
    ignore
        .add(b"*.log\n!keep.log\nnode_modules/\n", "ignore")
        .unwrap();
    let opts = WalkOptions {
        ignore,
        ..WalkOptions::default()
    };
    let (entries, skipped, summary) = mem_walk(&tree(), &opts);
    assert!(entries.contains(&"keep.log".to_owned()));
    assert!(!entries.contains(&"drop.log".to_owned()));
    assert_eq!(
        skipped,
        BTreeMap::from([
            ("app/node_modules".to_owned(), Skip::Excluded),
            ("lib/node_modules".to_owned(), Skip::Excluded),
        ])
    );
    assert_eq!(summary.excluded, 3);
}

#[test]
fn p1_walk_09_cache_rules_need_their_marker_in_the_parent_listing() {
    let opts = WalkOptions {
        exclude_caches: vec![CacheRule {
            name: Glob::new("node_modules").unwrap(),
            markers: vec![OsString::from("package.json")],
        }],
        ..WalkOptions::default()
    };
    let (entries, skipped, _) = mem_walk(&tree(), &opts);
    // app/ lists package.json; lib/ does not.
    assert_eq!(
        skipped,
        BTreeMap::from([("app/node_modules".to_owned(), Skip::ExcludedCache)])
    );
    assert!(entries.contains(&"lib/node_modules/y/index.js".to_owned()));
}

#[test]
fn p1_walk_09_a_marker_left_for_a_later_batch_still_counts() {
    // A directory wide enough that its read leaves names for stat batches:
    // the marker sorts last, the cache first.
    let mut tree = MemProvider::new("/t");
    tree.dir("wide/aaa_cache");
    for i in 0..2_000 {
        tree.file(format!("wide/f{i:04}"), 1);
    }
    tree.file("wide/zz.marker", 1);
    let opts = WalkOptions {
        exclude_caches: vec![CacheRule {
            name: Glob::new("aaa_cache").unwrap(),
            markers: vec![OsString::from("zz.marker")],
        }],
        ..WalkOptions::default()
    };
    let (_, skipped, _) = mem_walk(&tree, &opts);
    assert_eq!(
        skipped,
        BTreeMap::from([("wide/aaa_cache".to_owned(), Skip::ExcludedCache)])
    );
}

#[test]
fn p1_walk_09_a_valid_cachedir_tag_keeps_the_scan_out() {
    let env = TestEnv::new();
    let root = env.fixtures().case_root("cachedir-tag");
    let opts = WalkOptions {
        // Any cache rule turns the CACHEDIR.TAG check on.
        exclude_caches: vec![CacheRule {
            name: Glob::new("never-matches").unwrap(),
            markers: vec![OsString::from("x")],
        }],
        ..WalkOptions::default()
    };
    let (_, skipped, _) = walked(&root, |sink| walk(&root, &opts, sink).unwrap());
    // A regular file with the 43-byte signature, with or without the usual
    // comment; not a wrong signature, a truncated tag, a directory or a
    // link to a valid tag.
    assert_eq!(
        skipped,
        BTreeMap::from([
            ("exact".to_owned(), Skip::ExcludedCache),
            ("valid".to_owned(), Skip::ExcludedCache),
        ])
    );
    // Without --exclude-caches nothing is skipped.
    let (_, skipped, _) = walked(&root, |sink| {
        walk(&root, &WalkOptions::default(), sink).unwrap()
    });
    assert!(skipped.is_empty());
}

#[test]
fn p1_walk_09_an_excluded_entry_reports_no_error() {
    let mut tree = MemProvider::new("/t");
    tree.file("gone.tmp", 10)
        .unstatable("gone.tmp", libc::ENOENT)
        .file("locked.tmp", 10)
        .unstatable("locked.tmp", libc::EACCES)
        .file("locked.txt", 10)
        .unstatable("locked.txt", libc::EACCES)
        .file("kept.txt", 10);
    let opts = WalkOptions {
        excludes: globs(&["*.tmp"]),
        ..WalkOptions::default()
    };
    let (entries, _, summary) = mem_walk(&tree, &opts);
    assert_eq!(entries, ["", "kept.txt"]);
    // Only the entry the glob keeps reports that it could not be read; the
    // one that vanished is left out too.
    assert_eq!((summary.entry_errors, summary.vanished), (1, 0));
}

#[test]
fn p1_walk_09_an_excluded_link_is_not_followed() {
    let env = TestEnv::new();
    let root = env.fixtures().case_root("symlinks");
    let follow = |excludes| WalkOptions {
        follow_symlinks: true,
        excludes,
        ..WalkOptions::default()
    };
    let (_, _, all) = walked(&root, |sink| {
        walk(&root, &follow(GlobSet::empty()), sink).unwrap()
    });
    let (entries, _, kept) = walked(&root, |sink| {
        walk(&root, &follow(globs(&["loop-*", "self"])), sink).unwrap()
    });
    // The three links that loop are errors when followed; excluded, they
    // are left out unread.
    assert_eq!(all.entry_errors - kept.entry_errors, 3);
    assert!(
        !entries
            .iter()
            .any(|e| e.starts_with("loop-") || e == "self"),
        "{entries:?}"
    );
}

#[test]
fn p1_walk_09_a_fifo_named_cachedir_tag_does_not_stall_the_walk() {
    let env = TestEnv::new();
    let root = env.tmp_dir().join("fifo");
    std::fs::create_dir_all(root.join("cache")).unwrap();
    let made = env
        .command("mkfifo")
        .arg(root.join("cache/CACHEDIR.TAG"))
        .status()
        .unwrap();
    assert!(made.success());
    let opts = WalkOptions {
        exclude_caches: vec![CacheRule {
            name: Glob::new("never-matches").unwrap(),
            markers: vec![OsString::from("x")],
        }],
        ..WalkOptions::default()
    };
    // Opened for reading, a FIFO would block until a writer came.
    let (entries, skipped, _) = walked(&root, |sink| walk(&root, &opts, sink).unwrap());
    assert!(skipped.is_empty(), "{skipped:?}");
    assert!(entries.contains(&"cache/CACHEDIR.TAG".to_owned()));
}

#[test]
fn p1_walk_09_a_tagged_directory_in_memory_is_kept_out() {
    // The provider answers for CACHEDIR.TAG, so the rule is tested without
    // a file system: a tagged directory is listed but not entered, and only
    // when a cache rule turns the check on.
    let mut tree = MemProvider::new("/t");
    tree.file("build/out.o", 10)
        .file("src/main.rs", 10)
        .cachedir_tag("build");
    let opts = WalkOptions {
        exclude_caches: vec![CacheRule {
            name: Glob::new("never-matches").unwrap(),
            markers: vec![OsString::from("x")],
        }],
        ..WalkOptions::default()
    };
    let (entries, skipped, _) = mem_walk(&tree, &opts);
    assert_eq!(
        skipped,
        BTreeMap::from([("build".to_owned(), Skip::ExcludedCache)])
    );
    assert!(!entries.contains(&"build/out.o".to_owned()));
    let (entries, skipped, _) = mem_walk(&tree, &WalkOptions::default());
    assert!(skipped.is_empty());
    assert!(entries.contains(&"build/out.o".to_owned()));
}

#[test]
fn p1_walk_09_links_followed_in_memory() {
    // The provider resolves links too, so --follow-symlinks is tested
    // without a file system: a link to a directory is entered, one that
    // leads nowhere stays a link, and one to itself is an ELOOP error.
    let mut tree = MemProvider::new("/t");
    tree.file("real/x", 10)
        .link("to-real", "real")
        .link("nowhere", "missing")
        .link("self", "self");
    let opts = WalkOptions {
        follow_symlinks: true,
        ..WalkOptions::default()
    };
    let root = tree.root_path().to_owned();
    let mut errors = Vec::new();
    let (entries, _, _) = walked(&root, |sink| {
        walk_with(&tree, &root, &opts, |event| {
            if let ScanEvent::Error(error) = &event {
                errors.push((error.name.as_bytes().to_vec(), error.errno));
            }
            sink(event);
        })
        .unwrap()
    });
    assert!(entries.contains(&"to-real/x".to_owned()), "{entries:?}");
    assert!(entries.contains(&"nowhere".to_owned()));
    assert_eq!(errors, [(b"self".to_vec(), libc::ELOOP)]);
}
