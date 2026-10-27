//! P1-CLI-03: the queries on in-memory trees.

use std::path::{Path, PathBuf};

use duscope_walk::{Attrs, MemProvider, WalkOptions, walk_with};

use super::{
    ArenaView, ByTypeQuery, Filters, SizeKind, SortKey, StaleQuery, TopQuery, TreeQuery, TreeView,
    by_type, globs, stale, top, tree,
};
use crate::CoreError;
use crate::arena::Arena;
use crate::rollup::{RollUp, roll_up};
use crate::rules::Classes;

const MIB: u64 = 1 << 20;

/// The arena and roll-up of a walk of `tree`.
fn scan(tree: &MemProvider) -> (Arena, RollUp) {
    let mut arena = Arena::new(Attrs::Basic);
    walk_with(tree, tree.root_path(), &WalkOptions::default(), |event| {
        arena.push(event).unwrap();
    })
    .unwrap();
    let rollup = roll_up(&mut arena);
    (arena, rollup)
}

fn view<'a>(arena: &'a Arena, rollup: &'a RollUp, tree: &MemProvider) -> ArenaView<'a> {
    ArenaView::new(arena, rollup, tree.root_path().to_owned())
}

/// The proposal's example: `Library` holds a `Containers` of more than 80%
/// of its bytes, `Movies` has no subdirectories.
fn home() -> MemProvider {
    let mut tree = MemProvider::new("/home");
    tree.file("Library/Containers/app/data.db", 90 * MIB)
        .file("Library/Preferences/p.plist", 2 * MIB)
        .file("Movies/a.mov", 40 * MIB)
        .file("Movies/b.mov", 10 * MIB)
        .file("dev/target/debug/bin", 30 * MIB)
        .file("dev/src/main.rs", 1 << 10)
        .file("dev/node_modules/x/index.js", 20 * MIB)
        .file("notes.txt", 1 << 10);
    tree
}

fn paths(rows: &[String]) -> Vec<&str> {
    rows.iter()
        .map(|p| p.strip_prefix("/home/").unwrap_or(p))
        .collect()
}

#[test]
fn p1_cli_03_top_lists_the_depth_frontier_with_cut_through() {
    let tree = home();
    let (arena, rollup) = scan(&tree);
    let view = view(&arena, &rollup, &tree);
    let query = TopQuery {
        cut_through: true,
        ..TopQuery::default()
    };
    let page = top(&view, &query);
    let rows: Vec<String> = page.entries.iter().map(|e| e.path.clone()).collect();
    // Library is cut through to Containers (more than 80% of it), which is
    // cut through again to app: up to three levels.
    assert_eq!(paths(&rows), ["Library/Containers/app", "dev", "Movies"]);
    let library = &page.entries[0];
    assert_eq!(library.via, ["/home/Library", "/home/Library/Containers"]);
    assert_eq!(library.depth, 3);
    assert_eq!(page.total_count, 3);
    // Strict depth without cut-through.
    let strict = top(&view, &TopQuery::default());
    let rows: Vec<String> = strict.entries.iter().map(|e| e.path.clone()).collect();
    assert_eq!(paths(&rows), ["Library", "dev", "Movies"]);
    assert!(strict.entries.iter().all(|e| e.via.is_empty()));
    // Depth 2: the directories at depth 2, and Movies, which has none
    // below it.
    let deeper = top(
        &view,
        &TopQuery {
            depth: Some(2),
            ..TopQuery::default()
        },
    );
    let rows: Vec<String> = deeper.entries.iter().map(|e| e.path.clone()).collect();
    assert_eq!(
        paths(&rows),
        [
            "Library/Containers",
            "Movies",
            "dev/target",
            "dev/node_modules",
            "Library/Preferences",
            "dev/src"
        ]
    );
}

#[test]
fn p1_cli_03_top_files_and_limits() {
    let tree = home();
    let (arena, rollup) = scan(&tree);
    let view = view(&arena, &rollup, &tree);
    let page = top(
        &view,
        &TopQuery {
            files: true,
            limit: Some(2),
            ..TopQuery::default()
        },
    );
    let rows: Vec<String> = page.entries.iter().map(|e| e.path.clone()).collect();
    assert_eq!(
        paths(&rows),
        ["Library/Containers/app/data.db", "Movies/a.mov"]
    );
    assert_eq!(page.total_count, 8);
    assert_eq!(page.entries[0].kind, "file");
    // --depth limits files too.
    let shallow = top(
        &view,
        &TopQuery {
            files: true,
            depth: Some(1),
            ..TopQuery::default()
        },
    );
    assert_eq!(shallow.total_count, 1);
}

