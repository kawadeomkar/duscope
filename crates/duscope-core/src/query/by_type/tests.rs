//! P2-QRY-02: `by-type` from the index's own tables answers as the walk.

use std::path::Path;

use duscope_testkit::TestEnv;
use duscope_walk::{Attrs, MemProvider, WalkOptions, walk_with};

use super::{ByTypeQuery, from_index, tally};
use crate::arena::Arena;
use crate::index::{Index, IndexWriter, ScanId, ScanMeta, index_path};
use crate::query::{Filters, SizeKind, SortKey, Subtree, TreeView};
use crate::rollup::roll_up;
use crate::rules::Classes;

fn meta(root: &Path) -> ScanMeta {
    ScanMeta {
        scan_id: ScanId::new(b"by-type", 1, [0; 8]),
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

fn index_of(tree: &MemProvider, dir: &Path) -> Index {
    let mut arena = Arena::new(Attrs::Basic);
    walk_with(tree, tree.root_path(), &WalkOptions::default(), |e| {
        arena.push(e).unwrap();
    })
    .unwrap();
    let rollup = roll_up(&mut arena);
    let meta = meta(tree.root_path());
    IndexWriter::write(&arena, &rollup, &meta, dir).unwrap();
    Index::open(&index_path(dir, &meta.scan_id)).unwrap()
}

/// The walk's tallies without the newest file's time, which the tables do
/// not hold and only `--sort mtime`, which walks, reads.
fn walked(view: &Index, q: &ByTypeQuery<'_>) -> Vec<super::Tally> {
    let mut tallies = tally(view, q);
    for t in &mut tallies {
        t.newest_mtime_ns = 0;
    }
    tallies
}

fn query(classes: &Classes, examples: usize) -> ByTypeQuery<'_> {
    ByTypeQuery {
        classes,
        examples,
        filters: Filters::default(),
        limit: None,
        size: SizeKind::Allocated,
        sort: SortKey::Size,
        reverse: false,
    }
}

#[test]
fn p2_qry_02_by_type_from_the_tables_answers_as_the_walk() {
    let env = TestEnv::new();
    let mut tree = MemProvider::new("/r");
    tree.file("photos/a.jpg", 900_000)
        .file("photos/b.JPG", 800_000)
        .file("photos/c.png", 700_000)
        .file("src/main.rs", 40_000)
        .file("src/lib.rs", 30_000)
        .file("notes.md", 9_000)
        .file("README", 5_000)
        .file("archive.tar.gz", 600_000)
        // Files under node_modules are code, whatever their extension;
        // so are those under a nested one, part of the outer.
        .file("app/node_modules/x/logo.png", 500_000)
        .file("app/node_modules/x/data.json", 200_000)
        .file("app/node_modules/y/node_modules/z/blob.bin", 100_000)
        .file("repo/.git/objects/ab/cdef", 300_000)
        .link("photos/again.jpg", "photos/a.jpg");
    let index = index_of(&tree, &env.tmp_dir());
    let classes = Classes::vendored();
    for examples in [0, 1, 3] {
        let q = query(&classes, examples);
        let fast = from_index(&index, &q).expect("the tables answer");
        assert_eq!(fast, walked(&index, &q), "examples {examples}");
    }
    // A subtree, a filter, apparent sizes: the walk.
    let sub = Subtree::new(&index, index.child_dirs(0)[0].0);
    assert!(from_index(&sub, &query(&classes, 3)).is_none());
    let apparent = ByTypeQuery {
        size: SizeKind::Apparent,
        ..query(&classes, 3)
    };
    assert!(from_index(&index, &apparent).is_none());
    assert!(index.failure().is_none());
}

#[test]
fn p2_qry_02_a_rule_above_the_root_classes_everything() {
    let env = TestEnv::new();
    let mut tree = MemProvider::new("/w/node_modules/pkg");
    tree.file("a.png", 1_000).file("b.txt", 2_000);
    let index = index_of(&tree, &env.tmp_dir());
    let classes = Classes::vendored();
    let q = query(&classes, 2);
    let fast = from_index(&index, &q).expect("the tables answer");
    assert_eq!(fast, walked(&index, &q));
}
