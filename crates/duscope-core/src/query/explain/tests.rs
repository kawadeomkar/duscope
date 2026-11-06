//! P2-QRY-04: `explain`'s facts against the reference fixture tree.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use duscope_testkit::TestEnv;
use duscope_walk::{Attrs, MemProvider, WalkOptions, walk, walk_with};

use super::{ExplainQuery, explain, explain_unindexed};
use crate::arena::Arena;
use crate::git;
use crate::index::{Index, IndexWriter, ScanId, ScanMeta, index_path};
use crate::resolve::resolve_in_index;
use crate::rollup::roll_up;
use crate::rules::Caches;

fn meta(root: &Path) -> ScanMeta {
    ScanMeta {
        scan_id: ScanId::new(b"explain", 1, [0; 8]),
        root: root.as_os_str().as_encoded_bytes().to_vec(),
        root_as_given: root.as_os_str().as_encoded_bytes().to_vec(),
        started_at: 1,
        finished_at: 2,
        options: "{}".to_owned(),
        platform: "test".to_owned(),
        scanning_uid: 501,
        thread_count: 4,
        size_kind: "allocated",
        volume_total: 1 << 40,
        volume_free: 1 << 39,
        unaccounted_bytes: None,
        error_count: 0,
        vanished_count: 0,
        excluded_count: 0,
        clones_unavailable: false,
        incremental: false,
        parent_scan_id: None,
        source: "scan".to_owned(),
        tool_version: "0.0.0".to_owned(),
    }
}

fn index_of(root: &Path, events: impl FnOnce(&mut Arena), dir: &Path) -> Index {
    let mut arena = Arena::new(Attrs::Basic);
    events(&mut arena);
    let rollup = roll_up(&mut arena);
    let meta = meta(root);
    IndexWriter::write(&arena, &rollup, &meta, dir).unwrap();
    Index::open(&index_path(dir, &meta.scan_id)).unwrap()
}

fn no_env(_: &str) -> Option<OsString> {
    None
}

/// The reference fixture's root and an index of it.
fn fixture(env: &TestEnv) -> (PathBuf, Index) {
    let root = fs::canonicalize(env.fixtures().root()).unwrap();
    let index = index_of(
        &root,
        |arena| {
            walk(&root, &WalkOptions::default(), |e| arena.push(e).unwrap()).unwrap();
        },
        &env.tmp_dir(),
    );
    (root, index)
}

#[test]
fn p2_qry_04_links_inside_and_outside_the_path() {
    let env = TestEnv::new();
    let (root, index) = fixture(&env);
    let caches = Caches::vendored();
    let q = ExplainQuery {
        caches: &caches,
        env: &no_env,
    };
    let facts = |rel: &str| {
        let id = resolve_in_index(&index, &root.join(rel)).unwrap();
        explain(&index, id, &q).unwrap()
    };
    // One inode linked four times, from three directories.
    let original = facts("cases/hard-links/a/original.bin");
    assert_eq!(original.nlink, Some(4));
    assert_eq!(original.links_outside_subtree, Some(3));
    assert_eq!(original.freeable_now_bytes, Some(0));
    assert_eq!(original.freeable_kind, Some("lower_bound"));
    // a/ holds two of the four: deleting it frees its own blocks (none on
    // APFS, a block on ext4), not the file's.
    let a = facts("cases/hard-links/a");
    assert_eq!(a.links_outside_subtree, Some(2));
    assert_eq!(
        (a.freeable_now_bytes, a.freeable_kind),
        (
            Some(a.allocated_bytes - original.allocated_bytes),
            Some("lower_bound")
        )
    );
    // The case's root holds all four, and the pair's both: exact.
    let all = facts("cases/hard-links");
    assert_eq!(all.links_outside_subtree, Some(0));
    assert_eq!(all.freeable_now_bytes, Some(all.allocated_bytes));
    assert_eq!((all.freeable_kind, all.confidence), (Some("exact"), "high"));
    let pair = facts("cases/hard-links/pair");
    assert_eq!(
        (pair.links_outside_subtree, pair.freeable_kind),
        (Some(0), Some("exact"))
    );
    // A later link is uncharged: deleting it frees nothing.
    let later = facts("cases/hard-links/b/other-dir.bin");
    assert_eq!(later.freeable_now_bytes, Some(0));
    assert!(later.flags.contains(&"uncharged"));
    // Clones, where the filesystem has clone ids.
    if env.fixtures().built("clones") && cfg!(target_os = "macos") {
        let member = facts("cases/clones/clone-1.img");
        assert_eq!(member.is_clone, Some(true));
        assert_eq!(member.clone_group_size, Some(3));
        assert_eq!(member.clone_members_outside, Some(2));
        let group = facts("cases/clones");
        assert_eq!(group.clone_members_outside, Some(0));
        assert_eq!(group.freeable_kind, Some("exact"));
    }
    assert!(index.failure().is_none());
}

