//! P4-TUI-06: what a saved scan of `/` says of a mount it did not enter. A
//! mount point is stored as an absolute path, so the entry's own path has
//! to be spelt the same way under a root that already ends in `/`.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::path::Path;

use duscope_core::arena::Arena;
use duscope_core::index::{Index, IndexWriter, ScanId, ScanMeta, index_path};
use duscope_core::query::{ScanQuery, ScanSelector, resolve_scan};
use duscope_core::rollup::roll_up;
use duscope_testkit::TestEnv;
use duscope_walk::{Attrs, MemProvider, ScanEvent, Skip, WalkOptions, walk_with};

/// A saved scan of an in-memory tree rooted at `root`, with
/// `mnt/nas` a network mount the walk listed and did not enter. No real
/// filesystem is read.
fn saved(dir: &Path, root: &str) -> Index {
    let mut tree = MemProvider::new(root);
    tree.file("home/a/f", 4096).dir("mnt/nas");
    let mut arena = Arena::new(Attrs::Basic);
    let mut volume = None;
    let summary = walk_with(&tree, tree.root_path(), &WalkOptions::default(), |event| {
        if let ScanEvent::Volume(found) = &event {
            volume.get_or_insert_with(|| found.clone());
        }
        arena.push(event).unwrap();
    })
    .unwrap();
    let mut mount = volume.unwrap();
    mount.dev += 1;
    mount.mnt_id += 1;
    mount.mount_point = tree.root_path().join("mnt/nas");
    mount.reason = Some(Skip::Network);
    arena.push(ScanEvent::Volume(mount)).unwrap();
    let rollup = roll_up(&mut arena);
    let root = tree.root_path().as_os_str().as_encoded_bytes().to_vec();
    let meta = ScanMeta {
        scan_id: ScanId::new(&root, 1, [0; 8]),
        root: root.clone(),
        root_as_given: root,
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
        excluded_count: summary.excluded,
        clones_unavailable: false,
        incremental: false,
        parent_scan_id: None,
        source: "scan".to_owned(),
        tool_version: "0.0.0".to_owned(),
    };
    IndexWriter::write(&arena, &rollup, &meta, dir).unwrap();
    Index::open(&index_path(dir, &meta.scan_id)).unwrap()
}

/// The entry of `index` at the absolute `path`.
fn id(index: &Index, path: &str) -> u32 {
    index
        .lookup(index.below_root(Path::new(path)).unwrap())
        .unwrap()
}

#[test]
fn p4_tui_06_a_scan_of_slash_did_not_enter_the_mount_it_did_not_enter() {
    let env = TestEnv::new();
    let index = saved(&env.tmp_dir(), "/");
    assert!(!index.entered(id(&index, "/mnt/nas")));
    assert!(index.entered(id(&index, "/mnt")));
    assert!(index.entered(id(&index, "/home/a")));
    assert!(index.failure().is_none());
}

#[test]
fn p4_tui_06_any_other_root_did_not_enter_the_mount_it_did_not_enter() {
    let env = TestEnv::new();
    let index = saved(&env.tmp_dir(), "/tree");
    assert!(!index.entered(id(&index, "/tree/mnt/nas")));
    assert!(index.entered(id(&index, "/tree/mnt")));
    assert!(index.entered(id(&index, "/tree")));
}

#[test]
fn p4_tui_06_latest_leaves_a_mount_a_scan_of_slash_did_not_enter_to_a_scan_of_it() {
    let env = TestEnv::new();
    let dir = env.tmp_dir();
    saved(&dir, "/");
    let latest = |path: &str| {
        let q = ScanQuery {
            index_dir: &dir,
            path: Some(Path::new(path)),
            cwd: Path::new("/"),
            max_age_ns: i64::MAX,
            now: 3,
            trust: &[],
        };
        resolve_scan(&ScanSelector::Latest, &q, |_| true)
            .unwrap()
            .is_some()
    };
    assert!(latest("/home/a"));
    assert!(!latest("/mnt/nas"));
}
