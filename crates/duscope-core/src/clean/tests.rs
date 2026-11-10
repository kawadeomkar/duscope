//! P4-CORE-01: `clean`'s plan, from a saved scan of a tree on disk. These
//! tests plan only: nothing here moves anything to a Trash, since the
//! library would use the test process's own `HOME` (the binary's tests, run
//! in the test environment, move files).
//!
//! P5-CORE-03: `--permanent` deletes the test's own files, and the race it
//! guards against is made to happen between [`clean_plan`] and
//! [`clean_execute`]: a directory swapped for a symbolic link to a directory
//! outside the scan's root.
//!
//! P5-CORE-04: an item that changed since the scan (a directory grown, a
//! file rewritten in place) is priced from the tree, and one from a handle
//! fails the plan.

use std::ffi::OsString;
use std::fs;
use std::io::Write as _;
use std::os::unix::fs::{MetadataExt, symlink};
use std::path::{Path, PathBuf};
use std::time::Duration;

use duscope_testkit::TestEnv;
use duscope_walk::{Attrs, RemoveError, WalkOptions, walk};

use super::{CleanItem, Method, clean_execute, clean_plan, removal_error, within_one_percent};
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

/// Bytes on disk of `path` itself, as a scan counts them.
fn blocks(path: &Path) -> u64 {
    fs::symlink_metadata(path).unwrap().blocks() * 512
}

fn query(caches: &Caches) -> ExplainQuery<'_> {
    ExplainQuery {
        caches,
        env: &no_env,
    }
}

#[test]
fn p4_core_01_a_plan_takes_each_items_facts_from_the_saved_scan() {
    let env = TestEnv::new();
    let (root, index) = saved(&env);
    let caches = Caches::vendored();
    let q = query(&caches);
    // The file alone: its other link lies outside the plan, so deleting it
    // frees nothing.
    let plan = clean_plan(&[item(root.join("a.bin"))], &index, &q, Method::Trash).unwrap();
    let file = &plan.plan.items[0];
    assert_eq!(file.kind, "file");
    assert_eq!(file.nlink, Some(2));
    assert_eq!(file.links_outside_subtree, Some(1));
    assert_eq!(file.frees_bytes, Some(0));
    assert_eq!(file.frees_kind, Some("lower_bound"));
    // The directory alone holds that other link and its own file: it frees
    // the file and itself, the link's bytes kept by a.bin.
    let plan = clean_plan(&[item(root.join("dir"))], &index, &q, Method::Trash).unwrap();
    let dir = &plan.plan.items[0];
    assert_eq!(dir.kind, "dir");
    assert_eq!(
        dir.frees_bytes,
        Some(blocks(&root.join("dir")) + blocks(&root.join("dir/b.bin")))
    );
    assert_eq!(dir.frees_kind, Some("lower_bound"));
    let plan = clean_plan(
        &[item(root.join("a.bin")), item(root.join("dir"))],
        &index,
        &q,
        Method::Trash,
    )
    .unwrap();
    let items = &plan.plan.items;
    assert_eq!(plan.acting_count(), 2);
    assert_eq!(
        plan.plan.total_allocated_bytes,
        items[0].allocated_bytes + items[1].allocated_bytes
    );
    assert!(
        items
            .iter()
            .all(|i| i.outcome.is_none() && i.error.is_none())
    );
    assert!(plan.plan.confirmation_required);
}

#[test]
fn p4_core_01_two_links_both_in_the_plan_free_their_bytes_once() {
    let env = TestEnv::new();
    let (root, index) = saved(&env);
    let caches = Caches::vendored();
    let q = query(&caches);
    let plan = clean_plan(
        &[item(root.join("a.bin")), item(root.join("dir/a-again.bin"))],
        &index,
        &q,
        Method::Trash,
    )
    .unwrap();
    let items = &plan.plan.items;
    // Whichever link the scan charged frees the file; the other frees 0.
    assert_eq!(plan.plan.total_frees_bytes, blocks(&root.join("a.bin")));
    assert!(
        items.iter().all(|i| i.frees_kind == Some("exact")),
        "{items:?}"
    );
    // The file and the directory that holds its other link: every byte of
    // the three.
    let plan = clean_plan(
        &[item(root.join("a.bin")), item(root.join("dir"))],
        &index,
        &q,
        Method::Trash,
    )
    .unwrap();
    assert_eq!(
        plan.plan.total_frees_bytes,
        blocks(&root.join("a.bin")) + blocks(&root.join("dir")) + blocks(&root.join("dir/b.bin"))
    );
}

