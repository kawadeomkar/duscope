//! P2-IDX-03: indexes written by earlier versions open under this reader.
//!
//! `tests/fixtures/index/` holds one file per index minor version, written
//! by the release that introduced it, never regenerated afterwards: each
//! must still open and answer `top`. `p2_idx_03_write_the_v2_0_fixture`
//! wrote `v2.0.db` and is kept, ignored, as the record of what it holds.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::path::{Path, PathBuf};

use duscope_core::arena::Arena;
use duscope_core::index::{Index, IndexWriter, ScanId, ScanMeta, index_path};
use duscope_core::query::{self, TopQuery, TreeView};
use duscope_core::rollup::roll_up;
use duscope_testkit::TestEnv;
use duscope_walk::{Attrs, MemProvider, WalkOptions, walk_with};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/index")
}

/// The tree `v2.0.db` holds.
fn tree() -> MemProvider {
    let mut tree = MemProvider::new("/fixture");
    tree.file("photos/2034/beach.jpg", 3_000_000)
        .file("photos/2034/notes.txt", 900)
        .file("photos/cover.png", 400_000)
        .file("code/src/main.rs", 12_000)
        .file("code/target/debug/app", 5_000_000)
        .file("readme.md", 2_000)
        .symlink("latest", 13);
    tree
}

#[test]
#[ignore = "wrote tests/fixtures/index/v2.0.db once; run only to see how"]
fn p2_idx_03_write_the_v2_0_fixture() {
    let tree = tree();
    let mut arena = Arena::new(Attrs::Basic);
    walk_with(&tree, tree.root_path(), &WalkOptions::default(), |event| {
        arena.push(event).unwrap();
    })
    .unwrap();
    let rollup = roll_up(&mut arena);
    let id = ScanId::parse("f17ed0000200").unwrap();
    let meta = ScanMeta {
        scan_id: id.clone(),
        root: b"/fixture".to_vec(),
        root_as_given: b"/fixture".to_vec(),
        started_at: 2_000_000_000_000_000_000,
        finished_at: 2_000_000_001_000_000_000,
        options: r#"{"clones":true,"one_file_system":false,"include_network":false,"follow_symlinks":false,"excludes":[],"exclude_files":[],"ignore_files":[],"exclude_caches":false,"threads":0,"size_kind":"allocated"}"#.to_owned(),
        platform: "Linux 6.8.0".to_owned(),
        scanning_uid: 1000,
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
        tool_version: "0.2.0".to_owned(),
    };
    let env = TestEnv::new();
    let dir = env.tmp_dir();
    IndexWriter::write(&arena, &rollup, &meta, &dir).unwrap();
    std::fs::copy(index_path(&dir, &id), fixtures().join("v2.0.db")).unwrap();
}

#[test]
fn p2_idx_03_the_v2_0_fixture_opens_and_answers_top() {
    let _env = TestEnv::new();
    let index = Index::open(&fixtures().join("v2.0.db")).unwrap();
    assert_eq!(index.version(), "2.0");
    assert_eq!(index.meta().scan_id.as_str(), "f17ed0000200");
    assert_eq!(index.root_path(), Path::new("/fixture"));
    let page = query::top(&index, &TopQuery::default());
    let rows: Vec<(&str, u64)> = page
        .entries
        .iter()
        .map(|e| (e.path.as_str(), e.allocated_bytes))
        .collect();
    assert_eq!(
        rows,
        [("/fixture/code", 5_013_504), ("/fixture/photos", 3_407_872)]
    );
    let files = query::top(
        &index,
        &TopQuery {
            files: true,
            ..TopQuery::default()
        },
    );
    assert_eq!(files.entries[0].path, "/fixture/code/target/debug/app");
    // Six files and the link.
    assert_eq!(files.total_count, 7);
    assert!(index.failure().is_none());
    assert!(index.quick_check().unwrap().is_empty());
}