#[test]
fn p1_cli_03_filters_apply_to_every_row() {
    let tree = home();
    let (arena, rollup) = scan(&tree);
    let view = view(&arena, &rollup, &tree);
    let files = |filters: Filters| {
        let page = top(
            &view,
            &TopQuery {
                files: true,
                filters,
                sort: SortKey::Path,
                ..TopQuery::default()
            },
        );
        page.entries
            .iter()
            .map(|e| e.path.trim_start_matches("/home/").to_owned())
            .collect::<Vec<_>>()
    };
    let glob = globs(["*.mov"]).unwrap();
    assert_eq!(
        files(Filters {
            glob: Some(glob.clone()),
            ..Filters::default()
        }),
        ["Movies/a.mov", "Movies/b.mov"]
    );
    // --invert keeps the rest.
    assert_eq!(
        files(Filters {
            glob: Some(glob),
            invert: true,
            ..Filters::default()
        })
        .len(),
        6
    );
    let regex = regex::bytes::Regex::new(r"^dev/.*\.(rs|js)$").unwrap();
    assert_eq!(
        files(Filters {
            regex: Some(regex),
            ..Filters::default()
        }),
        ["dev/node_modules/x/index.js", "dev/src/main.rs"]
    );
    // --exclude, anchored at the root.
    assert_eq!(
        files(Filters {
            exclude: Some(globs(["/dev", "/Library/**"]).unwrap()),
            ..Filters::default()
        }),
        ["Movies/a.mov", "Movies/b.mov", "notes.txt"]
    );
    assert_eq!(
        files(Filters {
            min_size: Some(20 * MIB),
            max_size: Some(40 * MIB),
            ..Filters::default()
        }),
        [
            "Movies/a.mov",
            "dev/node_modules/x/index.js",
            "dev/target/debug/bin"
        ]
    );
    assert_eq!(
        files(Filters {
            min_depth: Some(4),
            ..Filters::default()
        }),
        [
            "Library/Containers/app/data.db",
            "dev/node_modules/x/index.js",
            "dev/target/debug/bin"
        ]
    );
    assert!(
        files(Filters {
            kinds: vec!["dir"],
            ..Filters::default()
        })
        .is_empty()
    );
}

#[test]
fn p1_cli_03_tree_lists_shares_down_to_its_depth() {
    let fixture = home();
    let (arena, rollup) = scan(&fixture);
    let view = view(&arena, &rollup, &fixture);
    let page = tree_rows(&view, 1, 0.0);
    assert_eq!(page, ["", "Library", "dev", "Movies", "notes.txt"]);
    // --min-pct leaves out small entries and what is under them.
    let page = tree_rows(&view, 3, 10.0);
    assert!(
        page.contains(&"Library/Containers/app".to_owned()),
        "{page:?}"
    );
    assert!(!page.contains(&"Library/Preferences".to_owned()));
    assert!(!page.contains(&"notes.txt".to_owned()));
    let rows = tree(
        &view,
        &TreeQuery {
            depth: Some(1),
            ..TreeQuery::default()
        },
    );
    assert!((rows.entries[0].pct_of_parent - 100.0).abs() < f64::EPSILON);
    let library = &rows.entries[1];
    assert!((library.pct_of_root - library.pct_of_parent).abs() < f64::EPSILON);
}

fn tree_rows(view: &ArenaView<'_>, depth: u32, min_pct: f64) -> Vec<String> {
    tree(
        view,
        &TreeQuery {
            depth: Some(depth),
            min_pct,
            ..TreeQuery::default()
        },
    )
    .entries
    .iter()
    .map(|e| {
        e.path
            .trim_start_matches("/home")
            .trim_start_matches('/')
            .to_owned()
    })
    .collect()
}

