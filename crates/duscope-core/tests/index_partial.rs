//! P2-QRY-02, P2-QRY-03: an index written before `dirs_errors`,
//! `dirs_excluded` and `files_dataless` answers as one with them: the
//! partial indexes speed the counts up and decide nothing.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fs;
use std::path::Path;

use duscope_core::arena::Arena;
use duscope_core::index::{Index, IndexWriter, ScanId, ScanMeta, index_path};
use duscope_core::query::TreeView;
use duscope_core::rollup::roll_up;
use duscope_testkit::TestEnv;
use duscope_walk::{Attrs, MemProvider, WalkOptions, walk_with};
use globset::{Glob, GlobSetBuilder};

fn meta(root: &Path) -> ScanMeta {
    ScanMeta {
        scan_id: ScanId::new(b"partial", 1, [0; 8]),
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
        // One unreadable directory, one excluded.
        error_count: 1,
        vanished_count: 0,
        excluded_count: 1,
        clones_unavailable: false,
        incremental: false,
        parent_scan_id: None,
        source: "scan".to_owned(),
        tool_version: "0.0.0".to_owned(),
    }
}

#[test]
fn p2_qry_02_an_index_without_the_partial_indexes_answers_alike() {
    let env = TestEnv::new();
    let mut tree = MemProvider::new("/r");
    tree.file("a/x.txt", 10)
        .file("locked/y.txt", 10)
        .file("skip/z.txt", 10)
        .file("b/c/w.txt", 10)
        .unreadable("locked", 13);
    let mut excludes = GlobSetBuilder::new();
    excludes.add(Glob::new("**/skip").unwrap());
    let opts = WalkOptions {
        excludes: excludes.build().unwrap(),
        ..WalkOptions::default()
    };
    let mut arena = Arena::new(Attrs::Basic);
    walk_with(&tree, tree.root_path(), &opts, |e| arena.push(e).unwrap()).unwrap();
    let rollup = roll_up(&mut arena);
    let meta = meta(tree.root_path());
    let dir = env.tmp_dir();
    IndexWriter::write(&arena, &rollup, &meta, &dir).unwrap();
    let file = index_path(&dir, &meta.scan_id);
    let with = Index::open(&file).unwrap();
    // The same file, as a build before the two indexes wrote it.
    let older = dir.join("older.db");
    fs::copy(&file, &older).unwrap();
    let db = rusqlite::Connection::open(&older).unwrap();
    db.execute_batch(
        "DROP INDEX dirs_errors; DROP INDEX dirs_excluded; DROP INDEX files_dataless;",
    )
    .unwrap();
    drop(db);
    let without = Index::open(&older).unwrap();
    let last = with.node(0).last_id;
    assert_eq!(with.unreadable_dirs(0, last), 1);
    assert_eq!(without.unreadable_dirs(0, last), 1);
    assert_eq!(with.excluded().len(), 1);
    assert_eq!(without.excluded(), with.excluded());
    assert_eq!(without.dataless_bytes(0), with.dataless_bytes(0));
    assert_eq!(without.errors_under(0, last), with.errors_under(0, last));
    for id in 0..with.len() {
        assert_eq!(without.child_dirs(id), with.child_dirs(id), "{id}");
    }
    assert!(with.failure().is_none() && without.failure().is_none());
}
