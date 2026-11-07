//! P5-WALK-03: a saved scan keeps why `--skip-containers` left a directory
//! out (flag bit 13 beside `excluded`), and counts its dataless files.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::path::{Path, PathBuf};

use duscope_core::arena::Arena;
use duscope_core::index::{
    EXCLUDED, Index, IndexWriter, SKIPPED_CONTAINERS, ScanId, ScanMeta, index_path,
};
use duscope_core::query::{ArenaView, Dataless, TreeView};
use duscope_core::rollup::roll_up;
use duscope_testkit::TestEnv;
use duscope_walk::{Attrs, MemProvider, WalkOptions, walk_with};
use globset::{Glob, GlobSetBuilder};

fn meta(root: &Path, excluded_count: u64) -> ScanMeta {
    ScanMeta {
        scan_id: ScanId::new(b"containers", 1, [0; 8]),
        root: root.as_os_str().as_encoded_bytes().to_vec(),
        root_as_given: root.as_os_str().as_encoded_bytes().to_vec(),
        started_at: 1,
        finished_at: 2,
        options: r#"{"skip_containers":true}"#.to_owned(),
        platform: "test".to_owned(),
        scanning_uid: 501,
        thread_count: 4,
        size_kind: "allocated",
        volume_total: 1 << 40,
        volume_free: 1 << 39,
        unaccounted_bytes: None,
        error_count: 0,
        vanished_count: 0,
        excluded_count,
        clones_unavailable: false,
        incremental: false,
        parent_scan_id: None,
        source: "scan".to_owned(),
        tool_version: "0.0.0".to_owned(),
    }
}

#[test]
fn p5_walk_03_a_saved_scan_keeps_the_skipped_containers() {
    let env = TestEnv::new();
    let mut tree = MemProvider::new("/u");
    tree.file("Library/Containers/com.app/db", 8192)
        .file("Library/Group Containers/group.app/x", 4096)
        .file("Library/Logs/l", 4096)
        .file("drop/y", 4096)
        .dataless("Documents/cloud.pages", 1 << 20)
        .file("Documents/local.txt", 100);
    let mut globs = GlobSetBuilder::new();
    globs.add(Glob::new("drop").unwrap());
    let opts = WalkOptions {
        excludes: globs.build().unwrap(),
        skip_containers: Some(PathBuf::from("/u")),
        ..WalkOptions::default()
    };
    let mut arena = Arena::new(Attrs::Basic);
    let summary = walk_with(&tree, tree.root_path(), &opts, |e| arena.push(e).unwrap()).unwrap();
    assert_eq!(summary.excluded, 3);
    let rollup = roll_up(&mut arena);
    // The fresh scan's tally of placeholders, from one pass over the arena.
    let fresh = ArenaView::new(&arena, &rollup, PathBuf::from("/u")).dataless(0);
    assert_eq!(
        fresh,
        Dataless {
            files: 1,
            apparent: 1 << 20,
            allocated: 0
        }
    );
    let meta = meta(tree.root_path(), summary.excluded);
    let dir = env.tmp_dir();
    IndexWriter::write(&arena, &rollup, &meta, &dir).unwrap();
    let index = Index::open(&index_path(&dir, &meta.scan_id)).unwrap();
    let excluded: Vec<(Vec<u8>, &str)> = index
        .excluded()
        .into_iter()
        .map(|(id, reason)| (index.path(id), reason))
        .collect();
    assert_eq!(
        excluded,
        [
            (b"Library/Containers".to_vec(), "skipped_containers"),
            (b"Library/Group Containers".to_vec(), "skipped_containers"),
            (b"drop".to_vec(), "excluded"),
        ]
    );
    // Bit 13 sits beside bit 10, so an older reader, and every query that
    // asks whether the scan entered a directory, reads it as excluded.
    let file = index_path(&dir, &meta.scan_id);
    let flags: u32 = rusqlite::Connection::open(&file)
        .unwrap()
        .query_row(
            "SELECT flags FROM dirs WHERE name = CAST('Containers' AS BLOB)",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        flags & (EXCLUDED | SKIPPED_CONTAINERS),
        EXCLUDED | SKIPPED_CONTAINERS
    );
    let (containers, _) = excluded_ids(&index);
    assert!(!index.entered(containers));
    // The saved scan's tally matches the fresh one's.
    assert_eq!(index.dataless(0), fresh);
    assert!(index.failure().is_none());
}

/// The id of the first excluded directory, and how many there are.
fn excluded_ids(index: &Index) -> (u32, usize) {
    let excluded = index.excluded();
    (excluded[0].0, excluded.len())
}
