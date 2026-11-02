//! P2-TEST-01: a tree exported and imported again is the tree it was.
//!
//! ncdu's JSON holds every entry with its two sizes, so the import of an
//! export reads entry for entry as the scan did, names of any bytes
//! included. A `.gpscan` holds only the files that take space, under names
//! XML can carry, so its import keeps the totals and those files.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::collections::HashSet;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use duscope_core::arena::Arena;
use duscope_core::index::{Index, IndexWriter, ScanId, ScanMeta, index_path};
use duscope_core::interchange::gpscan::{self, GpscanOpts};
use duscope_core::interchange::import::{ImportFormat, import};
use duscope_core::interchange::ncdu::{self, NcduOpts};
use duscope_core::query::TreeView;
use duscope_core::rollup::roll_up;
use duscope_testkit::TestEnv;
use duscope_walk::{Attrs, Kind, MemProvider, WalkOptions, walk_with};
use proptest::prelude::*;

/// What a generated entry is.
#[derive(Debug, Clone)]
enum What {
    Dir,
    File(u64),
    Symlink(u64),
    Unreadable,
}

/// A generated entry: the directory it goes in (an index into the
/// directories made so far), its name, and what it is.
type Entry = (usize, Vec<u8>, What);

fn what() -> impl Strategy<Value = What> {
    prop_oneof![
        3 => Just(What::Dir),
        6 => (0u64..10_000_000).prop_map(What::File),
        1 => (1u64..200).prop_map(What::Symlink),
        1 => Just(What::Unreadable),
    ]
}

/// A name of any bytes a directory entry may have: neither `/` nor NUL,
/// nor `.` or `..`.
fn any_name() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        "[a-z]{1,8}".prop_map(String::into_bytes),
        "[ -~]{1,10}".prop_map(String::into_bytes),
        proptest::collection::vec(any::<u8>(), 1..10),
    ]
    .prop_filter("a name", |n| {
        !n.contains(&b'/') && !n.contains(&0) && n != b"." && n != b".."
    })
}

/// A name XML carries as it is.
fn xml_name() -> impl Strategy<Value = Vec<u8>> {
    "[a-zA-Z0-9 ._&<>\"'-]{1,10}"
        .prop_map(String::into_bytes)
        .prop_filter("a name", |n| n != b"." && n != b"..")
}

/// The tree the entries describe, under `/r`: a name already taken in its
/// directory is skipped, and nothing goes in an unreadable directory.
fn tree(entries: &[Entry]) -> MemProvider {
    let mut tree = MemProvider::new("/r");
    let mut dirs: Vec<PathBuf> = vec![PathBuf::from("/r")];
    let mut taken: HashSet<PathBuf> = HashSet::new();
    for (parent, name, what) in entries {
        let path = dirs[parent % dirs.len()].join(OsStr::from_bytes(name));
        if !taken.insert(path.clone()) {
            continue;
        }
        match what {
            What::Dir => {
                tree.dir(&path);
                dirs.push(path);
            }
            What::File(size) => {
                tree.file(&path, *size);
            }
            What::Symlink(len) => {
                tree.symlink(&path, *len);
            }
            What::Unreadable => {
                tree.dir(&path).unreadable(&path, 13);
            }
        }
    }
    tree
}

fn meta(root: &[u8], source: &str) -> ScanMeta {
    ScanMeta {
        scan_id: ScanId::new(root, 1_700_000_000_000_000_000, [7; 8]),
        root: root.to_vec(),
        root_as_given: root.to_vec(),
        started_at: 1_700_000_000_000_000_000,
        finished_at: 1_700_000_001_000_000_000,
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
        clones_unavailable: false,
        incremental: false,
        parent_scan_id: None,
        source: source.to_owned(),
        tool_version: "0.0.0".to_owned(),
    }
}

/// `arena`, rolled up and written under `dir`, opened.
fn saved(mut arena: Arena, root: &[u8], source: &str, dir: &Path) -> Index {
    let rollup = roll_up(&mut arena);
    let meta = meta(root, source);
    IndexWriter::write(&arena, &rollup, &meta, dir).unwrap();
    Index::open(&index_path(dir, &meta.scan_id)).unwrap()
}

