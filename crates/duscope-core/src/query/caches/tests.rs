//! P2-QRY-05: `caches` on trees in memory; the fixture projects and
//! `CACHEDIR.TAG` are in the binary's goldens.

use std::path::{Path, PathBuf};

use duscope_walk::{Attrs, MemProvider, WalkOptions, walk_with};

use super::{CachesQuery, caches, projects};
use crate::arena::Arena;
use crate::query::{ArenaView, Filters, SizeKind, SortKey};
use crate::rollup::{RollUp, roll_up};
use crate::rules::Caches;

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
