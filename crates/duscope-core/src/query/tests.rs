//! P1-CLI-03: the queries on in-memory trees.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use duscope_walk::{
    Attrs, Boundary, DirId, Entry, Flags, Kind, MemProvider, NameRef, ScanEvent, Share,
    WalkOptions, walk_with,
};

use super::{
    ArenaView, ByTypeQuery, Filters, Node, SizeKind, SortKey, StaleQuery, TopQuery, TreeQuery,
    TreeView, by_type, globs, stale, top, tree,
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
fn p1_cli_03_stale_lists_large_files_past_the_cutoff_and_atime_needs_full_attrs() {
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

/// An entry of a synthetic stream: 1000 bytes, 4096 on disk.
fn entry(name: &str, parent: u32, kind: Kind) -> Entry {
    Entry {
        name: NameRef::from(name),
        parent: DirId::new(parent),
        dir: None,
        kind,
        apparent: 1000,
        allocated: 4096,
        mtime_ns: 0,
        ctime_ns: 0,
        nlink: 1,
        dev: 7,
        ino: 0,
        mnt_id: 7,
        share: Share::None,
        boundary: Boundary::None,
        flags: Flags::empty(),
        full: None,
    }
}

/// The arena and roll-up of a synthetic stream.
fn stream(entries: Vec<Entry>) -> (Arena, RollUp) {
    let mut arena = Arena::new(Attrs::Basic);
    for e in entries {
        arena.push(ScanEvent::Entry(e)).unwrap();
    }
    let rollup = roll_up(&mut arena);
    (arena, rollup)
}

#[test]
fn p1_cli_03_a_file_root_is_its_own_row() {
    let (arena, rollup) = stream(vec![entry("f.txt", 0, Kind::File)]);
    let view = ArenaView::new(&arena, &rollup, PathBuf::from("/r/f.txt"));
    let root = view.node(0);
    assert_eq!((root.depth, root.parent, root.allocated), (0, 0, 4096));
    let files = top(
        &view,
        &TopQuery {
            files: true,
            ..TopQuery::default()
        },
    );
    assert_eq!(files.total_count, 1);
    assert_eq!(files.entries[0].path, "/r/f.txt");
    assert_eq!(top(&view, &TopQuery::default()).total_count, 0);
    assert_eq!(tree(&view, &TreeQuery::default()).total_count, 1);
}

#[test]
fn p1_cli_03_shared_bytes_count_once_in_tree_and_by_type() {
    let dir = |name: &str, parent, id| Entry {
        dir: Some(DirId::new(id)),
        apparent: 0,
        allocated: 0,
        ..entry(name, parent, Kind::Dir)
    };
    let link = |name: &str, parent| Entry {
        nlink: 2,
        ino: 99,
        share: Share::Hardlink,
        ..entry(name, parent, Kind::File)
    };
    let clone = |name: &str, parent| Entry {
        share: Share::Clone(5),
        flags: Flags::MAY_SHARE | Flags::SHARES_ALL,
        ..entry(name, parent, Kind::File)
    };
    // a/x.txt and b/x.txt are one inode; a/c.txt and b/c.txt one clone
    // group: b's are charged at a's.
    let (arena, rollup) = stream(vec![
        dir("r", 0, 0),
        dir("a", 0, 1),
        dir("b", 0, 2),
        link("x.txt", 1),
        clone("c.txt", 1),
        link("x.txt", 2),
        clone("c.txt", 2),
    ]);
    let view = ArenaView::new(&arena, &rollup, PathBuf::from("/r"));
    let root = view.node(0);
    assert_eq!((root.allocated, root.apparent), (2 * 4096, 3 * 1000));
    let classes = Classes::vendored();
    for size in [SizeKind::Allocated, SizeKind::Apparent] {
        let page = by_type(
            &view,
            &ByTypeQuery {
                classes: &classes,
                examples: 3,
                filters: Filters::default(),
                limit: None,
                size,
            },
        );
        // The classes add up to the root: the later link not at all, the
        // uncharged clone with its apparent bytes and as a file.
        let sum =
            |f: fn(&crate::envelope::ByTypeEntry) -> u64| page.entries.iter().map(f).sum::<u64>();
        assert_eq!(sum(|e| e.allocated_bytes), root.allocated);
        assert_eq!(sum(|e| e.apparent_bytes), root.apparent);
        assert_eq!(sum(|e| e.file_count), 3);
        // Within a directory, the shares add up to at most 100%.
        let page = tree(
            &view,
            &TreeQuery {
                size,
                ..TreeQuery::default()
            },
        );
        let b: Vec<(String, f64)> = page
            .entries
            .iter()
            .filter(|e| e.path.starts_with("/r/b/"))
            .map(|e| (e.name.clone(), e.pct_of_parent))
            .collect();
        let expected = match size {
            SizeKind::Allocated => [("c.txt".to_owned(), 0.0), ("x.txt".to_owned(), 0.0)],
            SizeKind::Apparent => [("c.txt".to_owned(), 100.0), ("x.txt".to_owned(), 0.0)],
        };
        assert_eq!(b, expected, "{size:?}");
    }
}

#[test]
fn p1_cli_03_stale_needs_both_the_mtime_and_the_ctime_old() {
    let year = 365 * 86_400_000_000_000i64;
    let now = 60 * year;
    let (old, recent) = (now - 3 * year, now - 86_400_000_000_000);
    let file = |name: &str, mtime_ns, ctime_ns| Entry {
        mtime_ns,
        ctime_ns,
        ..entry(name, 0, Kind::File)
    };
    let (arena, rollup) = stream(vec![
        Entry {
            dir: Some(DirId::new(0)),
            ..entry("r", 0, Kind::Dir)
        },
        file("both-old", old, old),
        file("changed-lately", old, recent),
        file("modified-lately", recent, old),
        file("both-recent", recent, recent),
    ]);
    let view = ArenaView::new(&arena, &rollup, PathBuf::from("/r"));
    let page = stale(
        &view,
        &StaleQuery {
            now,
            older_than: year,
            ..StaleQuery::default()
        },
    )
    .unwrap();
    let names: Vec<&str> = page.entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["both-old"]);
}

#[test]
fn p1_cli_03_filters_judge_the_listed_directory_before_cut_through() {
    let tree = home();
    let (arena, rollup) = scan(&tree);
    let view = view(&arena, &rollup, &tree);
    let rows = |filters: Filters| {
        let page = top(
            &view,
            &TopQuery {
                cut_through: true,
                filters,
                ..TopQuery::default()
            },
        );
        page.entries
            .iter()
            .map(|e| e.path.strip_prefix("/home/").unwrap_or(&e.path).to_owned())
            .collect::<Vec<_>>()
    };
    // --glob names the directory listed; its row is still cut through.
    let glob = Filters {
        glob: Some(globs(["Library"]).unwrap()),
        ..Filters::default()
    };
    assert_eq!(rows(glob), ["Library/Containers/app"]);
    // Cut-through stops above a directory --exclude leaves out.
    let exclude = Filters {
        exclude: Some(globs(["app"]).unwrap()),
        ..Filters::default()
    };
    assert_eq!(rows(exclude), ["Library/Containers", "dev", "Movies"]);
}

#[test]
fn p1_cli_03_a_directory_rule_above_the_root_applies_below_it() {
    let mut tree = MemProvider::new("/p");
    tree.file("logo.png", 5 * MIB).file("index.js", MIB);
    let (arena, rollup) = scan(&tree);
    // Scanned as a package inside node_modules: everything is code.
    let view = ArenaView::new(&arena, &rollup, PathBuf::from("/w/node_modules/pkg"));
    let classes = Classes::vendored();
    let page = by_type(
        &view,
        &ByTypeQuery {
            classes: &classes,
            examples: 0,
            filters: Filters::default(),
            limit: None,
            size: SizeKind::Apparent,
        },
    );
    let summary: Vec<(&str, u64)> = page
        .entries
        .iter()
        .map(|e| (e.class, e.file_count))
        .collect();
    assert_eq!(summary, [("code", 2)]);
}

#[test]
fn p1_cli_03_reverse_keeps_ties_in_path_order() {
    let mut tree = MemProvider::new("/t");
    tree.file("b/f", MIB).file("a/f", MIB).file("c/f", MIB);
    let (arena, rollup) = scan(&tree);
    let view = view(&arena, &rollup, &tree);
    let order = |sort, reverse| {
        top(
            &view,
            &TopQuery {
                sort,
                reverse,
                ..TopQuery::default()
            },
        )
        .entries
        .iter()
        .map(|e| e.name.clone())
        .collect::<Vec<_>>()
    };
    assert_eq!(order(SortKey::Size, false), ["a", "b", "c"]);
    assert_eq!(order(SortKey::Size, true), ["a", "b", "c"]);
    assert_eq!(order(SortKey::Path, true), ["c", "b", "a"]);
}

#[test]
fn p1_cli_03_globs_refuse_empty_patterns_and_anchor_dot_slash() {
    for pattern in ["", "/", "//", ".", "./"] {
        assert!(globs([pattern]).is_err(), "{pattern:?}");
    }
    for pattern in ["/x", "./x", "//x"] {
        let set = globs([pattern]).unwrap();
        assert!(set.is_match("x") && !set.is_match("y/x"), "{pattern:?}");
    }
    assert!(globs(["x"]).unwrap().is_match("y/x"));
}

/// A view whose nodes carry no names, as the index's will not: names,
/// paths and subdirectories reach the queries through `name`, `path` and
/// `child_dirs` alone.
struct NamelessNodes<'a>(ArenaView<'a>);

