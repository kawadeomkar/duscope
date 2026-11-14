//! P2-QRY-01: the plans against the arena, path resolution with folding,
//! and `--under`. P2-QRY-10: every plan, with the filters that read no
//! path, against the arena.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use duscope_testkit::TestEnv;
use duscope_walk::{
    Attrs, Boundary, DirId, Entry, Flags, FullAttrs, Kind, MemProvider, NameRef, Normalization,
    ScanEvent, Share, WalkOptions, walk, walk_with,
};
use proptest::prelude::*;

use super::{Cut, Plan, walks};
use crate::CoreError;
use crate::arena::Arena;
use crate::envelope::{StaleEntry, TopEntry};
use crate::index::{Index, IndexWriter, ScanId, ScanMeta, index_path};
use crate::query::{
    self, After, ArenaView, Filters, Page, SizeKind, StaleQuery, Subtree, TimeField, TopQuery,
    TreeView, abs_path, under,
};
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
                unchanged_before: Some(now - DAY),
                min_size: 4096,
                ..Cut::default()
            };
            for plan in [Plan::Range, Plan::Walk] {
                let got = index
                    .largest_files(d, limit, Some(cut.clone()), Some(plan))
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
                unchanged_before: Some(i64::MAX),
                min_size: u64::MAX,
                ..Cut::default()
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

#[test]
fn p5_sec_04_ties_are_walked_past_a_directory_naming_itself_its_parent() {
    let env = TestEnv::new();
    let dir = env.tmp_dir();
    // Five files of one size, so the last row kept ties with others and
    // the ties are found by walking the tree in path order.
    let mut tree = MemProvider::new("/t");
    tree.file("x1", 10)
        .file("x2", 10)
        .file("a/y1", 10)
        .file("a/y2", 10)
        .file("c/z1", 10);
    let sound = mem_index(&tree, &dir.join("sound"));
    let want = sound.largest_files(0, 3, None, Some(Plan::Range)).unwrap();
    assert_eq!(paths(&sound, &want.ids), ["/t/a/y1", "/t/a/y2", "/t/c/z1"]);
    // The root's row names the root its parent, which the reader accepts
    // (the root is its own parent): the walk must not enter it again.
    let file = mem_index(&tree, &dir.join("self")).file().to_owned();
    let db = rusqlite::Connection::open(&file).unwrap();
    db.execute_batch("UPDATE dirs SET parent_id = 0 WHERE id = 0")
        .unwrap();
    drop(db);
    let index = Index::open(&file).unwrap();
    for plan in [Plan::Range, Plan::Walk] {
        let got = index.largest_files(0, 3, None, Some(plan)).unwrap();
        assert_eq!(got, want, "{plan:?}");
    }
    assert!(index.failure().is_none());
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
            unchanged_before: Some(i64::MAX),
            ..Cut::default()
        };
        for (name, d) in [
            ("root", 0),
            ("first subdirectory", sub),
            ("small subtree", small),
        ] {
            let t = best(&mut || {
                index.largest_files(d, 25, Some(cut.clone()), None).unwrap();
            });
            eprintln!("{name}: a cut every file passes, the chosen plan {t:?}");
            let default = Cut {
                unchanged_before: Some(i64::MAX),
                min_size: query::DEFAULT_MIN_SIZE,
                ..Cut::default()
            };
            let t = best(&mut || {
                index
                    .largest_files(d, 25, Some(default.clone()), None)
                    .unwrap();
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

// P2-QRY-10: filters on the index path.

const YEAR: i64 = 365 * DAY;
/// "Now" for the generated trees: 2027-01-15.
const NOW: i64 = 1_800_000_000_000_000_000;
/// The plans a read can be forced to, and the one the count chooses.
const PLANS: [Option<Plan>; 4] = [None, Some(Plan::Range), Some(Plan::Walk), Some(Plan::Dirs)];

/// One file of a generated tree: the directories it is in (each from a
/// set of names whose path order differs from their name order), its
/// name, kind, sizes and times, each an index into a small set, and what
/// it shares (`share`: nothing, a link of one of three inodes, or a member
/// of one of three clone groups, sharing all its blocks or some).
#[derive(Debug, Clone)]
struct Pick {
    dirs: Vec<u8>,
    name: u8,
    kind: u8,
    apparent: u8,
    allocated: u8,
    times: [u8; 4],
    share: u8,
}

fn pick() -> impl Strategy<Value = Pick> {
    (
        prop::collection::vec(any::<u8>(), 0..4),
        any::<u8>(),
        any::<u8>(),
        any::<u8>(),
        any::<u8>(),
        any::<[u8; 4]>(),
        any::<u8>(),
    )
        .prop_map(
            |(dirs, name, kind, apparent, allocated, times, share)| Pick {
                dirs,
                name,
                kind,
                apparent,
                allocated,
                times,
                share,
            },
        )
}

/// The arena (`--attrs full`) and roll-up of the tree `picks` describe,
/// under `/r`; with `equal`, every file has the same sizes. The links of
/// an inode are one file (its first link's kind, sizes and times, `nlink`
/// 3 whatever the tree holds of it), and a clone group's members regular
/// files of its first member's sizes: the first of each carries the
/// bytes, and a member that shares only some of its blocks its own.
fn generated(picks: &[Pick], equal: bool) -> (Arena, crate::rollup::RollUp) {
    // `a-b` and `a.b` sort before `a/` in a path, after `a` by name.
    const DIRS: [&str; 5] = ["a", "a-b", "a.b", "b", "c"];
    const NAMES: [&str; 5] = ["a", "a-", "a.b", "b", "z"];
    const KINDS: [Kind; 5] = [
        Kind::File,
        Kind::File,
        Kind::Symlink,
        Kind::Fifo,
        Kind::Socket,
    ];
    // One file in sixteen is large, so that a band of large sizes is
    // narrow in a large enough subtree.
    const APPARENT: [u64; 4] = [0, 7, 4096, 5000];
    const ALLOCATED: [u64; 3] = [0, 4096, 8192];
    const LARGE: u64 = 1 << 20;
    const AGES: [i64; 4] = [3 * YEAR, 30 * DAY, DAY, 0];
    // An inode's kind, sizes and times, by its index among the three.
    type Inode = (Kind, (u64, u64), [u8; 4]);
    let at = |i: u8| NOW - AGES[usize::from(i) % AGES.len()];
    let entry = |name: &str, parent: u32, kind: Kind, sizes: (u64, u64), times: [u8; 4]| Entry {
        name: NameRef::from(name),
        parent: DirId::new(parent),
        dir: None,
        kind,
        apparent: sizes.0,
        allocated: sizes.1,
        mtime_ns: at(times[0]),
        ctime_ns: at(times[1]),
        nlink: 1,
        dev: 7,
        ino: 0,
        mnt_id: 7,
        share: Share::None,
        boundary: Boundary::None,
        flags: Flags::empty(),
        full: Some(FullAttrs {
            atime_ns: at(times[2]),
            btime_ns: (times[3] % 3 != 0).then(|| at(times[3])),
            mode: 0o100_644,
            uid: 1,
            gid: 1,
        }),
    };
    let mut arena = Arena::new(Attrs::Full);
    let mut push = |e: Entry| arena.push(ScanEvent::Entry(e)).unwrap();
    push(Entry {
        dir: Some(DirId::new(0)),
        ..entry("r", 0, Kind::Dir, (0, 0), [3; 4])
    });
    let mut dirs: HashMap<Vec<u8>, u32> = HashMap::new();
    let mut inodes: HashMap<u8, Inode> = HashMap::new();
    let mut groups: HashMap<u8, (u64, u64)> = HashMap::new();
    for (i, p) in picks.iter().enumerate() {
        let mut parent = 0;
        let mut path = Vec::new();
        for &d in &p.dirs {
            let d = d % 5;
            path.push(d);
            let next = u32::try_from(dirs.len() + 1).unwrap();
            parent = *dirs.entry(path.clone()).or_insert_with(|| {
                push(Entry {
                    dir: Some(DirId::new(next)),
                    ..entry(DIRS[usize::from(d)], parent, Kind::Dir, (0, 0), [0; 4])
                });
                next
            });
        }
        let sizes = if equal {
            (1000, 4096)
        } else {
            let size = |i: u8, sizes: &[u64]| match i {
                0..16 => LARGE,
                i => sizes[usize::from(i) % sizes.len()],
            };
            (size(p.apparent, &APPARENT), size(p.allocated, &ALLOCATED))
        };
        let name = format!("{}{i}", NAMES[usize::from(p.name) % NAMES.len()]);
        let kind = KINDS[usize::from(p.kind) % KINDS.len()];
        let group = p.share / 8 % 3;
        let (kind, sizes, times) = match p.share % 8 {
            1 => *inodes.entry(group).or_insert((kind, sizes, p.times)),
            2 | 3 => (Kind::File, *groups.entry(group).or_insert(sizes), p.times),
            _ => (kind, sizes, p.times),
        };
        let clone = Share::Clone(500 + u64::from(group));
        let (nlink, ino, share, flags) = match p.share % 8 {
            1 => (3, 1000 + u64::from(group), Share::Hardlink, Flags::empty()),
            2 => (1, 0, clone, Flags::MAY_SHARE | Flags::SHARES_ALL),
            3 => (1, 0, clone, Flags::MAY_SHARE),
            _ => (1, 0, Share::None, Flags::empty()),
        };
        push(Entry {
            nlink,
            ino,
            share,
            flags,
            ..entry(&name, parent, kind, sizes, times)
        });
    }
    let rollup = crate::rollup::roll_up(&mut arena);
    (arena, rollup)
}

/// A query of a generated tree, each part an index into a small set: the
/// verb, the size kind, the filters (kinds as a bit set), `top`'s depth,
/// `stale`'s age and `--atime`, the limit, the subtree and whether a
/// second page is read.
#[derive(Debug, Clone)]
struct Ask {
    stale: bool,
    apparent: bool,
    kinds: u8,
    sizes: (Option<u8>, Option<u8>),
    depths: (Option<u8>, Option<u8>, Option<u8>),
    time: (u8, Option<u8>, Option<u8>),
    older: u8,
    atime: bool,
    limit: u8,
    under: u8,
}

fn ask() -> impl Strategy<Value = Ask> {
    (
        any::<bool>(),
        any::<bool>(),
        prop_oneof![Just(0u8), any::<u8>()],
        any::<(Option<u8>, Option<u8>)>(),
        any::<(Option<u8>, Option<u8>, Option<u8>)>(),
        any::<(u8, Option<u8>, Option<u8>)>(),
        any::<u8>(),
        any::<bool>(),
        any::<u8>(),
        any::<u8>(),
    )
        .prop_map(
            |(stale, apparent, kinds, sizes, depths, time, older, atime, limit, under)| Ask {
                stale,
                apparent,
                kinds,
                sizes,
                depths,
                time,
                older,
                atime,
                limit,
                under,
            },
        )
}

/// A page of either verb.
#[derive(Debug, PartialEq)]
enum Answer {
    Top(Page<TopEntry>),
    Stale(Page<StaleEntry>),
}

impl Answer {
    /// The rows' paths, the count, and the next page's start.
    fn parts(&self) -> (Vec<String>, u64, Option<After>) {
        match self {
            Answer::Top(p) => (
                p.entries.iter().map(|e| e.path.clone()).collect(),
                p.total_count,
                p.next,
            ),
            Answer::Stale(p) => (
                p.entries.iter().map(|e| e.path.clone()).collect(),
                p.total_count,
                p.next,
            ),
        }
    }
}

/// `ask` as the verb's query, the page after `after`.
enum Query {
    Top(TopQuery),
    Stale(StaleQuery),
}

impl Query {
    fn of(ask: &Ask, after: Option<After>) -> Query {
        const KINDS: [&str; 4] = ["file", "symlink", "other", "dir"];
        const BOUNDS: [u64; 6] = [0, 1, 4096, 5000, 8192, 1 << 20];
        const MARKS: [i64; 5] = [
            NOW - 2 * YEAR,
            NOW - 10 * DAY,
            NOW - DAY,
            NOW - DAY + 1,
            NOW + 1,
        ];
        const TIMES: [TimeField; 4] = [
            TimeField::Mtime,
            TimeField::Atime,
            TimeField::Ctime,
            TimeField::Btime,
        ];
        const OLDER: [i64; 4] = [0, DAY, 7 * DAY, YEAR];
        let bound = |i: Option<u8>| i.map(|i| BOUNDS[usize::from(i) % BOUNDS.len()]);
        let depth = |i: Option<u8>| i.map(|i| u32::from(i % 6));
        let mark = |i: Option<u8>| i.map(|i| MARKS[usize::from(i) % MARKS.len()]);
        let filters = Filters {
            kinds: (0..4)
                .filter(|b| ask.kinds & (1 << b) != 0)
                .map(|b| KINDS[b])
                .collect(),
            min_size: bound(ask.sizes.0),
            max_size: bound(ask.sizes.1),
            min_depth: depth(ask.depths.0),
            max_depth: depth(ask.depths.1),
            time: TIMES[usize::from(ask.time.0) % TIMES.len()],
            after: mark(ask.time.1),
            before: mark(ask.time.2),
            ..Filters::default()
        };
        let size = if ask.apparent {
            SizeKind::Apparent
        } else {
            SizeKind::Allocated
        };
        let limit = Some(usize::from(ask.limit % 8) + 1);
        if ask.stale {
            Query::Stale(StaleQuery {
                now: NOW,
                older_than: OLDER[usize::from(ask.older) % OLDER.len()],
                atime: ask.atime,
                min_size: BOUNDS[usize::from(ask.older / 4) % BOUNDS.len()],
                filters,
                limit,
                size,
                after,
                ..StaleQuery::default()
            })
        } else {
            Query::Top(TopQuery {
                depth: depth(ask.depths.2),
                files: true,
                filters,
                limit,
                size,
                after,
                ..TopQuery::default()
            })
        }
    }

    fn run(&self, view: &(impl TreeView + ?Sized)) -> Answer {
        match self {
            Query::Top(q) => Answer::Top(query::top(view, q)),
            Query::Stale(q) => Answer::Stale(query::stale(view, q).unwrap()),
        }
    }

    fn cut(&self) -> Cut {
        match self {
            Query::Top(q) => q.cut(),
            Query::Stale(q) => q.cut(),
        }
        .unwrap()
    }

    fn limit(&self) -> usize {
        match self {
            Query::Top(q) => q.limit,
            Query::Stale(q) => q.limit,
        }
        .unwrap()
    }
}

/// Whether `ask`, on the subtree at `d`, answers on the index as on the
/// arena: the verb's page and the page after it, and each plan forced, by
/// ids and count.
fn answers_as_the_arena(
    arena: &ArenaView<'_>,
    index: &Index,
    d: u32,
    ask: &Ask,
) -> Result<(), TestCaseError> {
    let on_arena = Subtree::new(arena, d);
    let on_index = Subtree::new(index, d);
    let mut after: Option<After> = None;
    for page in 0..2 {
        let q = Query::of(ask, after);
        let want = q.run(&on_arena);
        prop_assert_eq!(
            &q.run(&on_index),
            &want,
            "page {} of {:?} at {}",
            page,
            ask,
            d
        );
        let (paths_wanted, total, next) = want.parts();
        let seen = after
            .as_ref()
            .map_or(0, |a| usize::try_from(a.seen).unwrap());
        for plan in PLANS {
            let got = index
                .largest_files(d, seen + q.limit(), Some(q.cut()), plan)
                .unwrap();
            prop_assert_eq!(got.total_count, total, "{:?} {:?} at {}", plan, ask, d);
            prop_assert_eq!(
                paths(index, got.ids.get(seen..).unwrap_or_default()),
                paths_wanted.clone(),
                "{:?} page {} of {:?} at {}",
                plan,
                page,
                ask,
                d
            );
        }
        match next {
            Some(next) => after = Some(next),
            None => break,
        }
    }
    prop_assert!(index.failure().is_none(), "{:?}", index.failure());
    Ok(())
}

/// Every directory of `arena`'s tree.
fn dirs_of(view: &ArenaView<'_>) -> Vec<u32> {
    (0..view.len())
        .filter(|&id| view.node(id).is_dir())
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Each plan, and the plan the count chooses, answers a random query
    /// of a random tree as the arena does: at its root and in a subtree,
    /// with ties by path (a quarter of the trees have equal sizes), hard
    /// links and clones charged once, on both pages.
    #[test]
    fn p2_qry_10_every_plan_answers_as_the_arena(
        picks in prop::collection::vec(pick(), 1..64),
        equal in prop::bool::weighted(0.25),
        asks in prop::collection::vec(ask(), 1..8),
    ) {
        let env = TestEnv::new();
        let (arena, rollup) = generated(&picks, equal);
        let view = ArenaView::new(&arena, &rollup, PathBuf::from("/r"));
        let index = write(&arena, &rollup, Path::new("/r"), &env.tmp_dir());
        let dirs = dirs_of(&view);
        for ask in &asks {
            answers_as_the_arena(&view, &index, 0, ask)?;
            let d = dirs[usize::from(ask.under) % dirs.len()];
            answers_as_the_arena(&view, &index, d, ask)?;
        }
    }
}

/// A tree of equal files, more than a page of them in one directory, in
/// directories whose path order is not their name order: the ties are
/// found in path order past each page and depth, under every plan.
#[test]
fn p2_qry_10_ties_of_equal_files_are_found_in_path_order_at_every_depth() {
    let env = TestEnv::new();
    let names = ["a", "a-b", "a.b", "b"];
    let picks: Vec<Pick> = (0..900u32)
        .map(|i| {
            let i8 = u8::try_from(i % 251).unwrap();
            Pick {
                // A third in the root, so that it holds more than a page.
                dirs: if i % 3 == 0 {
                    Vec::new()
                } else {
                    vec![i8 % 4, (i8 / 4) % 4][..=usize::from(i8 % 2)].to_vec()
                },
                name: i8,
                kind: u8::from(i % 7 == 0) * 2,
                apparent: 0,
                allocated: 0,
                times: [i8 % 4, 0, 3, 3],
                share: 0,
            }
        })
        .collect();
    let (arena, rollup) = generated(&picks, true);
    let view = ArenaView::new(&arena, &rollup, PathBuf::from("/r"));
    let index = write(&arena, &rollup, Path::new("/r"), &env.tmp_dir());
    assert!(names.iter().all(|n| index.lookup([n.as_bytes()]).is_some()));
    let mut asks = Vec::new();
    for depths in [
        (None, None, None),
        (Some(2), None, None),
        (None, None, Some(1)),
        (Some(3), Some(3), None),
    ] {
        for (kinds, limit) in [(0, 0), (1, 2), (2, 7), (0, 3)] {
            for stale in [false, true] {
                asks.push(Ask {
                    stale,
                    apparent: limit % 2 == 1,
                    kinds,
                    sizes: (None, None),
                    depths,
                    time: (0, None, None),
                    older: 1,
                    atime: false,
                    // Limits of 1, 3, 8 and 4 rows, of 300 and more ties.
                    limit,
                    under: 0,
                });
            }
        }
    }
    for ask in &asks {
        for d in dirs_of(&view) {
            answers_as_the_arena(&view, &index, d, ask).unwrap();
        }
    }
}

/// The plan the count chooses, for each kind of cut, on a tree of 1,200
/// files a twentieth of which are large: the files each keeps are counted
/// as the arena counts them, and read where they cost least.
#[test]
fn p2_qry_10_the_count_chooses_each_plan() {
    let env = TestEnv::new();
    let picks: Vec<Pick> = (0..1200u32)
        .map(|i| {
            let i8 = u8::try_from(i % 251).unwrap();
            Pick {
                dirs: if i % 3 == 0 { Vec::new() } else { vec![i8 % 5] },
                name: i8,
                // A symbolic link in forty.
                kind: if i % 40 == 1 { 2 } else { 0 },
                apparent: 20,
                // 1 MiB (the generator's large size) in twenty, else 4 KiB.
                allocated: if i % 20 == 0 { 0 } else { 16 },
                times: [i8 % 4, 0, 3, 3],
                share: 0,
            }
        })
        .collect();
    let (arena, rollup) = generated(&picks, false);
    let view = ArenaView::new(&arena, &rollup, PathBuf::from("/r"));
    let index = write(&arena, &rollup, Path::new("/r"), &env.tmp_dir());
    let root = index.node(0);
    let large = 1 << 20;
    let cases: [(&str, Cut, Plan); 6] = [
        (
            "a narrow band, judged on the way",
            Cut {
                min_size: large,
                kinds: vec!["file"],
                ..Cut::default()
            },
            Plan::Walk,
        ),
        (
            "a band the index alone judges",
            Cut {
                min_size: 1,
                ..Cut::default()
            },
            Plan::Walk,
        ),
        (
            "a rare kind",
            Cut {
                kinds: vec!["symlink"],
                ..Cut::default()
            },
            Plan::Range,
        ),
        (
            "a common kind",
            Cut {
                kinds: vec!["file"],
                ..Cut::default()
            },
            Plan::Walk,
        ),
        (
            "the root's own files",
            Cut {
                max_depth: Some(1),
                ..Cut::default()
            },
            Plan::Dirs,
        ),
        (
            "apparent sizes, which have no index",
            Cut {
                size: SizeKind::Apparent,
                kinds: vec!["file"],
                ..Cut::default()
            },
            Plan::Range,
        ),
    ];
    for (what, cut, plan) in cases {
        let q = TopQuery {
            files: true,
            filters: Filters {
                kinds: cut.kinds.clone(),
                min_size: (cut.min_size > 0).then_some(cut.min_size),
                ..Filters::default()
            },
            depth: cut.max_depth,
            size: cut.size,
            limit: Some(25),
            ..TopQuery::default()
        };
        let want = query::top(&view, &q).total_count;
        let terms = super::Terms::new(&cut, &root).unwrap();
        assert_eq!(
            index.choose(&terms, &root, 25).unwrap(),
            (plan, want),
            "{what}"
        );
        let got = index.largest_files(0, 25, Some(cut), None).unwrap();
        assert_eq!(got.total_count, want, "{what}");
    }
}

/// A band is narrow below a sixteenth of the subtree, and the count that
/// finds it stops there: at the cap it is wide, and no capped count is
/// taken for the band's.
#[test]
fn p2_qry_10_a_band_at_a_sixteenth_of_the_subtree_is_wide() {
    let env = TestEnv::new();
    for (large, narrow) in [(2, Some(2)), (3, None), (4, None)] {
        // 32 files in `a` (a cap of 3), of which `large` are 1 MiB, and
        // 200 more files of 4 KiB in the root.
        let picks: Vec<Pick> = (0..232u32)
            .map(|i| Pick {
                dirs: if i < 32 { vec![0] } else { Vec::new() },
                name: 0,
                kind: 0,
                apparent: 20,
                allocated: if i < large { 0 } else { 16 },
                times: [0; 4],
                share: 0,
            })
            .collect();
        let (arena, rollup) = generated(&picks, false);
        let dir = env.tmp_dir().join(large.to_string());
        let index = write(&arena, &rollup, Path::new("/r"), &dir);
        let a = index.lookup([&b"a"[..]]).unwrap();
        let cut = Cut {
            min_size: 1 << 20,
            ..Cut::default()
        };
        let root = index.node(a);
        assert_eq!(root.file_count, 32);
        let terms = super::Terms::new(&cut, &root).unwrap();
        assert_eq!(
            index.by_band(&terms, &root, 232).unwrap(),
            narrow,
            "{large}"
        );
    }
}

#[test]
fn p2_qry_10_filters_that_read_paths_stay_off_the_index() {
    let size = SizeKind::Apparent;
    let pushed = Filters {
        kinds: vec!["file"],
        min_size: Some(1),
        max_size: Some(9),
        min_depth: Some(2),
        max_depth: Some(4),
        time: TimeField::Btime,
        after: Some(1),
        before: Some(2),
        ..Filters::default()
    };
    let cut = Cut::of(&pushed, size).unwrap();
    assert_eq!(
        cut,
        Cut {
            size,
            kinds: vec!["file"],
            min_size: 1,
            max_size: Some(9),
            min_depth: Some(2),
            max_depth: Some(4),
            time: TimeField::Btime,
            after: Some(1),
            before: Some(2),
            unchanged_before: None,
            atime: false,
        }
    );
    let glob = query::globs(["*.log"]).unwrap();
    for paths in [
        Filters {
            glob: Some(glob.clone()),
            ..pushed.clone()
        },
        Filters {
            regex: Some(query::Regex::new("x").unwrap()),
            ..Filters::default()
        },
        Filters {
            exclude: Some(glob),
            ..Filters::default()
        },
    ] {
        assert_eq!(Cut::of(&paths, size), None);
    }
    // top's --depth lists to a depth, stale's cut is its own.
    let top = TopQuery {
        depth: Some(3),
        filters: pushed.clone(),
        size,
        ..TopQuery::default()
    };
    assert_eq!(top.cut().unwrap().max_depth, Some(3));
    let stale = StaleQuery {
        now: 100,
        older_than: 30,
        atime: true,
        min_size: 5,
        filters: pushed,
        size,
        ..StaleQuery::default()
    };
    let cut = stale.cut().unwrap();
    assert_eq!(
        (cut.min_size, cut.unchanged_before, cut.atime),
        (5, Some(70), true)
    );
}

#[test]
fn p2_qry_10_the_count_chooses_the_walk_when_matches_are_common() {
    // A million files, 25 rows: every file matching walks; a sixteenth,
    // with a condition the index does not hold, walks; a few do not.
    let n = 1_000_000;
    assert!(walks(n, n, n, n, 25, true));
    assert!(walks(n / 16, n, n, n, 25, true));
    assert!(!walks(200, n, n, n, 25, true));
    assert!(!walks(0, n, n, n, 25, false));
    // Without a residual condition the walk reads the index alone.
    assert!(walks(1_000, n, n, n, 25, false));
    // The directories at three levels hold few rows: read them, not the
    // index.
    assert!(!walks(480, 480, n, n, 25, true));
}

/// Each plan's statements read the tables as designed: the walk along the
/// size index without a sort, the range and the directories by their
/// primary keys, the band count on the index alone.
#[test]
fn p2_qry_10_each_plan_reads_as_designed() {
    let env = TestEnv::new();
    let mut tree = MemProvider::new("/t");
    tree.file("a/x", 10).file("a/b/y", 20).file("c/z", 30);
    let index = mem_index(&tree, &env.tmp_dir());
    let explain = |sql: &str| -> String {
        let mut stmt = index
            .db()
            .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
            .unwrap();
        let n = stmt.parameter_count();
        let rows = stmt
            .query_map(rusqlite::params_from_iter(vec![0i64; n]), |row| {
                row.get::<_, String>(3)
            })
            .unwrap()
            .map(Result::unwrap)
            .collect::<Vec<_>>();
        rows.join("; ")
    };
    let cut = Cut {
        kinds: vec!["file"],
        min_size: 1,
        max_depth: Some(2),
        unchanged_before: Some(1),
        ..Cut::default()
    };
    let root = index.node(0);
    let terms = super::Terms::new(&cut, &root).unwrap();
    let ranked = |plan| {
        let read = terms.read(plan);
        format!(
            "SELECT f.id, f.allocated {} ORDER BY f.allocated DESC, f.id LIMIT 5",
            read.rows()
        )
    };
    let walk = explain(&ranked(Plan::Walk));
    assert!(walk.contains("USING INDEX files_allocated"), "{walk}");
    assert!(
        !walk.contains("TEMP B-TREE"),
        "the walk sorts nothing: {walk}"
    );
    let range = explain(&ranked(Plan::Range));
    assert!(
        range.contains("SEARCH f USING INTEGER PRIMARY KEY"),
        "{range}"
    );
    let dirs = explain(&ranked(Plan::Dirs));
    assert!(
        dirs.contains("SEARCH d USING INTEGER PRIMARY KEY")
            && dirs.contains("SEARCH f USING INTEGER PRIMARY KEY"),
        "{dirs}"
    );
    assert!(!dirs.contains("files_allocated"), "{dirs}");
    let probe = explain(
        "SELECT COUNT(*) FROM (SELECT 1 FROM files AS f INDEXED BY files_allocated \
         WHERE f.id BETWEEN ?1 AND ?2 AND f.allocated >= ?3 LIMIT 9)",
    );
    assert!(
        probe.contains("COVERING INDEX files_allocated"),
        "the band is counted on the index alone: {probe}"
    );
}

/// The filtered shapes `p2_qry_10_timings` times.
fn timed_shapes() -> [(&'static str, Cut); 9] {
    [
        (
            "depth 3",
            Cut {
                max_depth: Some(3),
                ..Cut::default()
            },
        ),
        (
            "min-size 1M",
            Cut {
                min_size: 1 << 20,
                ..Cut::default()
            },
        ),
        (
            "min-size 1",
            Cut {
                min_size: 1,
                ..Cut::default()
            },
        ),
        (
            "apparent",
            Cut {
                size: SizeKind::Apparent,
                ..Cut::default()
            },
        ),
        (
            "apparent, min-size 1K",
            Cut {
                size: SizeKind::Apparent,
                min_size: 1024,
                ..Cut::default()
            },
        ),
        (
            "type symlink",
            Cut {
                kinds: vec!["symlink"],
                ..Cut::default()
            },
        ),
        (
            "type file",
            Cut {
                kinds: vec!["file"],
                ..Cut::default()
            },
        ),
        (
            "min-depth 7",
            Cut {
                min_depth: Some(7),
                ..Cut::default()
            },
        ),
        (
            "stale 1 B, a day",
            Cut {
                min_size: 1,
                unchanged_before: Some(query::clock::now() - DAY),
                ..Cut::default()
            },
        ),
    ]
}

/// What a filtered `top --files` and `stale` cost on `DUSCOPE_BENCH_INDEX`
/// (an index of T2), each plan forced and the one the count chooses,
/// printed: `cargo test --release -p duscope-core p2_qry_10_timings --
/// --ignored --nocapture`.
#[test]
#[ignore = "prints timings; set DUSCOPE_BENCH_INDEX and run in release"]
fn p2_qry_10_timings() {
    let Some(file) = std::env::var_os("DUSCOPE_BENCH_INDEX") else {
        return;
    };
    let index = Index::open(Path::new(&file)).unwrap();
    let median = |f: &mut dyn FnMut()| {
        let mut runs: Vec<_> = (0..9)
            .map(|_| {
                let t = Instant::now();
                f();
                t.elapsed()
            })
            .collect();
        runs.sort();
        runs[runs.len() / 2]
    };
    let shapes = timed_shapes();
    let sub = index.child_dirs(0)[0].0;
    for (name, cut) in shapes {
        for d in [0, sub] {
            let mut line = format!("{name:24} {:>6}:", if d == 0 { "root" } else { "subdir" });
            for plan in PLANS {
                let mut total = 0;
                let t = median(&mut || {
                    total = index
                        .largest_files(d, 25, Some(cut.clone()), plan)
                        .unwrap()
                        .total_count;
                });
                write!(line, " {plan:?} {:.1} ms ({total})", t.as_secs_f64() * 1e3).unwrap();
            }
            eprintln!("{line}");
        }
    }
}