#[test]
fn p4_core_01_an_item_made_since_the_scan_is_planned_from_disk() {
    let env = TestEnv::new();
    let (root, index) = saved(&env);
    fs::write(root.join("new.bin"), vec![3u8; 7000]).unwrap();
    fs::create_dir(root.join("new")).unwrap();
    fs::write(root.join("new/c.bin"), vec![4u8; 6000]).unwrap();
    let caches = Caches::vendored();
    let q = query(&caches);
    let plan = clean_plan(
        &[item(root.join("new.bin")), item(root.join("new"))],
        &index,
        &q,
        Method::Trash,
    )
    .unwrap();
    let items = &plan.plan.items;
    assert_eq!(plan.acting_count(), 2);
    assert_eq!(items[0].allocated_bytes, blocks(&root.join("new.bin")));
    assert_eq!(items[0].frees_kind, Some("upper_bound"));
    // The directory with what is under it, which it frees at most
    // (P5-CORE-04: a directory priced from the tree counts in the totals).
    assert_eq!(
        items[1].allocated_bytes,
        blocks(&root.join("new")) + blocks(&root.join("new/c.bin"))
    );
    assert_eq!(items[1].frees_bytes, Some(items[1].allocated_bytes));
    assert_eq!(items[1].frees_kind, Some("upper_bound"));
}

#[test]
fn p4_core_01_refusals_in_a_saved_scan() {
    let env = TestEnv::new();
    let (root, index) = saved(&env);
    let outside = fs::canonicalize(env.tmp_dir()).unwrap().join("outside.txt");
    fs::write(&outside, b"x").unwrap();
    let caches = Caches::vendored();
    let q = query(&caches);
    let plan = clean_plan(
        &[item(root.join("link")), item(outside)],
        &index,
        &q,
        Method::Trash,
    )
    .unwrap();
    let reasons: Vec<Option<&str>> = plan.plan.items.iter().map(|i| i.reason).collect();
    assert_eq!(reasons, [Some("symlink"), Some("outside_root")]);
    assert_eq!(plan.refused_count(), 2);
    assert_eq!(plan.plan.total_allocated_bytes, 0);
    // The scan's root, alone: it holds every other item.
    let plan = clean_plan(&[item(root.clone())], &index, &q, Method::Trash).unwrap();
    assert_eq!(plan.plan.items[0].reason, Some("root"));
}