/// The scan of `tree`, saved under `dir`.
fn scanned(tree: &MemProvider, dir: &Path) -> Index {
    let mut arena = Arena::new(Attrs::Basic);
    walk_with(tree, tree.root_path(), &WalkOptions::default(), |e| {
        arena.push(e).unwrap();
    })
    .unwrap();
    saved(arena, b"/r", "scan", dir)
}

/// What ncdu's format keeps of an entry: its path, whether it is a
/// directory, its two sizes (a directory's subtree's), its counts, and
/// whether it could be read.
#[derive(Debug, PartialEq, Eq)]
struct Kept {
    path: Vec<u8>,
    dir: bool,
    apparent: u64,
    allocated: u64,
    files: u64,
    dirs: u64,
    unreadable: bool,
}

/// What ncdu's format keeps of each entry of `index`, in id order.
fn entries(index: &Index) -> Vec<Kept> {
    (0..index.len())
        .map(|id| {
            let node = index.node(id);
            Kept {
                path: index.path(id),
                dir: node.kind == Kind::Dir,
                apparent: node.apparent,
                allocated: node.allocated,
                files: node.file_count,
                dirs: node.dir_count,
                unreadable: node.unreadable,
            }
        })
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    #[test]
    fn p2_test_01_an_ncdu_export_imports_entry_for_entry(
        entries_in in proptest::collection::vec((0usize..64, any_name(), what()), 0..40)
    ) {
        let env = TestEnv::new();
        let tree = tree(&entries_in);
        let scan = scanned(&tree, &env.tmp_dir().join("scan"));
        let mut file = Vec::new();
        let opts = NcduOpts { progver: "0.0.0", timestamp: 1_700_000_000 };
        ncdu::write(&scan, 0, &opts, &mut file).unwrap();
        let imported = import(file.as_slice(), Some(ImportFormat::Ncdu)).unwrap();
        prop_assert_eq!(&imported.root, b"/r");
        let back = saved(imported.arena, b"/r", "import:ncdu", &env.tmp_dir().join("back"));
        prop_assert_eq!(entries(&back), entries(&scan));
    }

    #[test]
    fn p2_test_01_a_gpscan_export_imports_its_files_and_totals(
        entries_in in proptest::collection::vec((0usize..64, xml_name(), what()), 0..40)
    ) {
        let env = TestEnv::new();
        let tree = tree(&entries_in);
        let scan = scanned(&tree, &env.tmp_dir().join("scan"));
        let mut file = Vec::new();
        let opts = GpscanOpts {
            apparent: false,
            volume_path: b"/".to_vec(),
            volume_size: 1 << 40,
            free_space: 1 << 39,
            scan_time_ns: 1_700_000_000_000_000_000,
            app_version: "0.0.0",
        };
        gpscan::write(&scan, 0, &opts, &mut file).unwrap();
        let imported = import(file.as_slice(), Some(ImportFormat::Gpscan)).unwrap();
        // The top folder is named below the volume, which the import joins
        // back on.
        prop_assert_eq!(&imported.root, b"/r");
        let back = saved(imported.arena, b"/r", "import:gpscan", &env.tmp_dir().join("back"));
        // The files that take space, by path and size; the folders' totals
        // are theirs added up.
        let taking = |index: &Index| -> Vec<(Vec<u8>, u64)> {
            let mut files: Vec<(Vec<u8>, u64)> = (0..index.len())
                .map(|id| (id, index.node(id)))
                .filter(|(_, node)| node.kind != Kind::Dir && node.charged && node.allocated > 0)
                .map(|(id, node)| (index.path(id), node.allocated))
                .collect();
            files.sort();
            files
        };
        prop_assert_eq!(taking(&back), taking(&scan));
        prop_assert_eq!(back.node(0).allocated, scan.node(0).allocated);
    }
}
