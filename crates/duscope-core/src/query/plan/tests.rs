//! P2-QRY-01: the plans against the arena, path resolution with folding,
//! and `--under`.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use duscope_testkit::TestEnv;
use duscope_walk::{Attrs, MemProvider, Normalization, WalkOptions, walk, walk_with};

use super::{Cut, Plan};
use crate::CoreError;
use crate::arena::Arena;
use crate::index::{Index, IndexWriter, ScanId, ScanMeta, index_path};
use crate::query::{self, ArenaView, StaleQuery, Subtree, TopQuery, TreeView, abs_path, under};
use crate::resolve::resolve_in_index;
use crate::rollup::{RollUp, roll_up};

const DAY: i64 = 86_400 * 1_000_000_000;

fn meta(id: &ScanId, root: &Path) -> ScanMeta {
    ScanMeta {
        scan_id: id.clone(),
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

fn write(arena: &Arena, rollup: &RollUp, root: &Path, dir: &Path) -> Index {
    let id = ScanId::new(b"plan", 1, [0; 8]);
    IndexWriter::write(arena, rollup, &meta(&id, root), dir).unwrap();
    Index::open(&index_path(dir, &id)).unwrap()
}

/// The reference fixture tree, its arena and its index.
fn fixture(dir: &Path) -> (Arena, RollUp, PathBuf, Index) {
    let env = TestEnv::new();
    let root = fs::canonicalize(env.fixtures().root()).unwrap();
    let mut arena = Arena::new(Attrs::Basic);
    walk(&root, &WalkOptions::default(), |e| arena.push(e).unwrap()).unwrap();
    let rollup = roll_up(&mut arena);
    let index = write(&arena, &rollup, &root, dir);
    (arena, rollup, root, index)
}

fn mem_index(tree: &MemProvider, dir: &Path) -> Index {
    let mut arena = Arena::new(Attrs::Basic);
    walk_with(tree, tree.root_path(), &WalkOptions::default(), |e| {
        arena.push(e).unwrap();
    })
    .unwrap();
    let rollup = roll_up(&mut arena);
    write(&arena, &rollup, tree.root_path(), dir)
}

#[test]
fn p2_qry_01_the_plan_is_the_range_below_three_root_n() {
    // √1,000,000 is 1,000: the range below 3,000 files, the walk from it.
    assert_eq!(Plan::choose(2_999, 1_000_000), Plan::Range);
    assert_eq!(Plan::choose(3_000, 1_000_000), Plan::Walk);
    assert_eq!(Plan::choose(1_000_000, 1_000_000), Plan::Walk);
    assert_eq!(Plan::choose(0, 0), Plan::Walk);
    assert_eq!(Plan::choose(2, 1), Plan::Range);
}

/// The paths of `ids` in `index`.
fn paths(index: &Index, ids: &[u32]) -> Vec<String> {
    ids.iter()
        .map(|&id| String::from_utf8_lossy(&abs_path(index, id)).into_owned())
        .collect()
}

#[test]
fn p2_qry_01_both_plans_answer_as_the_arena() {
    let env = TestEnv::new();
    let dir = env.tmp_dir();
    let (arena, rollup, root, index) = fixture(&dir);
    let view = ArenaView::new(&arena, &rollup, root);
    // The root, each of its subdirectories, and one of theirs: subtrees on
    // both sides of 3·√N.
    let mut dirs = vec![0];
    for (sub, _) in view.child_dirs(0) {
        dirs.push(sub);
        dirs.extend(view.child_dirs(sub).into_iter().take(1).map(|(d, _)| d));
    }
    let now = 4_102_444_800_000_000_000;
    for &d in &dirs {
        let on_arena = Subtree::new(&view, d);
        for limit in [1, 10, 100] {
            let top = query::top(
                &on_arena,
                &TopQuery {
                    files: true,
                    limit: Some(limit),
                    ..TopQuery::default()
                },
            );
            let want: Vec<String> = top.entries.iter().map(|e| e.path.clone()).collect();
            for plan in [Plan::Range, Plan::Walk] {
                let got = index.largest_files(d, limit, None, Some(plan)).unwrap();
                assert_eq!(paths(&index, &got.ids), want, "{d} {limit} {plan:?}");
                assert_eq!(got.total_count, top.total_count, "{d} {limit} {plan:?}");
            }
            let stale = query::stale(
                &on_arena,
                &StaleQuery {
                    now,
                    older_than: DAY,
                    min_size: 4096,
                    limit: Some(limit),
                    ..StaleQuery::default()
                },
            )
            .unwrap();
            let want: Vec<String> = stale.entries.iter().map(|e| e.path.clone()).collect();
            let cut = Cut {
                before: now - DAY,
                min_allocated: 4096,
            };
            for plan in [Plan::Range, Plan::Walk] {
                let got = index
                    .largest_files(d, limit, Some(cut), Some(plan))
                    .unwrap();
                assert_eq!(paths(&index, &got.ids), want, "stale {d} {limit} {plan:?}");
                assert_eq!(
                    got.total_count, stale.total_count,
                    "stale {d} {limit} {plan:?}"
                );
            }
        }
    }
    assert!(index.failure().is_none());
}

#[test]
fn p2_qry_01_the_walk_reads_the_size_index() {
    let env = TestEnv::new();
    let mut tree = MemProvider::new("/t");
    tree.file("a/x", 10).file("a/y", 20).file("b/z", 30);
    let index = mem_index(&tree, &env.tmp_dir());
    let explain = |sql: &str| -> String {
        let mut stmt = index
            .db()
            .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
            .unwrap();
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(3))
            .unwrap()
            .map(Result::unwrap)
            .collect::<Vec<_>>();
        rows.join("; ")
    };
    let walk = explain(
        "SELECT id, allocated FROM files INDEXED BY files_allocated \
         WHERE id BETWEEN 0 AND 9 ORDER BY allocated DESC, id LIMIT 5",
    );
    assert!(walk.contains("files_allocated"), "{walk}");
    assert!(
        !walk.contains("TEMP B-TREE"),
        "the walk sorts nothing: {walk}"
    );
    let range = explain(
        "SELECT id, allocated FROM files WHERE id BETWEEN 0 AND 9 \
         ORDER BY allocated DESC, id LIMIT 5",
    );
    assert!(range.contains("INTEGER PRIMARY KEY"), "{range}");
}

#[test]
fn p2_qry_01_a_folding_volume_matches_case_and_nfc() {
    let env = TestEnv::new();
    // Stored decomposed, as macOS gives it back; asked composed and in
    // another case.
    let stored = "Photos/Cafe\u{301}/Menu.TXT";
    let mut folding = MemProvider::new("/v");
    folding
        .compares_names(false, Normalization::Insensitive)
        .file(stored, 100);
    let index = mem_index(&folding, &env.tmp_dir());
    let asked = Path::new("/v/photos/caf\u{e9}/menu.txt");
    let id = resolve_in_index(&index, asked).unwrap();
    assert_eq!(&*index.name(id), b"Menu.TXT");
    // Byte for byte first: the stored spelling resolves too.
    assert_eq!(
        resolve_in_index(&index, &Path::new("/v").join(stored)),
        Some(id)
    );
    // A volume that compares bytes finds neither.
    let mut exact = MemProvider::new("/v");
    exact.file(stored, 100);
    let index = mem_index(&exact, &env.tmp_dir().join("exact"));
    assert_eq!(resolve_in_index(&index, asked), None);
    assert!(resolve_in_index(&index, &Path::new("/v").join(stored)).is_some());
    // Case alone, without normalisation (exFAT).
    let mut exfat = MemProvider::new("/x");
    exfat
        .compares_names(false, Normalization::None)
        .file("Dir/File", 1);
    let index = mem_index(&exfat, &env.tmp_dir().join("exfat"));
    assert!(resolve_in_index(&index, Path::new("/x/DIR/file")).is_some());
    assert!(resolve_in_index(&index, Path::new("/x/Dir/Fil")).is_none());
}

#[test]
fn p2_qry_01_a_file_subtree_and_out_of_range_bounds() {
    let env = TestEnv::new();
    let mut tree = MemProvider::new("/t");
    tree.file("a/x", 10).file("a/y", 20).file("b", 30);
    let index = mem_index(&tree, &env.tmp_dir());
    let b = resolve_in_index(&index, Path::new("/t/b")).unwrap();
    // A subtree that is one file is that file, under either plan.
    for plan in [Plan::Range, Plan::Walk] {
        let one = index.largest_files(b, 1, None, Some(plan)).unwrap();
        assert_eq!((one.ids, one.total_count), (vec![b], 1));
    }
    // Every row, and a minimum past the largest size an index holds.
    let all = index.largest_files(0, usize::MAX, None, None).unwrap();
    assert_eq!(all.ids.len(), 3);
    let none = index
        .largest_files(
            0,
            10,
            Some(Cut {
                before: i64::MAX,
                min_allocated: u64::MAX,
            }),
            None,
        )
        .unwrap();
    assert_eq!((none.ids.len(), none.total_count), (0, 0));
}

#[test]
fn p2_qry_01_a_root_typed_in_another_case_holds_the_path() {
    let env = TestEnv::new();
    let mut tree = MemProvider::new("/Volumes/USB/Photos");
    tree.compares_names(false, Normalization::None)
        .file("2024/\u{3bf}\u{3b4}\u{3bf}\u{3c3}", 5);
    let index = mem_index(&tree, &env.tmp_dir());
    // The root's own components fold, and a final sigma typed in capitals
    // matches the stored medial one.
    let asked = Path::new("/volumes/usb/PHOTOS/2024/\u{39f}\u{394}\u{39f}\u{3a3}");
    let id = resolve_in_index(&index, asked).unwrap();
    assert_eq!(
        index.path(id),
        "2024/\u{3bf}\u{3b4}\u{3bf}\u{3c3}".as_bytes()
    );
    // A different root is not this one.
    assert!(resolve_in_index(&index, Path::new("/volumes/usb/Video/2024")).is_none());
}

#[test]
fn p2_qry_01_under_binds_the_subtree() {
    let env = TestEnv::new();
    let mut tree = MemProvider::new("/t");
    tree.file("sub/a", 1).file("sub/deep/b", 2).file("c", 3);
    let index = mem_index(&tree, &env.tmp_dir());
    let (id, last) = under(&index, Path::new("/t/sub")).unwrap();
    assert_eq!(index.path(id), b"sub");
    assert_eq!(last, index.node(id).last_id);
    assert!(last > id);
    assert_eq!(
        under(&index, Path::new("/t")).unwrap(),
        (0, index.len() - 1)
    );
    assert!(matches!(
        under(&index, Path::new("/t/none")),
        Err(CoreError::ScanNotFound(_))
    ));
}

/// The timings P2-QRY-01 promises, printed: each plan on the root and on a
/// subtree of `DUSCOPE_BENCH_INDEX` (an index of T2), and a 22-level path
/// resolved in the fixture tree's deep case. Run in a release build:
/// `cargo test --release -p duscope-core p2_qry_01_timings -- --ignored
/// --nocapture`.
#[test]
#[ignore = "prints timings; set DUSCOPE_BENCH_INDEX and run in release"]
fn p2_qry_01_timings() {
    let best = |f: &mut dyn FnMut()| {
        (0..20)
            .map(|_| {
                let t = Instant::now();
                f();
                t.elapsed()
            })
            .min()
            .unwrap()
    };
    if let Some(file) = std::env::var_os("DUSCOPE_BENCH_INDEX") {
        let index = Index::open(Path::new(&file)).unwrap();
        let sub = index.child_dirs(0)[0].0;
        let total = index.node(0).file_count;
        // A subtree the range is chosen for, of at least a hundred files.
        let small = (0..index.len())
            .find(|&id| {
                let n = index.node(id);
                n.is_dir()
                    && n.file_count >= 100
                    && Plan::choose(n.file_count, total) == Plan::Range
            })
            .unwrap();
        let cut = Cut {
            before: i64::MAX,
            min_allocated: 0,
        };
        for (name, d) in [
            ("root", 0),
            ("first subdirectory", sub),
            ("small subtree", small),
        ] {
            let t = best(&mut || {
                index.largest_files(d, 25, Some(cut), None).unwrap();
            });
            eprintln!("{name}: a cut every file passes, the chosen plan {t:?}");
            let default = Cut {
                before: i64::MAX,
                min_allocated: query::DEFAULT_MIN_SIZE,
            };
            let t = best(&mut || {
                index.largest_files(d, 25, Some(default), None).unwrap();
            });
            eprintln!("{name}: stale's default 10 MiB cut, the chosen plan {t:?}");
            for plan in [Plan::Range, Plan::Walk] {
                let chosen = Plan::choose(index.node(d).file_count, index.node(0).file_count);
                let t = best(&mut || {
                    index.largest_files(d, 25, None, Some(plan)).unwrap();
                });
                eprintln!("{name}: {plan:?} {t:?} (chosen: {chosen:?})");
            }
        }
    }
    let env = TestEnv::new();
    let dir = env.tmp_dir();
    let (_, _, root, index) = fixture(&dir);
    let deep = (0..index.len())
        .find(|&id| index.node(id).depth == 22)
        .unwrap();
    let path = root.join(std::ffi::OsStr::new(
        &String::from_utf8(index.path(deep)).unwrap(),
    ));
    let t = best(&mut || {
        assert_eq!(resolve_in_index(&index, &path), Some(deep));
    });
    eprintln!("22-level path: {t:?}");
}
