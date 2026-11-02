//! P4-CORE-01: `clean`'s plan, from a saved scan of a tree on disk. These
//! tests plan only: nothing here moves anything to a Trash, since the
//! library would use the test process's own `HOME` (the binary's tests, run
//! in the test environment, move files).

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::{MetadataExt, symlink};
use std::path::{Path, PathBuf};

use duscope_testkit::TestEnv;
use duscope_walk::{Attrs, WalkOptions, walk};

use super::{CleanItem, clean_plan};
use crate::CoreError;
use crate::arena::Arena;
use crate::index::{Index, IndexWriter, ScanId, ScanMeta, index_path};
use crate::query::ExplainQuery;
use crate::rollup::roll_up;
use crate::rules::Caches;

fn meta(root: &Path) -> ScanMeta {
    ScanMeta {
        scan_id: ScanId::new(b"clean", 1, [0; 8]),
        root: root.as_os_str().as_encoded_bytes().to_vec(),
        root_as_given: root.as_os_str().as_encoded_bytes().to_vec(),
        started_at: 1,
        finished_at: 2,
        options: "{}".to_owned(),
        platform: "test".to_owned(),
        scanning_uid: 501,
        thread_count: 1,
        size_kind: "allocated",
        volume_total: 1 << 40,
        volume_free: 1 << 39,
        unaccounted_bytes: None,
        error_count: 0,
        vanished_count: 0,
        excluded_count: 0,
        clones_unavailable: true,
        incremental: false,
        parent_scan_id: None,
        source: "scan".to_owned(),
        tool_version: "0.0.0".to_owned(),
    }
}

/// A tree on disk: a file, a second link of it, a directory with a file,
/// and a symbolic link; and its saved scan.
fn saved(env: &TestEnv) -> (PathBuf, Index) {
    let root = fs::canonicalize(env.tmp_dir()).unwrap().join("tree");
    fs::create_dir_all(root.join("dir")).unwrap();
    fs::write(root.join("a.bin"), vec![1u8; 9000]).unwrap();
    fs::hard_link(root.join("a.bin"), root.join("dir/a-again.bin")).unwrap();
    fs::write(root.join("dir/b.bin"), vec![2u8; 5000]).unwrap();
    symlink("a.bin", root.join("link")).unwrap();
    let mut arena = Arena::new(Attrs::Basic);
    walk(&root, &WalkOptions::default(), |e| arena.push(e).unwrap()).unwrap();
    let rollup = roll_up(&mut arena);
    let meta = meta(&root);
    let dir = env.tmp_dir().join("index");
    fs::create_dir_all(&dir).unwrap();
    IndexWriter::write(&arena, &rollup, &meta, &dir).unwrap();
    let index = Index::open(&index_path(&dir, &meta.scan_id)).unwrap();
    (root, index)
}

fn item(path: PathBuf) -> CleanItem {
    CleanItem { path, handle: None }
}

fn no_env(_: &str) -> Option<OsString> {
    None
}

#[test]
fn p4_core_01_a_plan_takes_each_items_facts_from_the_saved_scan() {
    let env = TestEnv::new();
    let (root, index) = saved(&env);
    let caches = Caches::vendored();
    let q = ExplainQuery {
        caches: &caches,
        env: &no_env,
    };
    let plan = clean_plan(
        &[item(root.join("a.bin")), item(root.join("dir"))],
        Some(&index),
        &q,
    )
    .unwrap();
    let items = &plan.plan.items;
    assert_eq!(plan.trash_count(), 2);
    // The file's other link lies outside it: deleting it frees nothing. The
    // directory holds that link and its own file: deleting it frees the file
    // alone, the link's bytes kept by a.bin.
    assert_eq!(items[0].kind, "file");
    assert_eq!(items[0].nlink, Some(2));
    assert_eq!(items[0].links_outside_subtree, Some(1));
    assert_eq!(items[0].frees_bytes, Some(0));
    assert_eq!(items[0].frees_kind, Some("lower_bound"));
    assert_eq!(items[1].kind, "dir");
    assert_eq!(
        plan.plan.total_allocated_bytes,
        items[0].allocated_bytes + items[1].allocated_bytes
    );
    assert_eq!(plan.plan.total_frees_bytes, items[1].frees_bytes.unwrap());
    assert!(
        items
            .iter()
            .all(|i| i.outcome.is_none() && i.error.is_none())
    );
    assert!(plan.plan.confirmation_required);
}

#[test]
fn p4_core_01_refusals_in_a_saved_scan() {
    let env = TestEnv::new();
    let (root, index) = saved(&env);
    let outside = fs::canonicalize(env.tmp_dir()).unwrap().join("outside.txt");
    fs::write(&outside, b"x").unwrap();
    let caches = Caches::vendored();
    let q = ExplainQuery {
        caches: &caches,
        env: &no_env,
    };
    let plan = clean_plan(&[item(root.join("link")), item(outside)], Some(&index), &q).unwrap();
    let reasons: Vec<Option<&str>> = plan.plan.items.iter().map(|i| i.reason).collect();
    assert_eq!(reasons, [Some("symlink"), Some("outside_root")]);
    assert_eq!(plan.refused_count(), 2);
    assert_eq!(plan.plan.total_allocated_bytes, 0);
    // The scan's root, alone: it holds every other item.
    let plan = clean_plan(&[item(root.clone())], Some(&index), &q).unwrap();
    assert_eq!(plan.plan.items[0].reason, Some("root"));
}

#[test]
fn p4_core_01_overlap_a_missing_path_and_a_changed_handle_fail_the_plan() {
    let env = TestEnv::new();
    let (root, index) = saved(&env);
    let caches = Caches::vendored();
    let q = ExplainQuery {
        caches: &caches,
        env: &no_env,
    };
    let plan = |items: &[CleanItem]| clean_plan(items, Some(&index), &q);
    assert!(matches!(
        plan(&[item(root.join("dir")), item(root.join("dir/b.bin"))]),
        Err(CoreError::CleanOverlap(_))
    ));
    assert!(matches!(
        plan(&[item(root.join("a.bin")), item(root.join("a.bin"))]),
        Err(CoreError::CleanOverlap(_))
    ));
    assert!(matches!(
        plan(&[item(root.join("gone"))]),
        Err(CoreError::ScanNotFound(_))
    ));
    let meta = fs::symlink_metadata(root.join("a.bin")).unwrap();
    let signed = CleanItem {
        path: root.join("a.bin"),
        handle: Some((meta.dev(), meta.ino())),
    };
    assert!(plan(std::slice::from_ref(&signed)).is_ok());
    let stale = CleanItem {
        handle: Some((meta.dev(), meta.ino() + 1)),
        ..signed
    };
    assert!(matches!(plan(&[stale]), Err(CoreError::PathChanged(_))));
}
