//! P2-QRY-02: `by-type` from the index's own tables answers as the walk;
//! P2-QRY-09: so does a subtree's; P2-QRY-12: so does a scan with modes.

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;

use duscope_testkit::TestEnv;
use duscope_walk::{Attrs, MemProvider, WalkOptions, walk, walk_with};
use proptest::prelude::*;

use super::{ByTypeQuery, from_index, index, tally};
use crate::arena::Arena;
use crate::index::{Index, IndexWriter, ScanId, ScanMeta, index_path};
use crate::query::{Filters, SizeKind, SortKey, Subtree, TreeView};
use crate::rollup::roll_up;
use crate::rules::{Class, Classes};

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
    index_with(tree, dir, Attrs::Basic)
}

fn index_with(tree: &MemProvider, dir: &Path, attrs: Attrs) -> Index {
    let mut arena = Arena::new(attrs);
    let opts = WalkOptions {
        attrs,
        ..WalkOptions::default()
    };
    walk_with(tree, tree.root_path(), &opts, |e| {
        arena.push(e).unwrap();
    })
    .unwrap();
    saved(arena, tree.root_path(), dir)
}

/// The tree at `root` scanned with `attrs`, saved under `dir` and opened.
fn scanned(root: &Path, dir: &Path, attrs: Attrs) -> Index {
    let mut arena = Arena::new(attrs);
    let opts = WalkOptions {
        attrs,
        ..WalkOptions::default()
    };
    walk(root, &opts, |e| arena.push(e).unwrap()).unwrap();
    saved(arena, root, dir)
}

/// `arena`, a scan of `root`, saved under `dir` and opened.
fn saved(mut arena: Arena, root: &Path, dir: &Path) -> Index {
    let rollup = roll_up(&mut arena);
    let meta = meta(root);
    IndexWriter::write(&arena, &rollup, &meta, dir).unwrap();
    Index::open(&index_path(dir, &meta.scan_id)).unwrap()
}

