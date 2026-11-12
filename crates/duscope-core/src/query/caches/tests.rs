//! P2-QRY-05: `caches` on trees in memory; the fixture projects and
//! `CACHEDIR.TAG` are in the binary's goldens. P2-QRY-11: a saved scan,
//! read sparsely, answers as the arena.

use std::fs;
use std::path::{Path, PathBuf};

use duscope_testkit::TestEnv;
use duscope_walk::{Attrs, MemProvider, WalkOptions, walk, walk_with};
use proptest::prelude::*;
use rusqlite::Connection;

use super::{CachesQuery, Entries, caches, looked_for, projects, rule_markers};
use crate::arena::Arena;
use crate::index::{Index, IndexWriter, ScanId, ScanMeta, index_path};
use crate::query::{ArenaView, Filters, SizeKind, SortKey, Subtree, TreeView};
use crate::rollup::{RollUp, roll_up};
use crate::rules::{Caches, Confirm};

const MIB: u64 = 1 << 20;

fn scan(tree: &MemProvider) -> (Arena, RollUp) {
    let mut arena = Arena::new(Attrs::Basic);
    walk_with(tree, tree.root_path(), &WalkOptions::default(), |event| {
        arena.push(event).unwrap();
    })
    .unwrap();
    let rollup = roll_up(&mut arena);
    (arena, rollup)
}

fn nothing_above(_: &Path, _: &str) -> bool {
    false
}

fn query<'a>(caches: &'a Caches, above: &'a dyn Fn(&Path, &str) -> bool) -> CachesQuery<'a> {
    CachesQuery {
        caches,
        above,
        loose: false,
        min_size: 0,
        filters: Filters::default(),
        sort: SortKey::Size,
        reverse: false,
        limit: None,
        size: SizeKind::Apparent,
        after: None,
    }
}

/// Each row's path, rule, project and whether it is confirmed.
fn rows(tree: &MemProvider, q: &CachesQuery<'_>) -> Vec<(String, String, Option<String>, bool)> {
    let (arena, rollup) = scan(tree);
    let view = ArenaView::new(&arena, &rollup, tree.root_path().to_owned());
    caches(&view, q)
        .entries
        .into_iter()
        .map(|e| (e.path, e.rule, e.project_root, e.confirmed))
        .collect()
}

fn row(
    path: &str,
    rule: &str,
    project: Option<&str>,
    confirmed: bool,
) -> (String, String, Option<String>, bool) {
    (
        path.to_owned(),
        rule.to_owned(),
        project.map(str::to_owned),
        confirmed,
    )
}

#[test]
fn p2_qry_05_a_name_needs_its_marker_unless_loose() {
    let mut tree = MemProvider::new("/p");
    tree.file("app/package.json", 700)
        .file("app/node_modules/x/index.js", 3 * MIB)
        .file("app/node_modules/y/node_modules/z/index.js", MIB)
        .file("rust/Cargo.toml", 400)
        .file("rust/target/debug/bin", 2 * MIB)
        .file("stray/target/debug/leftover", 5 * MIB);
    let vendored = Caches::vendored();
    let q = query(&vendored, &nothing_above);
    // The nested node_modules is part of the outer one; stray/target has
    // no Cargo.toml above it.
    assert_eq!(
        rows(&tree, &q),
        [
            row("/p/app/node_modules", "node_modules", Some("/p/app"), true),
            row("/p/rust/target", "target", Some("/p/rust"), true),
        ]
    );
    let loose = CachesQuery { loose: true, ..q };
    assert_eq!(
        rows(&tree, &loose),
        [
            row("/p/stray/target", "target", None, false),
            row("/p/app/node_modules", "node_modules", Some("/p/app"), true),
            row("/p/rust/target", "target", Some("/p/rust"), true),
        ]
    );
}

#[test]
fn p2_qry_05_the_nearest_marker_names_the_project() {
    let mut tree = MemProvider::new("/p");
    tree.file("mono/package.json", 400)
        .file("mono/packages/web/package.json", 300)
        .file("mono/packages/web/node_modules/react/index.js", 80 << 10)
        .file("mono/tools/scratch/node_modules/helper/index.js", 8 << 10)
        .file("py/requirements.txt", 200)
        .file("py/sub/pyproject.toml", 200)
        .file("py/sub/pkg/__pycache__/m.pyc", 4 << 10);
    let vendored = Caches::vendored();
    let q = query(&vendored, &nothing_above);
    assert_eq!(
        rows(&tree, &q),
        [
            row(
                "/p/mono/packages/web/node_modules",
                "node_modules",
                Some("/p/mono/packages/web"),
                true
            ),
            row(
                "/p/mono/tools/scratch/node_modules",
                "node_modules",
                Some("/p/mono"),
                true
            ),
            // Any of the rule's markers: the nearer pyproject.toml, not the
            // farther requirements.txt.
            row(
                "/p/py/sub/pkg/__pycache__",
                "__pycache__",
                Some("/p/py/sub"),
                true
            ),
        ]
    );
}