#[test]
fn p2_qry_04_cache_rules_need_their_marker() {
    let env = TestEnv::new();
    let (root, index) = fixture(&env);
    let caches = Caches::vendored();
    let q = ExplainQuery {
        caches: &caches,
        env: &no_env,
    };
    let facts = |rel: &str| {
        let id = resolve_in_index(&index, &root.join(rel)).unwrap();
        explain(&index, id, &q).unwrap()
    };
    let npm = facts("home/Projects/npm-app/node_modules");
    assert_eq!(npm.regenerable_by, Some("npm install"));
    let stray = facts("cases/no-marker/node_modules");
    assert_eq!(stray.regenerable_by, None);
    assert_eq!(stray.confidence, "medium");
    assert!(stray.evidence.iter().any(|e| e.contains("no marker")));
    let target = facts("home/Projects/rust-cli/target");
    assert_eq!(target.regenerable_by, Some("cargo build"));
    assert!(target.cachedir_tag);
}

#[test]
fn p2_qry_04_outside_a_repository_no_git_index_is_read() {
    let env = TestEnv::new();
    let mut tree = MemProvider::new("/explain-tree");
    tree.file("a/b.txt", 10);
    let index = index_of(
        Path::new("/explain-tree"),
        |arena| {
            walk_with(&tree, tree.root_path(), &WalkOptions::default(), |e| {
                arena.push(e).unwrap();
            })
            .unwrap();
        },
        &env.tmp_dir(),
    );
    let caches = Caches::vendored();
    let q = ExplainQuery {
        caches: &caches,
        env: &no_env,
    };
    let before = git::index_reads();
    let id = resolve_in_index(&index, Path::new("/explain-tree/a/b.txt")).unwrap();
    let facts = explain(&index, id, &q).unwrap();
    assert!(facts.git.is_none());
    assert_eq!(
        git::index_reads(),
        before,
        "an index file was read outside a repository"
    );
}

#[test]
fn p2_qry_04_a_path_no_saved_scan_holds_is_stat_with_low_confidence() {
    let env = TestEnv::new();
    let file = env.tmp_dir().join("loose.txt");
    fs::write(&file, b"loose").unwrap();
    let caches = Caches::vendored();
    let q = ExplainQuery {
        caches: &caches,
        env: &no_env,
    };
    let facts = explain_unindexed(&file, &q).unwrap();
    assert_eq!((facts.kind, facts.apparent_bytes), ("file", 5));
    assert_eq!(facts.confidence, "low");
    assert_eq!(facts.nlink, Some(1));
    assert_eq!(facts.freeable_kind, Some("upper_bound"));
    assert_eq!(facts.evidence, ["stat: no saved scan holds the path"]);
    assert!(explain_unindexed(&env.tmp_dir().join("absent"), &q).is_err());
}

/// `path` as components.
fn components(path: &str) -> Vec<&[u8]> {
    path.split('/').map(str::as_bytes).collect()
}

#[test]
fn p2_qry_04_the_nearest_marker_chooses_the_rule() {
    let caches = Caches::vendored();
    // repo/build.gradle and repo/native/CMakeLists.txt: native/build is
    // cmake's, though gradle's rule comes first.
    let path = components("repo/native/build");
    let held =
        |k: usize, marker: &str| matches!((k, marker), (1, "build.gradle") | (2, "CMakeLists.txt"));
    let (rule, at) = caches.taking(&path, held, false).unwrap();
    assert_eq!(rule.regenerable_by.as_str(), "cmake --build");
    assert_eq!(at, Some(2));
    // A marker rule wins over the CACHEDIR.TAG rule after it.
    let path = components("p/target");
    let (rule, at) = caches
        .taking(&path, |k, m| k == 1 && m == "Cargo.toml", true)
        .unwrap();
    assert_eq!((rule.regenerable_by.as_str(), at), ("cargo build", Some(1)));
    let (rule, at) = caches.taking(&path, |_, _| false, true).unwrap();
    assert_eq!((rule.name.as_deref(), at), (None, None));
}