impl TreeView for NamelessNodes<'_> {
    fn len(&self) -> u32 {
        self.0.len()
    }

    fn node(&self, id: u32) -> Node<'_> {
        Node {
            name: &[],
            ..self.0.node(id)
        }
    }

    fn root_path(&self) -> &Path {
        self.0.root_path()
    }

    fn full_attrs(&self) -> bool {
        self.0.full_attrs()
    }

    fn name(&self, id: u32) -> Cow<'_, [u8]> {
        Cow::Owned(self.0.node(id).name.to_vec())
    }

    fn path(&self, id: u32) -> Vec<u8> {
        self.0.path(id)
    }

    fn child_dirs(&self, dir: u32) -> Vec<(u32, u32)> {
        self.0.child_dirs(dir)
    }
}

#[test]
fn p1_cli_03_queries_read_names_and_paths_through_the_view() {
    let fixture = home();
    let (arena, rollup) = scan(&fixture);
    let view = view(&arena, &rollup, &fixture);
    // The defaults, on the arena.
    let id_of = |name: &[u8]| {
        (0..view.len())
            .find(|&id| view.node(id).name == name)
            .unwrap()
    };
    let app = id_of(b"app");
    assert_eq!(view.path(app), b"Library/Containers/app");
    assert_eq!(&*view.name(app), b"app");
    let subdirs = view.child_dirs(0);
    let names: Vec<Vec<u8>> = subdirs.iter().map(|&(id, _)| view.path(id)).collect();
    assert_eq!(names, [&b"Library"[..], b"Movies", b"dev"]);
    assert!(
        subdirs
            .iter()
            .all(|&(id, last)| last == view.node(id).last_id)
    );
    assert!(view.child_dirs(id_of(b"notes.txt")).is_empty());
    assert!(view.child_dirs(app).is_empty());
    // The queries give the same rows through a view that keeps names out
    // of its nodes.
    let nameless = NamelessNodes(ArenaView::new(
        &arena,
        &rollup,
        fixture.root_path().to_owned(),
    ));
    let glob = Filters {
        glob: Some(globs(["*.mov", "app"]).unwrap()),
        ..Filters::default()
    };
    for q in [
        TopQuery {
            cut_through: true,
            ..TopQuery::default()
        },
        TopQuery {
            depth: Some(2),
            sort: SortKey::Path,
            ..TopQuery::default()
        },
        TopQuery {
            files: true,
            filters: glob.clone(),
            ..TopQuery::default()
        },
    ] {
        assert_eq!(top(&view, &q), top(&nameless, &q));
    }
    let q = TreeQuery {
        depth: Some(4),
        filters: glob,
        ..TreeQuery::default()
    };
    assert_eq!(tree(&view, &q), tree(&nameless, &q));
    let day = 86_400_000_000_000i64;
    let q = StaleQuery {
        now: 1_792_627_200_000_000_000 + 10 * day,
        older_than: 7 * day,
        min_size: MIB,
        ..StaleQuery::default()
    };
    assert_eq!(stale(&view, &q).unwrap(), stale(&nameless, &q).unwrap());
}