#[test]
fn p1_cli_03_by_type_lets_directory_rules_win() {
    let mut tree = MemProvider::new("/p");
    tree.file("app/node_modules/pkg/logo.png", 5 * MIB)
        .file("app/node_modules/pkg/index.js", MIB)
        .file("photos/a.jpg", 3 * MIB)
        .file("photos/b.HEIC", 2 * MIB)
        .file("src/main.rs", 100 << 10)
        .file("backup.tar.gz", 4 * MIB)
        .file("Makefile", 1 << 10)
        .file("weird.qqq", 1 << 10);
    let (arena, rollup) = scan(&tree);
    let view = ArenaView::new(&arena, &rollup, PathBuf::from("/p"));
    let classes = Classes::vendored();
    let page = by_type(
        &view,
        &ByTypeQuery {
            classes: &classes,
            examples: 2,
            filters: Filters::default(),
            limit: None,
            size: SizeKind::Apparent,
        },
    );
    let summary: Vec<(&str, u64, u64)> = page
        .entries
        .iter()
        .map(|e| (e.class, e.apparent_bytes, e.file_count))
        .collect();
    assert_eq!(
        summary,
        [
            // node_modules is code: its png counts as code.
            ("code", 6 * MIB + (100 << 10), 3),
            ("media", 5 * MIB, 2),
            ("archives", 4 * MIB, 1),
            // A name without an extension (no mode collected) and an
            // unknown extension are other, last.
            ("other", 2 << 10, 2),
        ]
    );
    let media = &page.entries[1];
    assert_eq!(
        media
            .largest
            .iter()
            .map(|e| e.path.as_str())
            .collect::<Vec<_>>(),
        ["/p/photos/a.jpg", "/p/photos/b.HEIC"]
    );
    // --limit caps the classes, `other` still last when it fits.
    let page = by_type(
        &view,
        &ByTypeQuery {
            classes: &classes,
            examples: 0,
            filters: Filters::default(),
            limit: Some(1),
            size: SizeKind::Apparent,
        },
    );
    assert_eq!(page.entries.len(), 1);
    assert_eq!(page.total_count, 4);
    assert!(page.entries[0].largest.is_empty());
}

#[test]
fn p1_cli_03_stale_needs_both_times_old_and_atime_needs_full_attrs() {
    let tree = home();
    let (arena, rollup) = scan(&tree);
    let view = view(&arena, &rollup, &tree);
    // MemProvider's times are 2026-10-22T00:00:00Z.
    let day = 86_400_000_000_000i64;
    let times = 1_792_627_200_000_000_000i64;
    let query = |now: i64, older: i64| StaleQuery {
        now,
        older_than: older,
        min_size: 10 * MIB,
        ..StaleQuery::default()
    };
    // Ten days later, older than a week: the files of 10 MiB or more.
    let page = stale(&view, &query(times + 10 * day, 7 * day)).unwrap();
    assert_eq!(page.total_count, 5);
    assert_eq!(page.entries[0].mtime, "2026-10-22T00:00:00Z");
    assert_eq!(page.entries[0].atime, None);
    // Older than a month: none yet.
    assert_eq!(
        stale(&view, &query(times + 10 * day, 30 * day))
            .unwrap()
            .total_count,
        0
    );
    // --atime on a basic scan.
    let atime = StaleQuery {
        atime: true,
        ..query(times + 10 * day, 7 * day)
    };
    assert!(matches!(
        stale(&view, &atime),
        Err(CoreError::AttrsNotCollected(_))
    ));
}

#[test]
fn p1_cli_03_sort_keys_and_reverse() {
    let tree = home();
    let (arena, rollup) = scan(&tree);
    let view = view(&arena, &rollup, &tree);
    let order = |sort, reverse| {
        let page = top(
            &view,
            &TopQuery {
                sort,
                reverse,
                ..TopQuery::default()
            },
        );
        page.entries
            .iter()
            .map(|e| e.name.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(order(SortKey::Size, false), ["Library", "dev", "Movies"]);
    assert_eq!(order(SortKey::Size, true), ["Movies", "dev", "Library"]);
    assert_eq!(order(SortKey::Path, false), ["Library", "Movies", "dev"]);
    assert_eq!(order(SortKey::Count, false), ["dev", "Library", "Movies"]);
    assert_eq!(
        SortKey::Size.describe(SizeKind::Allocated, false),
        "allocated_desc"
    );
    assert_eq!(
        SortKey::Path.describe(SizeKind::Allocated, true),
        "path_desc"
    );
    let _ = Path::new("/");
    assert!(view.len() > 10);
}