#[test]
fn p4_core_01_overlap_a_missing_path_and_a_changed_handle_fail_the_plan() {
    let env = TestEnv::new();
    let (root, index) = saved(&env);
    let caches = Caches::vendored();
    let q = query(&caches);
    let plan = |items: &[CleanItem]| clean_plan(items, &index, &q, Method::Trash);
    assert!(matches!(
        plan(&[item(root.join("dir")), item(root.join("dir/b.bin"))]),
        Err(CoreError::CleanOverlap(_))
    ));
    assert!(matches!(
        plan(&[item(root.join("a.bin")), item(root.join("a.bin"))]),
        Err(CoreError::CleanOverlap(_))
    ));
    // Two links of one file are two items.
    assert!(plan(&[item(root.join("a.bin")), item(root.join("dir/a-again.bin"))]).is_ok());
    // A volume that compares names without case gives the directory a
    // second path: still one item.
    if root.join("DIR").exists() {
        assert!(matches!(
            plan(&[item(root.join("dir")), item(root.join("DIR"))]),
            Err(CoreError::CleanOverlap(_))
        ));
        assert!(matches!(
            plan(&[item(root.join("dir")), item(root.join("DIR/b.bin"))]),
            Err(CoreError::CleanOverlap(_))
        ));
    }
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

#[test]
fn p5_core_03_expect_bytes_holds_within_one_percent_either_way() {
    // The bounds themselves pass; a byte past either fails.
    for (bytes, expected, within) in [
        (10_000, 10_000, true),
        (9_900, 10_000, true),
        (9_899, 10_000, false),
        (10_100, 10_000, true),
        (10_101, 10_000, false),
        (0, 0, true),
        (1, 0, false),
        // 1% of 99 is under a byte: only 99 itself.
        (98, 99, false),
        (99, 99, true),
        (u64::MAX, u64::MAX, true),
        (u64::MAX - u64::MAX / 100, u64::MAX, true),
        (u64::MAX - u64::MAX / 100 - 1, u64::MAX, false),
    ] {
        assert_eq!(
            within_one_percent(bytes, expected),
            within,
            "{bytes} against {expected}"
        );
    }
    // Against a plan's own total: the smallest and largest --expect-bytes
    // it meets.
    let env = TestEnv::new();
    let (root, index) = saved(&env);
    let caches = Caches::vendored();
    let q = query(&caches);
    let plan = clean_plan(&[item(root.join("dir"))], &index, &q, Method::Permanent).unwrap();
    let total = plan.plan.total_frees_bytes;
    assert!(total > 0);
    let lowest = (100 * total).div_ceil(101);
    let highest = 100 * total / 99;
    assert!(plan.meets(total) && plan.meets(lowest) && plan.meets(highest));
    assert!(!plan.meets(lowest - 1) && !plan.meets(highest + 1));
}

#[test]
fn p5_core_03_a_permanent_plan_deletes_and_counts_what_it_freed() {
    let env = TestEnv::new();
    let (root, index) = saved(&env);
    let caches = Caches::vendored();
    let q = query(&caches);
    // The file, and the directory that holds its other link: the inode is
    // freed once, by whichever link goes last.
    let freed =
        blocks(&root.join("a.bin")) + blocks(&root.join("dir")) + blocks(&root.join("dir/b.bin"));
    let mut plan = clean_plan(
        &[item(root.join("a.bin")), item(root.join("dir"))],
        &index,
        &q,
        Method::Permanent,
    )
    .unwrap();
    assert_eq!(plan.plan.method, "permanent");
    assert!(plan.plan.items.iter().all(|i| i.action == "permanent"));
    assert_eq!(plan.acting_count(), 2);
    assert_eq!(plan.plan.total_frees_bytes, freed);
    let outcome = clean_execute(&mut plan);
    assert_eq!(outcome.deleted_count, 2);
    assert_eq!(outcome.failed_count, 0);
    assert_eq!((outcome.trashed_count, outcome.trashed_bytes), (0, 0));
    assert_eq!(outcome.freed_bytes, freed);
    assert!(
        plan.plan
            .items
            .iter()
            .all(|i| i.outcome == Some("deleted") && i.error.is_none())
    );
    assert!(fs::symlink_metadata(root.join("a.bin")).is_err());
    assert!(fs::symlink_metadata(root.join("dir")).is_err());
    // The symbolic link to a.bin is another entry, left dangling.
    assert!(fs::symlink_metadata(root.join("link")).is_ok());
}

/// Replaces `path` with a symbolic link to `to`, keeping what was there at
/// `away`.
fn swap_for_link(path: &Path, away: &Path, to: &Path) {
    fs::rename(path, away).unwrap();
    symlink(to, path).unwrap();
}

#[test]
fn p5_core_03_a_directory_swapped_for_a_symlink_after_the_plan_removes_nothing_outside() {
    let env = TestEnv::new();
    let (root, index) = saved(&env);
    let outside = fs::canonicalize(env.tmp_dir()).unwrap().join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("precious.bin"), vec![8u8; 6000]).unwrap();
    // A second link of dir/b.bin outside: the same device and inode, so only
    // the walk that never follows a link keeps it.
    fs::hard_link(root.join("dir/b.bin"), outside.join("b.bin")).unwrap();
    let caches = Caches::vendored();
    let q = query(&caches);

    // The directory itself, planned, then swapped for a link to outside.
    let mut plan = clean_plan(&[item(root.join("dir"))], &index, &q, Method::Permanent).unwrap();
    swap_for_link(&root.join("dir"), &root.join("dir.away"), &outside);
    let outcome = clean_execute(&mut plan);
    assert_eq!(
        (
            outcome.deleted_count,
            outcome.failed_count,
            outcome.freed_bytes
        ),
        (0, 1, 0)
    );
    let failed = &plan.plan.items[0];
    assert_eq!(failed.outcome, Some("failed"));
    assert_eq!(failed.error.as_ref().unwrap().code, "path_changed");
    assert!(
        fs::symlink_metadata(root.join("dir"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read(outside.join("precious.bin")).unwrap(), [8u8; 6000]);
    assert_eq!(fs::read(outside.join("b.bin")).unwrap(), [2u8; 5000]);
    assert_eq!(fs::read(root.join("dir.away/b.bin")).unwrap(), [2u8; 5000]);
    fs::remove_file(root.join("dir")).unwrap();
    fs::rename(root.join("dir.away"), root.join("dir")).unwrap();

    // A file under it, planned, then its directory swapped for the link: the
    // file outside is the same inode, and stays.
    let mut plan = clean_plan(
        &[item(root.join("dir/b.bin"))],
        &index,
        &q,
        Method::Permanent,
    )
    .unwrap();
    swap_for_link(&root.join("dir"), &root.join("dir.away"), &outside);
    let outcome = clean_execute(&mut plan);
    assert_eq!((outcome.deleted_count, outcome.failed_count), (0, 1));
    let error = plan.plan.items[0].error.as_ref().unwrap();
    assert_eq!(error.code, "path_changed");
    assert!(error.message.contains("on the way to"), "{}", error.message);
    assert_eq!(fs::read(outside.join("b.bin")).unwrap(), [2u8; 5000]);
    assert_eq!(fs::read(root.join("dir.away/b.bin")).unwrap(), [2u8; 5000]);
    assert_eq!(fs::metadata(outside.join("b.bin")).unwrap().nlink(), 2);
    assert_eq!(fs::read(outside.join("precious.bin")).unwrap(), [8u8; 6000]);
}

#[test]
fn p5_core_03_a_directory_whose_mount_cannot_be_told_is_refused() {
    let item = Path::new("/home/u/chroot");
    let unknown = |path: &Path| RemoveError::MountUnknown {
        path: path.to_owned(),
        source: std::io::ErrorKind::NotFound.into(),
    };
    // The item itself: nothing in it was deleted.
    let error = removal_error(item, &unknown(item));
    assert_eq!(
        (error.code, error.retryable, error.fixable_by),
        ("refused_path", false, "user")
    );
    assert!(
        error
            .message
            .starts_with("cannot tell whether /home/u/chroot is a mount point")
            && error.message.ends_with("so nothing in it was deleted"),
        "{}",
        error.message
    );
    assert_eq!(
        error.hint.as_deref(),
        Some("Rerun duscope clean without --permanent to move it to the Trash instead")
    );
    // A directory under it: it stays with the directories above it.
    let error = removal_error(item, &unknown(&item.join("home/data")));
    assert_eq!(error.code, "refused_path");
    assert!(
        error
            .message
            .contains("/home/u/chroot/home/data is a mount point")
            && error
                .message
                .ends_with("so it and the directories above it in /home/u/chroot were left"),
        "{}",
        error.message
    );
}

#[test]
fn p5_mcp_01_an_item_that_frees_other_than_its_figure_is_refused() {
    let env = TestEnv::new();
    let (root, index) = saved(&env);
    let caches = Caches::vendored();
    let q = query(&caches);
    let plan_of = || {
        clean_plan(
            &[item(root.join("dir/b.bin")), item(root.join("link"))],
            &index,
            &q,
            Method::Trash,
        )
        .unwrap()
    };
    let mut plan = plan_of();
    let frees = plan.plan.items[0].frees_bytes.unwrap();
    assert!(frees > 0);
    let (allocated, total) = (plan.plan.total_allocated_bytes, plan.plan.total_frees_bytes);
    // Within 1%, the item stays, and one refused already keeps its reason.
    plan.refuse_unexpected(&[frees, 1234]);
    assert_eq!(plan.plan.items[0].action, "trash");
    assert_eq!(plan.plan.items[1].reason, Some("symlink"));
    assert_eq!(plan.plan.total_frees_bytes, total);
    assert_eq!(plan.plan.total_allocated_bytes, allocated);
    // Off by more than 1%, it is refused, its figures kept for the reader,
    // and the plan counts what is left.
    let mut plan = plan_of();
    plan.refuse_unexpected(&[frees * 2, 1234]);
    assert_eq!(plan.plan.items[0].action, "refused");
    assert_eq!(plan.plan.items[0].reason, Some(super::UNEXPECTED));
    assert_eq!(plan.plan.items[0].frees_bytes, Some(frees));
    assert_eq!(plan.plan.items[1].reason, Some("symlink"));
    assert_eq!(plan.refused_count(), 2);
    assert_eq!(
        (plan.plan.total_allocated_bytes, plan.plan.total_frees_bytes),
        (0, 0)
    );
    // A size no saved scan knows meets no figure, 0 included: a directory
    // made since the scan is refused whatever it holds.
    let fresh = root.join("made-since");
    fs::create_dir(&fresh).unwrap();
    fs::write(fresh.join("big.bin"), vec![7u8; 64 * 1024]).unwrap();
    let walked = blocks(&fresh) + blocks(&fresh.join("big.bin"));
    for expected in [0, 1, 64 * 1024, walked] {
        let mut plan = clean_plan(&[item(fresh.clone())], &index, &q, Method::Trash).unwrap();
        // P5-CORE-04: priced from the tree, its walk's figure, at most.
        assert_eq!(plan.plan.items[0].frees_bytes, Some(walked));
        assert_eq!(plan.plan.items[0].frees_kind, Some("upper_bound"));
        assert_eq!(plan.plan.items[0].action, "trash");
        plan.refuse_unexpected(&[expected]);
        assert_eq!(plan.plan.items[0].action, "refused", "{expected}");
        assert_eq!(plan.plan.items[0].reason, Some(super::UNEXPECTED));
        assert_eq!(plan.acting_count(), 0);
    }
}

/// Scans the tree at `root` into a saved scan in `env`'s scratch directory.
fn scan_of(env: &TestEnv, root: &Path) -> Index {
    let mut arena = Arena::new(Attrs::Basic);
    walk(root, &WalkOptions::default(), |e| arena.push(e).unwrap()).unwrap();
    let rollup = roll_up(&mut arena);
    let meta = meta(root);
    let dir = env.tmp_dir().join("index");
    fs::create_dir_all(&dir).unwrap();
    IndexWriter::write(&arena, &rollup, &meta, &dir).unwrap();
    Index::open(&index_path(&dir, &meta.scan_id)).unwrap()
}

/// Appends `n` bytes to the file at `path`: the same inode, grown.
fn grow(path: &Path, n: usize) {
    let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
    file.write_all(&vec![6u8; n]).unwrap();
}

/// Rewrites the file at `path` in place with bytes of its own length, and
/// moves its modification time `ahead`: only that time tells.
fn rewrite(path: &Path, ahead: Duration) {
    let len = fs::metadata(path).unwrap().len();
    let was = fs::metadata(path).unwrap().modified().unwrap();
    let mut file = fs::OpenOptions::new().write(true).open(path).unwrap();
    file.write_all(&vec![4u8; usize::try_from(len).unwrap()])
        .unwrap();
    file.set_modified(was + ahead).unwrap();
}

/// A path's item with the handle of what is there now.
fn signed(path: &Path) -> CleanItem {
    let meta = fs::symlink_metadata(path).unwrap();
    CleanItem {
        path: path.to_owned(),
        handle: Some((meta.dev(), meta.ino())),
    }
}

#[test]
fn p5_core_04_a_directory_grown_since_the_scan_is_priced_live() {
    let env = TestEnv::new();
    let (root, index) = saved(&env);
    let caches = Caches::vendored();
    let q = query(&caches);
    let dir = root.join("dir");
    // As the scan saw it: its figures, a.bin keeping the link's bytes.
    let plan = clean_plan(&[item(dir.clone())], &index, &q, Method::Trash).unwrap();
    assert_eq!(plan.changed_count(), 0);
    let scanned = plan.plan.items[0].clone();
    assert_eq!(scanned.frees_kind, Some("lower_bound"));
    assert_eq!(
        scanned.frees_bytes,
        Some(blocks(&dir) + blocks(&dir.join("b.bin")))
    );

    // A file made in it since: the walk's bytes, every inode under it once,
    // which it frees at most; the plan counts them.
    fs::write(dir.join("grown.bin"), vec![5u8; 50 * 1024]).unwrap();
    let live = blocks(&dir)
        + blocks(&dir.join("a-again.bin"))
        + blocks(&dir.join("b.bin"))
        + blocks(&dir.join("grown.bin"));
    let plan = clean_plan(&[item(dir.clone())], &index, &q, Method::Trash).unwrap();
    assert_eq!(plan.changed_count(), 1);
    let item0 = &plan.plan.items[0];
    assert_eq!(
        (item0.allocated_bytes, item0.frees_bytes, item0.frees_kind),
        (live, Some(live), Some("upper_bound"))
    );
    assert_eq!((item0.action, item0.reason), ("trash", None));
    assert_eq!(
        (plan.plan.total_allocated_bytes, plan.plan.total_frees_bytes),
        (live, live)
    );
    assert!(live >= scanned.allocated_bytes + 50 * 1024, "{live}");

    // Deeper down, where neither the directory's own time nor its entries
    // move: a file under a folder under it, rewritten in place at its own
    // length, changes the newest time under it.
    let top = fs::canonicalize(env.tmp_dir()).unwrap().join("deep");
    fs::create_dir_all(top.join("proj/sub")).unwrap();
    fs::write(top.join("proj/sub/deep.bin"), vec![3u8; 9000]).unwrap();
    let deep_index = scan_of(&env, &top);
    let proj = top.join("proj");
    let plan = clean_plan(&[item(proj.clone())], &deep_index, &q, Method::Trash).unwrap();
    assert_eq!(plan.changed_count(), 0);
    assert_eq!(plan.plan.items[0].frees_kind, Some("exact"));
    rewrite(&top.join("proj/sub/deep.bin"), Duration::from_secs(60));
    let plan = clean_plan(&[item(proj.clone())], &deep_index, &q, Method::Trash).unwrap();
    assert_eq!(plan.changed_count(), 1);
    assert_eq!(plan.plan.items[0].frees_kind, Some("upper_bound"));
    assert_eq!(
        plan.plan.items[0].frees_bytes,
        Some(blocks(&proj) + blocks(&proj.join("sub")) + blocks(&proj.join("sub/deep.bin")))
    );
}

#[test]
fn p5_core_04_a_file_rewritten_in_place_is_priced_live() {
    let env = TestEnv::new();
    let (root, index) = saved(&env);
    let caches = Caches::vendored();
    let q = query(&caches);
    let file = root.join("dir/b.bin");
    let ino = fs::metadata(&file).unwrap().ino();
    let plan_of = || clean_plan(&[item(file.clone())], &index, &q, Method::Trash).unwrap();
    let plan = plan_of();
    assert_eq!(plan.changed_count(), 0);
    assert_eq!(plan.plan.items[0].frees_kind, Some("exact"));

    // The same bytes' length, written again: its modification time alone
    // differs from its row.
    rewrite(&file, Duration::from_secs(60));
    let plan = plan_of();
    assert_eq!(plan.changed_count(), 1);
    let item0 = &plan.plan.items[0];
    assert_eq!(
        (item0.allocated_bytes, item0.frees_bytes, item0.frees_kind),
        (blocks(&file), Some(blocks(&file)), Some("upper_bound"))
    );

    // Grown in place, the same inode: its size now, from the plan's stat.
    grow(&file, 60 * 1024);
    assert_eq!(fs::metadata(&file).unwrap().ino(), ino);
    let plan = plan_of();
    assert_eq!(plan.changed_count(), 1);
    let item0 = &plan.plan.items[0];
    assert!(blocks(&file) >= 65 * 1024, "{}", blocks(&file));
    assert_eq!(
        (item0.allocated_bytes, item0.frees_bytes, item0.frees_kind),
        (blocks(&file), Some(blocks(&file)), Some("upper_bound"))
    );
    assert_eq!(plan.plan.total_frees_bytes, blocks(&file));
    assert_eq!((item0.action, item0.reason), ("trash", None));
}

#[test]
fn p5_core_04_a_drifted_handle_item_is_path_changed() {
    let env = TestEnv::new();
    let (root, index) = saved(&env);
    let caches = Caches::vendored();
    let q = query(&caches);
    let (dir, file) = (root.join("dir"), root.join("dir/b.bin"));
    // As the scan saw them, their handles plan.
    let plan = clean_plan(&[signed(&dir)], &index, &q, Method::Trash).unwrap();
    assert_eq!(plan.changed_count(), 0);
    assert!(clean_plan(&[signed(&file)], &index, &q, Method::Trash).is_ok());

    // The file grown in place: the same device and inode, so the handle
    // still names it, but not what the scan saw, nor is its directory.
    grow(&file, 40 * 1024);
    for path in [&file, &dir] {
        match clean_plan(&[signed(path)], &index, &q, Method::Trash) {
            Err(CoreError::PathChanged(message)) => assert!(
                message.contains("has changed since the saved scan"),
                "{message}"
            ),
            other => panic!("{}: {other:?}", path.display()),
        }
    }
    // Beside an unchanged item, the whole plan fails, as clean acts on all
    // of a list or none.
    assert!(matches!(
        clean_plan(
            &[signed(&root.join("a.bin")), signed(&dir)],
            &index,
            &q,
            Method::Trash
        ),
        Err(CoreError::PathChanged(_))
    ));
    // By path, the same items are priced from the tree.
    let plan = clean_plan(
        &[item(file), item(root.join("a.bin"))],
        &index,
        &q,
        Method::Trash,
    )
    .unwrap();
    assert_eq!(plan.changed_count(), 1);
}

#[test]
fn p5_core_04_a_directory_the_scan_did_not_enter_is_measured_and_has_not_changed() {
    let env = TestEnv::new();
    let top = fs::canonicalize(env.tmp_dir()).unwrap().join("proj");
    fs::create_dir_all(top.join("app/cache/deeper")).unwrap();
    fs::write(top.join("app/main.rs"), vec![1u8; 3000]).unwrap();
    fs::write(top.join("app/cache/blob.bin"), vec![2u8; 40 * 1024]).unwrap();
    fs::write(top.join("app/cache/deeper/more.bin"), vec![2u8; 20 * 1024]).unwrap();
    // Scanned with app/cache listed and not entered.
    let mut arena = Arena::new(Attrs::Basic);
    let opts = WalkOptions {
        excludes: globset::GlobSetBuilder::new()
            .add(globset::Glob::new("app/cache").unwrap())
            .build()
            .unwrap(),
        ..WalkOptions::default()
    };
    walk(&top, &opts, |e| arena.push(e).unwrap()).unwrap();
    let rollup = roll_up(&mut arena);
    let meta = meta(&top);
    let dir = env.tmp_dir().join("index");
    fs::create_dir_all(&dir).unwrap();
    IndexWriter::write(&arena, &rollup, &meta, &dir).unwrap();
    let index = Index::open(&index_path(&dir, &meta.scan_id)).unwrap();
    let caches = Caches::vendored();
    let q = query(&caches);
    let app = top.join("app");
    let cache = app.join("cache");

    // The folder that holds it: what the scan read has not changed, so its
    // figures are the scan's, and it plans from a handle.
    let plan = clean_plan(&[signed(&app)], &index, &q, Method::Trash).unwrap();
    assert_eq!(plan.changed_count(), 0);
    assert_ne!(plan.plan.items[0].frees_kind, Some("upper_bound"));
    assert!(!plan.unmeasured(0));

    // The folder itself: the scan holds nothing under it, so a walk prices
    // it, at most; it has not changed, and its handle plans.
    let plan = clean_plan(&[signed(&cache)], &index, &q, Method::Trash).unwrap();
    assert_eq!(plan.changed_count(), 0);
    let live = blocks(&cache)
        + blocks(&cache.join("blob.bin"))
        + blocks(&cache.join("deeper"))
        + blocks(&cache.join("deeper/more.bin"));
    let item0 = &plan.plan.items[0];
    assert_eq!(
        (item0.allocated_bytes, item0.frees_bytes, item0.frees_kind),
        (live, Some(live), Some("upper_bound"))
    );
    assert!(plan.unmeasured(0));

    // A change inside it is not one the scan could see: still unchanged.
    fs::write(cache.join("deeper/new.bin"), vec![2u8; 8 * 1024]).unwrap();
    let plan = clean_plan(&[signed(&app)], &index, &q, Method::Trash).unwrap();
    assert_eq!(plan.changed_count(), 0);
    // A change beside it is.
    fs::write(app.join("lib.rs"), b"x").unwrap();
    let plan = clean_plan(&[item(app.clone())], &index, &q, Method::Trash).unwrap();
    assert_eq!(plan.changed_count(), 1);
}