/// The walk's tallies without the newest file's time, which the tables do
/// not hold and only `--sort mtime`, which walks, reads.
fn walked(view: &(impl TreeView + ?Sized), q: &ByTypeQuery<'_>) -> Vec<super::Tally> {
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
    // Apparent sizes: the walk.
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

/// Every directory of `index` (the root included) read as a subtree
/// answers from the tables as the walk answers, for each count of examples.
fn every_subtree_answers_as_the_walk(index: &Index, examples: &[usize]) -> Result<(), String> {
    let classes = Classes::vendored();
    for id in 0..index.len() {
        if !index.node(id).is_dir() {
            continue;
        }
        let sub = Subtree::new(index, id);
        for &n in examples {
            let q = query(&classes, n);
            let fast = from_index(&sub, &q).ok_or(format!("{id}: the tables did not answer"))?;
            let walk = walked(&sub, &q);
            if fast != walk {
                return Err(format!(
                    "{} (id {id}), examples {n}:\n tables {fast:?}\n walk   {walk:?}",
                    sub.root_path().display()
                ));
            }
        }
    }
    match index.failure() {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

#[test]
fn p2_qry_09_every_subtree_answers_from_the_tables_as_the_walk() {
    let env = TestEnv::new();
    let mut tree = MemProvider::new("/r");
    tree.file("photos/a.jpg", 900_000)
        .file("photos/b.JPG", 800_000)
        .file("photos/empty.png", 0)
        .file("src/main.rs", 40_000)
        .file("src/lib.rs", 40_000)
        .file("src/empty.rs", 0)
        .file("README", 5_000)
        .file("archive.tar.gz", 600_000)
        // A rule's directory, one inside it, and a subtree inside both,
        // whose rule is above its root.
        .file("app/node_modules/x/logo.png", 500_000)
        .file("app/node_modules/x/deep/data.json", 200_000)
        .file("app/node_modules/y/node_modules/z/blob.bin", 100_000)
        .file("app/Tool.app/Contents/MacOS/tool", 70_000)
        .file("app/Tool.app/Contents/Info.plist", 1_000)
        .file("repo/.git/objects/ab/cdef", 300_000)
        .dir("hollow/inner")
        // Symbolic links, classed by their own names.
        .link("photos/again.jpg", "photos/a.jpg")
        .link("src/logo-copy.png", "app/node_modules/x/logo.png");
    let index = index_of(&tree, &env.tmp_dir());
    every_subtree_answers_as_the_walk(&index, &[0, 1, 3, 10]).unwrap();
    // What the tables cannot answer walks, in a subtree as in the whole
    // scan: a filter, the newest file, apparent sizes. A scan with modes
    // answers from them too since P2-QRY-12.
    let classes = Classes::vendored();
    let sub = Subtree::new(&index, index.child_dirs(0)[0].0);
    let filtered = ByTypeQuery {
        filters: Filters {
            min_size: Some(1),
            ..Filters::default()
        },
        ..query(&classes, 3)
    };
    let newest = ByTypeQuery {
        sort: SortKey::Mtime,
        ..query(&classes, 3)
    };
    let apparent = ByTypeQuery {
        size: SizeKind::Apparent,
        ..query(&classes, 3)
    };
    for q in [filtered, newest, apparent] {
        assert!(from_index(&sub, &q).is_none(), "{q:?}");
    }
    let full = index_with(&tree, &env.tmp_dir().join("full"), Attrs::Full);
    let sub = Subtree::new(&full, full.child_dirs(0)[0].0);
    let q = query(&classes, 3);
    assert_eq!(from_index(&sub, &q), Some(walked(&sub, &q)));
}

/// The reference tree holds what a `MemProvider` cannot: hard links, whose
/// later links count nothing, and on APFS clones, whose uncharged members
/// count no allocated bytes.
#[test]
fn p2_qry_09_every_directory_of_the_fixture_tree_answers_as_the_walk() {
    let env = TestEnv::new();
    let root = env.fixtures().root();
    let mut arena = Arena::new(Attrs::Basic);
    walk(&root, &WalkOptions::default(), |e| arena.push(e).unwrap()).unwrap();
    let index = saved(arena, &root, &env.tmp_dir());
    every_subtree_answers_as_the_walk(&index, &[3]).unwrap();
}

/// Builds under `root` files without an extension whose mode has an
/// execute bit (the owner's, the group's alone, the others' alone; an empty
/// one; a symbolic link, whose mode has them all; a copy, which APFS makes
/// a clone that counts no allocated bytes): outside every rule, under a
/// rule of another class, and under a rule of `binaries`; beside them files
/// without an extension and without the bit, and an executable with one;
/// and two later hard links of such files, one under a rule to a file
/// outside it and one outside to a file under it, which count nothing.
fn executables(root: &Path) {
    let file = |path: &str, size: usize, mode: u32| {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, vec![b'x'; size]).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
    };
    file("README", 5_000, 0o644);
    file("run.sh", 3_000, 0o755);
    file("bin/tool", 90_000, 0o755);
    file("bin/group-only", 20_000, 0o610);
    file("bin/others-only", 10_000, 0o601);
    file("bin/empty-tool", 0, 0o755);
    file("bin/notes", 1_000, 0o644);
    symlink("tool", root.join("bin/tool-link")).unwrap();
    // `node_modules` is code and `*.app/Contents/MacOS` binaries.
    file("node_modules/pkg/cli", 70_000, 0o755);
    file("node_modules/pkg/LICENSE", 2_000, 0o644);
    file("Tool.app/Contents/MacOS/Tool", 50_000, 0o755);
    // Pre-order puts `bin` before `node_modules` before `zz`.
    fs::hard_link(
        root.join("bin/tool"),
        root.join("node_modules/pkg/tool-again"),
    )
    .unwrap();
    fs::create_dir_all(root.join("zz")).unwrap();
    fs::hard_link(root.join("node_modules/pkg/cli"), root.join("zz/cli-again")).unwrap();
    fs::copy(root.join("bin/tool"), root.join("zz/tool-copy")).unwrap();
}

#[test]
fn p2_qry_12_a_scan_with_modes_answers_from_the_tables_as_the_walk() {
    let env = TestEnv::new();
    let root = env.tmp_dir().join("tree");
    executables(&root);
    let scan = scanned(&root, &env.tmp_dir().join("full"), Attrs::Full);
    assert!(scan.full_attrs());
    // What the tables have to reproduce: the six files without an
    // extension and with an execute bit outside the rules are `binaries`
    // beside the app's, those under `node_modules` code, and the later
    // links count nothing.
    let classes = Classes::vendored();
    let walk = walked(&scan, &query(&classes, 10));
    let files = |class: Class| walk[index(class)].files;
    assert_eq!(
        [Class::Binaries, Class::Code, Class::Other].map(files),
        [7, 3, 2],
        "{walk:?}"
    );
    every_subtree_answers_as_the_walk(&scan, &[0, 1, 3, 10]).unwrap();
    // Without modes the same files are `other`, outside the rules, and
    // the tables answer as the walk there too.
    let basic = scanned(&root, &env.tmp_dir().join("basic"), Attrs::Basic);
    let walk = walked(&basic, &query(&classes, 10));
    let files = |class: Class| walk[index(class)].files;
    assert_eq!(
        [Class::Binaries, Class::Code, Class::Other].map(files),
        [1, 3, 8],
        "{walk:?}"
    );
    every_subtree_answers_as_the_walk(&basic, &[0, 3]).unwrap();
}

/// The reference tree scanned with modes: its executables without an
/// extension lie outside every rule (`tool`, a cargo and a cmake build's
/// binaries), under a rule of `binaries` (an app's, a framework's, one in
/// `DerivedData`) and under one of code (`node_modules/.bin`'s link).
#[test]
fn p2_qry_12_every_directory_of_the_fixture_tree_with_modes_answers_as_the_walk() {
    let env = TestEnv::new();
    let root = env.fixtures().root();
    let full = scanned(&root, &env.tmp_dir().join("full"), Attrs::Full);
    let basic = scanned(&root, &env.tmp_dir().join("basic"), Attrs::Basic);
    let classes = Classes::vendored();
    let binaries = |scan: &Index| walked(scan, &query(&classes, 0))[index(Class::Binaries)].files;
    assert!(binaries(&full) > binaries(&basic));
    every_subtree_answers_as_the_walk(&full, &[3]).unwrap();
}

/// A tree with one entry per pick: a path of directories from a set that
/// holds the rules' names, then a file named from a set of extensions with
/// a size from a set that holds 0, or a symbolic link to an earlier file.
fn tree_from(picks: &[(Vec<u8>, u8, u8, u8)]) -> MemProvider {
    const DIRS: [&str; 8] = [
        "a",
        "b",
        "node_modules",
        ".git",
        "objects",
        "x.framework",
        "DerivedData",
        "c",
    ];
    const FILES: [&str; 8] = [
        "f.png", "g.TXT", "README", "h.tar.gz", "i.json", "j.bin", ".hidden", "k.rs",
    ];
    const SIZES: [u64; 5] = [0, 1, 4_096, 5_000, 900_000];
    let mut tree = MemProvider::new("/p");
    let mut files: Vec<String> = Vec::new();
    for (i, (dirs, file, size, link)) in picks.iter().enumerate() {
        let mut path: Vec<&str> = dirs
            .iter()
            .map(|&d| DIRS[usize::from(d) % DIRS.len()])
            .collect();
        let name = format!("{i}{}", FILES[usize::from(*file) % FILES.len()]);
        path.push(&name);
        let path = path.join("/");
        // Now and then a symbolic link to an earlier file instead of a file.
        if *link < 32 && !files.is_empty() {
            let target = files[usize::from(*link) % files.len()].clone();
            tree.link(&path, &target);
        } else {
            tree.file(&path, SIZES[usize::from(*size) % SIZES.len()]);
            files.push(path);
        }
    }
    tree
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    #[test]
    fn p2_qry_09_subtrees_of_random_trees_answer_as_the_walk(
        picks in prop::collection::vec(
            (prop::collection::vec(any::<u8>(), 0..4), any::<u8>(), any::<u8>(), any::<u8>()),
            1..40,
        ),
    ) {
        let env = TestEnv::new();
        let index = index_of(&tree_from(&picks), &env.tmp_dir());
        prop_assert_eq!(every_subtree_answers_as_the_walk(&index, &[0, 2]), Ok(()));
    }

    /// Scanned with modes, a `MemProvider`'s symbolic links have every
    /// execute bit, so a link named `<n>README` is `binaries` outside the
    /// rules.
    #[test]
    fn p2_qry_12_subtrees_of_random_scans_with_modes_answer_as_the_walk(
        picks in prop::collection::vec(
            (prop::collection::vec(any::<u8>(), 0..4), any::<u8>(), any::<u8>(), any::<u8>()),
            1..40,
        ),
    ) {
        let env = TestEnv::new();
        let index = index_with(&tree_from(&picks), &env.tmp_dir(), Attrs::Full);
        prop_assert_eq!(every_subtree_answers_as_the_walk(&index, &[0, 2]), Ok(()));
    }
}