#[test]
fn p2_qry_04_a_marker_beyond_a_directory_of_the_rules_name_confirms_that_one() {
    let caches = Caches::vendored();
    // w/Cargo.toml, w/target/fixtures/cases/target: the outer target is
    // the cache, the inner one part of it.
    let path = components("w/target/fixtures/cases/target");
    let held = |k: usize, marker: &str| k == 1 && marker == "Cargo.toml";
    assert!(caches.taking(&path, held, false).is_none());
    let outer = components("w/target");
    assert!(caches.taking(&outer, held, false).is_some());
    // Another rule's name does not stop the search.
    let path = components("w/target/web/node_modules");
    let held = |k: usize, marker: &str| k == 1 && marker == "package.json";
    let (rule, at) = caches.taking(&path, held, false).unwrap();
    assert_eq!((rule.regenerable_by.as_str(), at), ("npm install", Some(1)));
}

#[test]
fn p5_walk_03_explain_on_a_dataless_file_reports_freeable_now_bytes_0() {
    // A cloud placeholder as the macOS provider reads one (logical size,
    // nothing allocated, flagged dataless): the mocked attributes, since no
    // runner has an evicted iCloud file to read.
    let env = TestEnv::new();
    let mut tree = MemProvider::new("/cloud-tree");
    tree.dataless("Documents/report.pdf", 5 << 20)
        .file("Documents/notes.txt", 10_000)
        .file("Pictures/a.jpg", 20_000);
    let index = index_of(
        Path::new("/cloud-tree"),
        |arena| {
            walk_with(&tree, tree.root_path(), &WalkOptions::default(), |e| {
                arena.push(e).unwrap();
            })
            .unwrap();
        },
        &env.tmp_dir(),
    );
    let caches = Caches::vendored();
    let q = ExplainQuery {
        caches: &caches,
        env: &no_env,
    };
    let facts = |path: &str| {
        let id = resolve_in_index(&index, Path::new(path)).unwrap();
        explain(&index, id, &q).unwrap()
    };
    let placeholder = facts("/cloud-tree/Documents/report.pdf");
    assert_eq!(placeholder.apparent_bytes, 5 << 20);
    assert_eq!(placeholder.freeable_now_bytes, Some(0));
    assert_eq!(placeholder.freeable_kind, Some("exact"));
    assert!(placeholder.flags.contains(&"dataless"));
    assert!(
        placeholder
            .evidence
            .iter()
            .any(|e| e.starts_with("dataless:")),
        "{:?}",
        placeholder.evidence
    );
    // Its directory frees what is on the disk: the local file's blocks,
    // none of the placeholder's 5 MiB.
    let documents = facts("/cloud-tree/Documents");
    let notes = facts("/cloud-tree/Documents/notes.txt");
    assert_eq!(documents.freeable_now_bytes, notes.freeable_now_bytes);
    assert_eq!(notes.freeable_now_bytes, Some(12_288));
    // The tally a scan's warning and summary read.
    let dataless = crate::query::TreeView::dataless(&index, 0);
    assert_eq!((dataless.files, dataless.apparent), (1, 5 << 20));
    let pictures = facts("/cloud-tree/Pictures");
    assert!(!pictures.evidence.iter().any(|e| e.starts_with("dataless:")));
}