#[test]
fn p2_qry_05_markers_above_the_root_and_a_cache_at_the_root() {
    let mut tree = MemProvider::new("/w/crate/target");
    tree.file("debug/bin", MIB);
    let above = |dir: &Path, marker: &str| dir == Path::new("/w/crate") && marker == "Cargo.toml";
    let vendored = Caches::vendored();
    let q = query(&vendored, &above);
    assert_eq!(
        rows(&tree, &q),
        [row("/w/crate/target", "target", Some("/w/crate"), true)]
    );
    // Under the project, from a subdirectory of it.
    let mut tree = MemProvider::new("/w/crate/src");
    tree.file("target/debug/bin", MIB);
    assert_eq!(
        rows(&tree, &q),
        [row("/w/crate/src/target", "target", Some("/w/crate"), true)]
    );
}

#[test]
fn p2_qry_05_a_marker_beyond_a_directory_of_the_rules_name_confirms_that_one() {
    // A tree inside a cargo project's target (as the reference fixture is,
    // under the checkout's): the checkout's Cargo.toml confirms its
    // target, not a target inside it.
    let mut tree = MemProvider::new("/w/target/fixtures/tree");
    tree.file("cases/no-marker/target/debug/leftover", 32 << 10)
        .file("cases/no-marker/node_modules/pkg/index.js", 2 << 10);
    let above = |dir: &Path, marker: &str| {
        dir == Path::new("/w") && matches!(marker, "Cargo.toml" | "package.json")
    };
    let vendored = Caches::vendored();
    let q = query(&vendored, &above);
    // node_modules has no ancestor of its own name: /w's package.json
    // confirms it.
    assert_eq!(
        rows(&tree, &q),
        [row(
            "/w/target/fixtures/tree/cases/no-marker/node_modules",
            "node_modules",
            Some("/w"),
            true
        )]
    );
}

#[test]
fn p2_qry_05_derived_data_under_xcode_only() {
    let mut tree = MemProvider::new("/Users/a");
    tree.file(
        "Library/Developer/Xcode/DerivedData/App-x/Build/app",
        2 * MIB,
    )
    .file("elsewhere/DerivedData/stray.o", MIB);
    let vendored = Caches::vendored();
    let q = query(&vendored, &nothing_above);
    let (arena, rollup) = scan(&tree);
    let view = ArenaView::new(&arena, &rollup, PathBuf::from("/Users/a"));
    let page = caches(&view, &q);
    assert_eq!(page.entries.len(), 1);
    let row = &page.entries[0];
    assert_eq!(row.path, "/Users/a/Library/Developer/Xcode/DerivedData");
    assert_eq!(row.regenerable_by, "xcodebuild");
    assert!(row.project_root.is_none());
    assert!(!row.cachedir_tag);
}

#[test]
fn p2_qry_05_min_size_limit_and_sort() {
    let mut tree = MemProvider::new("/p");
    tree.file("a/package.json", 1)
        .file("a/node_modules/big", 30 * MIB)
        .file("a/.next/cache", 12 * MIB)
        .file("b/package.json", 1)
        .file("b/node_modules/small", 2 * MIB);
    let vendored = Caches::vendored();
    let mut q = query(&vendored, &nothing_above);
    q.min_size = 10 * MIB;
    let (arena, rollup) = scan(&tree);
    let view = ArenaView::new(&arena, &rollup, PathBuf::from("/p"));
    let page = caches(&view, &q);
    let paths: Vec<&str> = page.entries.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(paths, ["/p/a/node_modules", "/p/a/.next"]);
    assert_eq!(page.total_count, 2);
    q.limit = Some(1);
    q.sort = SortKey::Path;
    let page = caches(&view, &q);
    let paths: Vec<&str> = page.entries.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(paths, ["/p/a/.next"]);
    assert_eq!(page.total_count, 2);
}

#[test]
fn p2_qry_05_by_project_sums_each_projects_caches() {
    let mut tree = MemProvider::new("/p");
    tree.file("a/package.json", 1)
        .file("a/node_modules/big", 30 * MIB)
        .file("a/.next/cache", 12 * MIB)
        .file("b/package.json", 1)
        .file("b/node_modules/small", 2 * MIB)
        .file("c/Cargo.toml", 1)
        .file("c/target/debug/bin", 20 * MIB);
    let vendored = Caches::vendored();
    let mut q = query(&vendored, &nothing_above);
    let (arena, rollup) = scan(&tree);
    let view = ArenaView::new(&arena, &rollup, PathBuf::from("/p"));
    let page = projects(&view, &q);
    let got: Vec<(Option<&str>, u64, u64, Vec<&str>)> = page
        .entries
        .iter()
        .map(|p| {
            (
                p.project_root.as_deref(),
                p.apparent_bytes,
                p.cache_count,
                p.caches.iter().map(String::as_str).collect(),
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            (
                Some("/p/a"),
                42 * MIB,
                2,
                vec!["/p/a/node_modules", "/p/a/.next"]
            ),
            (Some("/p/c"), 20 * MIB, 1, vec!["/p/c/target"]),
            (Some("/p/b"), 2 * MIB, 1, vec!["/p/b/node_modules"]),
        ]
    );
    // --min-size is a project's total.
    q.min_size = 15 * MIB;
    let page = projects(&view, &q);
    let kept: Vec<Option<&str>> = page
        .entries
        .iter()
        .map(|p| p.project_root.as_deref())
        .collect();
    assert_eq!(kept, [Some("/p/a"), Some("/p/c")]);
    assert_eq!(page.total_count, 2);
}

#[test]
fn p2_qry_05_loose_gives_way_to_a_confirmed_cache_inside() {
    // build/ has no gradle or cmake marker above it: under --loose it is a
    // row until a confirmed cache turns up inside it, and then it gives way
    // to what is inside.
    let mut tree = MemProvider::new("/r");
    tree.file("build/tools/web/package.json", 300)
        .file("build/tools/web/node_modules/x/index.js", 2 * MIB)
        .file("build/other/target/debug/bin", MIB)
        .file("stray/target/x/node_modules/y/index.js", MIB);
    let vendored = Caches::vendored();
    let q = CachesQuery {
        loose: true,
        ..query(&vendored, &nothing_above)
    };
    assert_eq!(
        rows(&tree, &q),
        [
            row(
                "/r/build/tools/web/node_modules",
                "node_modules",
                Some("/r/build/tools/web"),
                true
            ),
            // Inside the row that gave way: a row of its own.
            row("/r/build/other/target", "target", None, false),
            // Inside a row that stands: part of it.
            row("/r/stray/target", "target", None, false),
        ]
    );
    // Without --loose, only the confirmed one.
    assert_eq!(
        rows(&tree, &query(&vendored, &nothing_above)),
        [row(
            "/r/build/tools/web/node_modules",
            "node_modules",
            Some("/r/build/tools/web"),
            true
        )]
    );
}

#[test]
fn p2_qry_05_the_nearest_marker_chooses_the_rule() {
    let mut tree = MemProvider::new("/r");
    tree.file("build.gradle", 100)
        .file("native/CMakeLists.txt", 100)
        .file("native/build/app.o", MIB)
        .file("build/libs/app.jar", MIB);
    let vendored = Caches::vendored();
    let q = query(&vendored, &nothing_above);
    let (arena, rollup) = scan(&tree);
    let view = ArenaView::new(&arena, &rollup, PathBuf::from("/r"));
    let page = caches(&view, &q);
    let got: Vec<(&str, &str, Option<&str>)> = page
        .entries
        .iter()
        .map(|e| (e.path.as_str(), e.regenerable_by, e.project_root.as_deref()))
        .collect();
    assert_eq!(
        got,
        [
            ("/r/build", "gradle build", Some("/r")),
            ("/r/native/build", "cmake --build", Some("/r/native")),
        ]
    );
}

#[test]
fn p2_qry_05_a_project_row_names_its_ten_largest_caches() {
    let mut tree = MemProvider::new("/r");
    tree.file("pyproject.toml", 100);
    for i in 0..12u64 {
        tree.file(format!("pkg{i:02}/__pycache__/m.pyc"), (i + 1) << 10);
    }
    let vendored = Caches::vendored();
    let q = query(&vendored, &nothing_above);
    let (arena, rollup) = scan(&tree);
    let view = ArenaView::new(&arena, &rollup, PathBuf::from("/r"));
    let page = projects(&view, &q);
    assert_eq!(page.entries.len(), 1);
    let row = &page.entries[0];
    assert_eq!(row.cache_count, 12);
    assert_eq!(row.caches.len(), super::PROJECT_CACHES);
    assert_eq!(row.caches[0], "/r/pkg11/__pycache__");
    assert_eq!(row.apparent_bytes, (1..=12).sum::<u64>() << 10);
}

// P2-QRY-11: the sparse read of a saved scan.

/// What a test says the directories above the root hold.
type Above<'a> = &'a dyn Fn(&Path, &str) -> bool;

fn meta(root: &Path) -> ScanMeta {
    ScanMeta {
        scan_id: ScanId::new(b"caches", 1, [0; 8]),
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

/// The scan of `root` in `arena`, saved under `dir` and opened.
fn saved(arena: &Arena, rollup: &RollUp, root: &Path, dir: &Path) -> Index {
    let meta = meta(root);
    IndexWriter::write(arena, rollup, &meta, dir).unwrap();
    Index::open(&index_path(dir, &meta.scan_id)).unwrap()
}

/// Whether `view` would be read sparsely under `table`: its directories and
/// its files of the names looked for, from the saved scan's two tables.
fn sparse(view: &(impl TreeView + ?Sized), table: &Caches) -> bool {
    let markers = rule_markers(table);
    let wanted = looked_for(&markers);
    matches!(
        Entries::new(view, view.node(0).last_id, &wanted),
        Entries::Saved { .. }
    )
}

/// Asserts that `caches` and `caches --by-project`, with and without
/// `--loose`, answer on `index` as on `arena` for the subtree at `dir`: how
/// many rows they hold, for a check that the comparison compared something.
fn same_answers(
    arena: &ArenaView<'_>,
    index: &Index,
    dir: u32,
    table: &Caches,
    above: Above<'_>,
) -> usize {
    let (want, got) = (Subtree::new(arena, dir), Subtree::new(index, dir));
    let mut rows = 0;
    for loose in [false, true] {
        let q = CachesQuery {
            loose,
            ..query(table, above)
        };
        let page = caches(&want, &q);
        assert_eq!(caches(&got, &q), page, "{dir}, loose {loose}");
        assert_eq!(
            projects(&got, &q),
            projects(&want, &q),
            "{dir}, loose {loose}, by project"
        );
        rows += page.entries.len();
    }
    rows
}

#[test]
fn p2_qry_11_the_index_answers_as_the_arena_on_the_fixture_tree() {
    let env = TestEnv::new();
    let root = fs::canonicalize(env.fixtures().root()).unwrap();
    let mut arena = Arena::new(Attrs::Basic);
    walk(&root, &WalkOptions::default(), |e| arena.push(e).unwrap()).unwrap();
    let rollup = roll_up(&mut arena);
    let index = saved(&arena, &rollup, &root, &env.tmp_dir());
    let view = ArenaView::new(&arena, &rollup, root.clone());
    let vendored = Caches::vendored();
    assert!(sparse(&index, &vendored));
    // Markers above: none, or every one in the directory above the root
    // (the fixture is inside a cargo checkout, whose Cargo.toml confirms
    // what no marker inside the tree does).
    let parent = root.parent().unwrap().to_owned();
    let all_above = move |dir: &Path, _: &str| dir == parent;
    let aboves: [Above<'_>; 2] = [&nothing_above, &all_above];
    // The root, the cases, the home and its projects: every directory down
    // to depth 3.
    let mut rows = 0;
    for dir in (0..view.len()).filter(|&id| {
        let node = view.node(id);
        node.is_dir() && node.depth <= 3
    }) {
        for above in aboves {
            rows += same_answers(&view, &index, dir, &vendored, above);
        }
    }
    assert!(index.failure().is_none(), "{:?}", index.failure());
    assert!(rows > 100, "{rows} rows");
    // Each kind of row is among them: a tag's, a marker's, and a name
    // without its marker.
    let q = CachesQuery {
        loose: true,
        ..query(&vendored, &nothing_above)
    };
    let page = caches(&index, &q);
    let path = |p: &str| format!("{}/{p}", root.display());
    let row = |p: &str| page.entries.iter().find(|e| e.path == path(p)).unwrap();
    let valid = row("cases/cachedir/valid");
    assert_eq!(
        (valid.rule.as_str(), valid.cachedir_tag),
        ("CACHEDIR.TAG", true)
    );
    let target = row("home/Projects/rust-cli/target");
    assert!(target.confirmed && target.cachedir_tag);
    assert!(!row("cases/no-marker/target").confirmed);
    assert!(
        page.entries
            .iter()
            .all(|e| !e.path.starts_with(&path("cases/cachedir/wrong-signature")))
    );
}

#[test]
fn p2_qry_11_every_marker_of_the_rules_is_read() {
    // The vendored rules and a user's, whose markers no other rule has: one
    // spelled outside ASCII, with a quote.
    let user = Caches::parse(
        r#"
[[rule]]
name = "cmake-build-*"
markers = ["CMakePresets.json", "it's ünïcode.txt"]
regenerable_by = "cmake --build"
"#,
        "caches.toml",
    )
    .unwrap();
    let table = Caches::vendored().merged(user);
    // A project for each marker of each rule, holding that marker alone and
    // a directory of the rule's name: confirmed by that marker only.
    let mut tree = MemProvider::new("/m");
    let mut projects_made = Vec::new();
    for (r, rule) in table.rules().iter().enumerate() {
        let (Confirm::Markers(markers), Some(name)) = (&rule.confirm, &rule.name) else {
            continue;
        };
        for (k, marker) in markers.iter().enumerate() {
            let project = format!("p{r:02}-{k}");
            tree.file(format!("{project}/{marker}"), 100)
                .file(format!("{project}/{}/f", name.replace('*', "debug")), MIB);
            projects_made.push(format!("/m/{project}"));
        }
    }
    assert!(projects_made.len() >= 20, "{}", projects_made.len());
    let env = TestEnv::new();
    let (arena, rollup) = scan(&tree);
    let view = ArenaView::new(&arena, &rollup, PathBuf::from("/m"));
    let index = saved(&arena, &rollup, Path::new("/m"), &env.tmp_dir());
    assert!(sparse(&index, &table));
    for loose in [false, true] {
        let q = CachesQuery {
            loose,
            ..query(&table, &nothing_above)
        };
        let page = caches(&index, &q);
        assert_eq!(page, caches(&view, &q), "loose {loose}");
        // Every project's cache, each confirmed by its marker.
        let mut found: Vec<String> = page
            .entries
            .iter()
            .filter(|e| e.confirmed)
            .filter_map(|e| e.project_root.clone())
            .collect();
        found.sort();
        assert_eq!(found, projects_made, "loose {loose}");
    }
    assert!(index.failure().is_none(), "{:?}", index.failure());
}

#[test]
fn p2_qry_11_more_markers_than_a_statement_takes_read_every_entry() {
    let markers: Vec<String> = (0..300).map(|i| format!("\"m{i:03}\"")).collect();
    let user = Caches::parse(
        &format!(
            "[[rule]]\nname = \"out\"\nmarkers = [{}]\nregenerable_by = \"unknown\"\n",
            markers.join(", ")
        ),
        "caches.toml",
    )
    .unwrap();
    let table = Caches::vendored().merged(user);
    let mut tree = MemProvider::new("/r");
    tree.file("a/m299", 10)
        .file("a/out/x", MIB)
        .file("b/package.json", 10)
        .file("b/node_modules/y", MIB);
    let env = TestEnv::new();
    let (arena, rollup) = scan(&tree);
    let view = ArenaView::new(&arena, &rollup, PathBuf::from("/r"));
    let index = saved(&arena, &rollup, Path::new("/r"), &env.tmp_dir());
    // The names do not fit one statement: the pass reads every entry, and
    // answers as the arena.
    assert!(!sparse(&index, &table));
    let q = query(&table, &nothing_above);
    let page = caches(&index, &q);
    assert_eq!(page, caches(&view, &q));
    let paths: Vec<&str> = page.entries.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(paths, ["/r/a/out", "/r/b/node_modules"]);
    assert!(index.failure().is_none(), "{:?}", index.failure());
}

/// More directories and marker files than a page holds, in projects whose
/// caches hold directories and markers of their own, which the pass passes
/// over; every third project has no marker, so that under `--loose` its
/// name is a row and the pass reads on inside it, and every seventh has a
/// cargo project inside.
fn many_projects() -> MemProvider {
    let mut tree = MemProvider::new("/w");
    for i in 0..1500 {
        let p = format!("proj{i:04}");
        if i % 3 != 0 {
            tree.file(format!("{p}/package.json"), 10);
        }
        tree.file(format!("{p}/node_modules/a/b/index.js"), 4096)
            .file(format!("{p}/node_modules/a/package.json"), 10)
            .file(format!("{p}/node_modules/c/node_modules/d/x.js"), 4096)
            .file(format!("{p}/src/lib/m.js"), 100);
        if i % 7 == 0 {
            tree.file(format!("{p}/sub/Cargo.toml"), 10)
                .file(format!("{p}/sub/target/debug/bin"), 8192);
        }
    }
    tree
}

#[test]
fn p2_qry_11_pages_and_the_caches_passed_over_answer_as_the_arena() {
    let tree = many_projects();
    let env = TestEnv::new();
    let (arena, rollup) = scan(&tree);
    let view = ArenaView::new(&arena, &rollup, PathBuf::from("/w"));
    let index = saved(&arena, &rollup, Path::new("/w"), &env.tmp_dir());
    let vendored = Caches::vendored();
    assert!(sparse(&index, &vendored));
    assert!(index.node(0).dir_count > 10 * 1024);
    // Without --loose, the 1,000 node_modules a package.json confirms and
    // the 215 targets; with it, also the 500 node_modules none confirms,
    // each holding its inner one, which the outer's name keeps from its
    // marker.
    let rows = same_answers(&view, &index, 0, &vendored, &nothing_above);
    assert_eq!(rows, 1215 + 1715);
    // A subtree that starts and ends inside a page: proj0700, its
    // node_modules and its target, with and without --loose.
    let (some, _) = view.child_dirs(0)[700];
    assert_eq!(view.path(some), b"proj0700");
    assert_eq!(
        same_answers(&view, &index, some, &vendored, &nothing_above),
        4
    );
    assert!(index.failure().is_none(), "{:?}", index.failure());
}

#[test]
fn p2_qry_11_the_readers_give_each_row_once_in_id_order() {
    let tree = many_projects();
    let env = TestEnv::new();
    let (arena, rollup) = scan(&tree);
    let view = ArenaView::new(&arena, &rollup, PathBuf::from("/w"));
    let index = saved(&arena, &rollup, Path::new("/w"), &env.tmp_dir());
    let names: [&[u8]; 2] = [b"package.json", b"Cargo.toml"];
    let last = view.node(0).last_id;
    let dirs: Vec<u32> = (0..=last).filter(|&id| view.node(id).is_dir()).collect();
    let files: Vec<u32> = (0..=last)
        .filter(|&id| !view.node(id).is_dir() && names.contains(&&*view.name(id)))
        .collect();
    assert!(dirs.len() > 10 * 1024 && files.len() > 2 * 1024);
    let read_dirs: Vec<u32> = index.dirs_in(0, last).map(|r| r.id).collect();
    assert_eq!(read_dirs, dirs);
    let read_files: Vec<u32> = index
        .files_named(0, last, &names)
        .unwrap()
        .map(|r| r.id)
        .collect();
    assert_eq!(read_files, files);
    // Read up to a bound at a time: each row once, none past its bound.
    for stride in [1, 300, 5000] {
        let mut bounded = index.files_named(0, last, &names).unwrap();
        let (mut got, mut upto) = (Vec::new(), 0);
        while upto <= last {
            while let Some(row) = bounded.next_upto(upto) {
                assert!(row.id <= upto, "{} past {upto}", row.id);
                got.push(row.id);
            }
            // Nothing past the bound was read.
            assert_eq!(bounded.reach(), None, "upto {upto}");
            upto += stride;
        }
        got.extend(bounded.map(|r| r.id));
        assert_eq!(got, files, "stride {stride}");
    }
    // Each row's name and ids, as the arena has them (but the root's,
    // whose row holds its whole path).
    for row in index.dirs_in(0, last).skip(1).step_by(97) {
        let node = view.node(row.id);
        assert_eq!(
            (row.files_end, row.last_id, &*row.name),
            (node.files_end, node.last_id, &*view.name(row.id))
        );
    }
    // Passing over stretches of ids, within a page and across pages, then
    // taking a row: each the first not passed over, as a cursor over the
    // ids read whole takes it.
    let model = |ids: &[u32], stride: u32| -> Vec<u32> {
        let (mut out, mut at, mut to) = (Vec::new(), 0, 0);
        while to <= last {
            while ids.get(at).is_some_and(|&id| id < to) {
                at += 1;
            }
            out.extend(ids.get(at).copied());
            at += 1;
            to += stride;
        }
        out
    };
    for stride in [3, 700, 2500] {
        let mut skipping_dirs = index.dirs_in(0, last);
        let mut skipping_files = index.files_named(0, last, &names).unwrap();
        let (mut got_dirs, mut got_files) = (Vec::new(), Vec::new());
        let mut to = 0;
        while to <= last {
            skipping_dirs.skip_to(to);
            skipping_files.skip_to(to);
            got_dirs.extend(skipping_dirs.next().map(|r| r.id));
            got_files.extend(skipping_files.next().map(|r| r.id));
            to += stride;
        }
        assert_eq!(got_dirs, model(&dirs, stride), "stride {stride}");
        assert_eq!(got_files, model(&files, stride), "stride {stride}");
    }
    assert!(index.failure().is_none(), "{:?}", index.failure());
}

#[test]
fn p2_qry_11_a_cache_passed_over_is_not_read() {
    // A confirmed node_modules of more directories than a page holds, and
    // in its last one a marker whose row is damaged. The walk passed over
    // the cache once it was confirmed, and never read that row; nor does
    // the sparse read, whose files are read no further than its
    // directories, and which passes over the cache in both.
    let mut tree = MemProvider::new("/r");
    tree.file("app/package.json", 10);
    for i in 0..1500 {
        tree.file(format!("app/node_modules/m{i:04}/index.js"), 4096);
    }
    tree.file("app/node_modules/m1499/package.json", 10);
    tree.file("z/package.json", 10)
        .file("z/node_modules/x", 4096);
    let env = TestEnv::new();
    let dir = env.tmp_dir();
    let (arena, rollup) = scan(&tree);
    let index = saved(&arena, &rollup, Path::new("/r"), &dir);
    let damaged = (0..index.len())
        .find(|&id| index.path(id) == b"app/node_modules/m1499/package.json")
        .unwrap();
    drop(index);
    let file = index_path(&dir, &meta(Path::new("/r")).scan_id);
    let db = Connection::open(&file).unwrap();
    db.execute("UPDATE files SET parent_id = id WHERE id = ?1", [damaged])
        .unwrap();
    drop(db);
    let index = Index::open(&file).unwrap();
    let vendored = Caches::vendored();
    assert!(sparse(&index, &vendored));
    let rows: Vec<String> = caches(&index, &query(&vendored, &nothing_above))
        .entries
        .into_iter()
        .map(|e| e.path)
        .collect();
    assert_eq!(rows, ["/r/app/node_modules", "/r/z/node_modules"]);
    assert!(index.failure().is_none(), "{:?}", index.failure());
    // A read that reaches the row fails.
    let last = index.node(0).last_id;
    let wanted: [&[u8]; 1] = [b"package.json"];
    let _ = index.files_named(0, last, &wanted).unwrap().count();
    assert!(
        index
            .failure()
            .is_some_and(|f| f.starts_with(&format!("files row {damaged}: its parent_id"))),
        "{:?}",
        index.failure()
    );
}

/// Random trees of cache names, markers, tags and other files.
fn random_tree(picks: &[(Vec<u8>, u8, bool)]) -> MemProvider {
    const DIRS: [&str; 8] = [
        "node_modules",
        "target",
        "build",
        "src",
        "a",
        "__pycache__",
        "CACHEDIR.TAG",
        "Pods",
    ];
    const FILES: [&str; 8] = [
        "package.json",
        "Cargo.toml",
        "CMakeLists.txt",
        "CACHEDIR.TAG",
        "x.js",
        "setup.py",
        "Podfile",
        "README",
    ];
    let mut tree = MemProvider::new("/r");
    for (i, (dirs, file, as_dir)) in picks.iter().enumerate() {
        let mut path: Vec<String> = dirs
            .iter()
            .map(|&d| DIRS[usize::from(d) % DIRS.len()].to_owned())
            .collect();
        // Files of the same name in the same directory are one file.
        let name = FILES[usize::from(*file) % FILES.len()];
        if *as_dir {
            path.push(format!("{name}{i}"));
            tree.dir(path.join("/"));
        } else {
            path.push(name.to_owned());
            tree.file(path.join("/"), 4096 * (i as u64 + 1));
        }
    }
    tree
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    #[test]
    fn p2_qry_11_random_trees_answer_as_the_arena(
        picks in prop::collection::vec(
            (prop::collection::vec(any::<u8>(), 0..5), any::<u8>(), any::<bool>()),
            1..40,
        ),
    ) {
        let tree = random_tree(&picks);
        let env = TestEnv::new();
        let (arena, rollup) = scan(&tree);
        let view = ArenaView::new(&arena, &rollup, PathBuf::from("/r"));
        let index = saved(&arena, &rollup, Path::new("/r"), &env.tmp_dir());
        let vendored = Caches::vendored();
        let above = |dir: &Path, marker: &str| dir == Path::new("/") && marker == "Cargo.toml";
        for dir in (0..view.len()).filter(|&id| view.node(id).is_dir()) {
            same_answers(&view, &index, dir, &vendored, &above);
        }
        prop_assert!(index.failure().is_none(), "{:?}", index.failure());
    }
}

/// A small index whose ids are known, under `dir`, changed by `sql`:
/// `/t` holding `p` (1) holding `package.json` (2) and `node_modules` (3)
/// holding `x` (4), and `q` (5) holding `target` (6) holding `y` (7).
fn layout(dir: &Path, sql: &str) -> Index {
    let mut tree = MemProvider::new("/t");
    tree.file("p/package.json", 10)
        .file("p/node_modules/x", MIB)
        .file("q/target/y", MIB);
    let (arena, rollup) = scan(&tree);
    let meta = meta(Path::new("/t"));
    IndexWriter::write(&arena, &rollup, &meta, dir).unwrap();
    let file = index_path(dir, &meta.scan_id);
    let db = Connection::open(&file).unwrap();
    db.execute_batch(sql).unwrap();
    drop(db);
    Index::open(&file).unwrap()
}

#[test]
fn p2_qry_11_rows_out_of_layout_fail_the_read_and_the_pass_ends() {
    let env = TestEnv::new();
    let dir = env.tmp_dir();
    let sound = layout(&dir.join("sound"), "");
    let paths: Vec<Vec<u8>> = (0..sound.len()).map(|id| sound.path(id)).collect();
    assert_eq!(
        paths,
        [
            &b""[..],
            b"p",
            b"p/package.json",
            b"p/node_modules",
            b"p/node_modules/x",
            b"q",
            b"q/target",
            b"q/target/y"
        ],
        "the ids the cases name"
    );
    let vendored = Caches::vendored();
    let q = query(&vendored, &nothing_above);
    let rows: Vec<String> = caches(&sound, &q)
        .entries
        .into_iter()
        .map(|e| e.path)
        .collect();
    assert_eq!(rows, ["/t/p/node_modules"]);
    assert!(sound.failure().is_none());
    // Each case: the rows it changes, the subtree read (its first id), and
    // the start of what the read reports.
    let cases = [
        (
            "UPDATE dirs SET parent_id = 3 WHERE id = 1",
            0,
            "dirs row 1: its parent_id 3 is not below its id",
        ),
        (
            "UPDATE dirs SET last_id = 8 WHERE id = 3",
            0,
            "dirs row 3: id <= files_end <= last_id < n_entries fails",
        ),
        (
            "UPDATE files SET parent_id = 2 WHERE id = 2",
            0,
            "files row 2: its parent_id 2 is not below its id",
        ),
        (
            "UPDATE files SET parent_id = 0 WHERE id = 2",
            1,
            "files row 2: its parent_id 0 and last_id 2 put it outside the subtree of the entry \
             1, which ends at 4",
        ),
        (
            "UPDATE dirs SET last_id = 5 WHERE id = 3",
            1,
            "dirs row 3: its parent_id 1 and last_id 5 put it outside the subtree of the entry \
             1, which ends at 4",
        ),
    ];
    for (n, (sql, first, reported)) in cases.into_iter().enumerate() {
        // The two readers, alone.
        let index = layout(&dir.join(format!("{n}-readers")), sql);
        let last = if first == 0 { 7 } else { 4 };
        let wanted: [&[u8]; 1] = [b"package.json"];
        let dirs = index.dirs_in(first, last).count();
        let files = index.files_named(first, last, &wanted).unwrap().count();
        let failure = index.failure();
        assert!(
            failure.as_deref().is_some_and(|f| f.starts_with(reported)),
            "{sql}: {failure:?} ({dirs} dirs, {files} files)"
        );
        // The verb: it ends, and the read has failed.
        let index = layout(&dir.join(format!("{n}-caches")), sql);
        if first == 0 {
            caches(&index, &q);
        } else {
            caches(&Subtree::new(&index, first), &q);
        }
        assert!(index.failure().is_some(), "{sql}");
    }
}

#[test]
fn p5_cli_01_a_cache_under_a_non_utf8_directory_carries_path_raw() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    use crate::envelope::base64;
    let mut tree = MemProvider::new("/p");
    tree.file(Path::new(OsStr::from_bytes(b"caf\xe9/package.json")), 1)
        .file(
            Path::new(OsStr::from_bytes(b"caf\xe9/node_modules/big")),
            30 * MIB,
        )
        .file("ok/package.json", 1)
        .file("ok/node_modules/small", 2 * MIB);
    let vendored = Caches::vendored();
    let q = query(&vendored, &nothing_above);
    let (arena, rollup) = scan(&tree);
    let view = ArenaView::new(&arena, &rollup, PathBuf::from("/p"));
    let got: Vec<_> = caches(&view, &q)
        .entries
        .into_iter()
        .map(|e| (e.path, e.path_raw, e.project_root, e.project_root_raw))
        .collect();
    assert_eq!(
        got,
        [
            (
                "/p/caf\u{fffd}/node_modules".to_owned(),
                Some(base64(b"/p/caf\xe9/node_modules")),
                Some("/p/caf\u{fffd}".to_owned()),
                Some(base64(b"/p/caf\xe9")),
            ),
            (
                "/p/ok/node_modules".to_owned(),
                None,
                Some("/p/ok".to_owned()),
                None
            ),
        ]
    );
    let got: Vec<_> = projects(&view, &q)
        .entries
        .into_iter()
        .map(|p| (p.project_root, p.project_root_raw))
        .collect();
    assert_eq!(
        got,
        [
            (
                Some("/p/caf\u{fffd}".to_owned()),
                Some(base64(b"/p/caf\xe9"))
            ),
            (Some("/p/ok".to_owned()), None),
        ]
    );
}