#[test]
fn p5_walk_02_clones_read_extents_where_the_scan_has_no_clone_ids() {
    use super::shared_extents;

    let env = TestEnv::new();
    let root = fs::canonicalize(env.tmp_dir()).unwrap().join("extents");
    fs::create_dir_all(root.join("dir")).unwrap();
    fs::write(root.join("dir/a.bin"), vec![1u8; 64 * 1024]).unwrap();
    fs::write(root.join("dir/b.bin"), vec![2u8; 16 * 1024]).unwrap();
    let mut arena = Arena::new(Attrs::Basic);
    walk(&root, &WalkOptions::default(), |e| arena.push(e).unwrap()).unwrap();
    let rollup = roll_up(&mut arena);
    // A Linux scan: no clone ids.
    let mut scan = meta(&root);
    scan.clones_unavailable = true;
    let dir = env.tmp_dir().join("index");
    IndexWriter::write(&arena, &rollup, &scan, &dir).unwrap();
    let index = Index::open(&index_path(&dir, &scan.scan_id)).unwrap();
    let caches = Caches::vendored();
    let q = ExplainQuery {
        caches: &caches,
        env: &no_env,
    };
    // What this system answers for a plain file: no map (macOS), a map
    // with nothing shared, or no map on this filesystem (tmpfs).
    let map = duscope_walk::extents(&root.join("dir/a.bin"));
    let facts = |rel: &str| {
        let path = root.join(rel);
        let id = resolve_in_index(&index, &path).unwrap();
        let mut facts = explain(&index, id, &q).unwrap();
        let before = facts.clone();
        shared_extents(Some(&index), id, &path, &mut facts).unwrap();
        (before, facts)
    };
    let (before, file) = facts("dir/a.bin");
    let (_, dir_facts) = facts("dir");
    let last = |f: &crate::envelope::Explain| f.evidence.last().cloned().unwrap_or_default();
    match map {
        Ok(None) => {
            assert_eq!(file.is_clone, None);
            assert!(last(&file).contains("reads no extent map"), "{file:?}");
        }
        Ok(Some(_)) => {
            assert_eq!(file.is_clone, Some(false), "nothing is shared");
            assert!(last(&file).starts_with("fiemap: 0 of 1 files"), "{file:?}");
            assert!(last(&dir_facts).starts_with("fiemap: 0 of 2 files"));
            // Nothing shared: what deleting frees stands.
            assert_eq!(
                (file.freeable_now_bytes, file.freeable_kind),
                (before.freeable_now_bytes, before.freeable_kind)
            );
            assert_eq!(dir_facts.freeable_kind, Some("exact"));
        }
        Err(_) => {
            assert!(last(&file).contains("extent map is not read"), "{file:?}");
            assert_eq!(file.freeable_kind, Some("upper_bound"));
        }
    }
    // The clone group's size and its members outside: the map never says.
    assert_eq!(
        (file.clone_group_size, file.clone_members_outside),
        (None, None)
    );

    // Where the scan recorded clone ids, their facts stand and nothing is
    // read.
    let mut apfs = meta(&root);
    apfs.clones_unavailable = false;
    apfs.scan_id = ScanId::new(b"explain-apfs", 1, [0; 8]);
    IndexWriter::write(&arena, &rollup, &apfs, &dir).unwrap();
    let index = Index::open(&index_path(&dir, &apfs.scan_id)).unwrap();
    let path = root.join("dir/a.bin");
    let id = resolve_in_index(&index, &path).unwrap();
    let mut facts = explain(&index, id, &q).unwrap();
    let before = facts.clone();
    shared_extents(Some(&index), id, &path, &mut facts).unwrap();
    assert_eq!(facts, before);
}

#[test]
fn p5_walk_02_clones_read_nothing_through_a_link_put_in_after_the_scan() {
    use super::shared_extents;

    let env = TestEnv::new();
    let tmp = fs::canonicalize(env.tmp_dir()).unwrap();
    let root = tmp.join("swapped");
    fs::create_dir_all(root.join("proj")).unwrap();
    fs::write(root.join("proj/a.bin"), vec![1u8; 16 * 1024]).unwrap();
    let mut arena = Arena::new(Attrs::Basic);
    walk(&root, &WalkOptions::default(), |e| arena.push(e).unwrap()).unwrap();
    let rollup = roll_up(&mut arena);
    let mut scan = meta(&root);
    scan.clones_unavailable = true;
    let dir = tmp.join("index");
    IndexWriter::write(&arena, &rollup, &scan, &dir).unwrap();
    let index = Index::open(&index_path(&dir, &scan.scan_id)).unwrap();
    // Since the scan, the owner of `proj` has put a link to another
    // directory in its place, holding a file of the same name.
    let elsewhere = tmp.join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    fs::write(elsewhere.join("a.bin"), vec![2u8; 16 * 1024]).unwrap();
    fs::rename(root.join("proj"), tmp.join("proj-was")).unwrap();
    std::os::unix::fs::symlink(&elsewhere, root.join("proj")).unwrap();
    let caches = Caches::vendored();
    let q = ExplainQuery {
        caches: &caches,
        env: &no_env,
    };
    let id = resolve_in_index(&index, &root).unwrap();
    let mut facts = explain(&index, id, &q).unwrap();
    shared_extents(Some(&index), id, &root, &mut facts).unwrap();
    let last = facts.evidence.last().cloned().unwrap_or_default();
    if matches!(duscope_walk::extents(&elsewhere.join("a.bin")), Ok(None)) {
        // macOS reads no extent map.
        assert!(last.contains("reads no extent map"), "{facts:?}");
    } else {
        // The file the scan saw is not reached: not read through the link,
        // so not checked, and what deleting frees is an upper bound.
        assert!(last.starts_with("fiemap: 0 of 0 files"), "{last}");
        assert!(last.ends_with("; 1 could not be read"), "{last}");
        assert_eq!(facts.freeable_kind, Some("upper_bound"));
    }
}
