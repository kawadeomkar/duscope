//! P2-IDX-03: the reader, against the arena each index was written from,
//! and `--scan`'s resolution.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use duscope_testkit::TestEnv;
use duscope_walk::{Attrs, Kind, MemProvider, WalkOptions, walk, walk_with};
use rusqlite::Connection;

use super::Index;
use crate::CoreError;
use crate::arena::Arena;
use crate::index::{IndexWriter, ScanId, ScanMeta, index_path};
use crate::query::{
    self, ArenaView, ByTypeQuery, Node, ScanQuery, ScanSelector, StaleQuery, Subtree, TopQuery,
    TreeQuery, TreeView, resolve_scan,
};
use crate::rollup::{RollUp, roll_up};
use crate::rules::Classes;

const DAY: i64 = 86_400 * 1_000_000_000;

fn meta(id: ScanId, root: &Path, started_at: i64) -> ScanMeta {
    ScanMeta {
        scan_id: id,
        root: root.as_os_str().as_encoded_bytes().to_vec(),
        root_as_given: root.as_os_str().as_encoded_bytes().to_vec(),
        started_at,
        finished_at: started_at + 1_000,
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

/// The reference fixture tree, scanned with `attrs`, and its index under
/// `dir`.
fn fixture_index(dir: &Path, attrs: Attrs) -> (Arena, RollUp, PathBuf, Index) {
    let env = TestEnv::new();
    let root = fs::canonicalize(env.fixtures().root()).unwrap();
    let opts = WalkOptions {
        attrs,
        ..WalkOptions::default()
    };
    let mut arena = Arena::new(attrs);
    walk(&root, &opts, |event| arena.push(event).unwrap()).unwrap();
    let rollup = roll_up(&mut arena);
    let id = ScanId::new(b"fixture", 1, [0; 8]);
    IndexWriter::write(&arena, &rollup, &meta(id.clone(), &root, 1), dir).unwrap();
    let index = Index::open(&index_path(dir, &id)).unwrap();
    (arena, rollup, root, index)
}

/// A node as both views can answer it: an index keeps no link count for a
/// directory, and spells the kinds it does not name (sockets, pipes,
/// devices) as one.
fn comparable(mut node: Node) -> Node {
    if node.kind == Kind::Dir {
        node.nlink = 0;
    }
    if !matches!(node.kind, Kind::Dir | Kind::File | Kind::Symlink) {
        node.kind = Kind::Unknown;
    }
    node
}

fn assert_same_views(arena: &(impl TreeView + ?Sized), index: &(impl TreeView + ?Sized)) {
    assert_eq!(index.len(), arena.len());
    assert_eq!(index.root_path(), arena.root_path());
    assert_eq!(index.full_attrs(), arena.full_attrs());
    for id in 0..arena.len() {
        assert_eq!(
            comparable(index.node(id)),
            comparable(arena.node(id)),
            "{id}"
        );
        assert_eq!(index.name(id), arena.name(id), "{id}");
        assert_eq!(index.path(id), arena.path(id), "{id}");
        assert_eq!(index.child_dirs(id), arena.child_dirs(id), "{id}");
    }
}

fn json(rows: impl serde::Serialize) -> serde_json::Value {
    serde_json::to_value(rows).unwrap()
}

/// Every list query, its rows as JSON, on a view; long lists are cut,
/// which still compares their order.
fn answers(view: &(impl TreeView + ?Sized), full: bool) -> Vec<serde_json::Value> {
    let classes = Classes::vendored();
    let mut out = vec![
        json(query::top(view, &TopQuery::default()).entries),
        json(
            query::top(
                view,
                &TopQuery {
                    files: true,
                    limit: Some(100),
                    ..TopQuery::default()
                },
            )
            .entries,
        ),
        json(
            query::top(
                view,
                &TopQuery {
                    cut_through: true,
                    depth: Some(3),
                    ..TopQuery::default()
                },
            )
            .entries,
        ),
        json(
            query::tree(
                view,
                &TreeQuery {
                    depth: Some(3),
                    limit: Some(200),
                    ..TreeQuery::default()
                },
            )
            .entries,
        ),
        json(
            query::by_type(
                view,
                &ByTypeQuery {
                    classes: &classes,
                    examples: 3,
                    filters: query::Filters::default(),
                    limit: None,
                    size: query::SizeKind::Allocated,
                    sort: query::SortKey::Size,
                    reverse: false,
                },
            )
            .entries,
        ),
    ];
    let stale = StaleQuery {
        now: 4_102_444_800_000_000_000,
        older_than: DAY,
        min_size: 0,
        limit: Some(100),
        atime: full,
        ..StaleQuery::default()
    };
    out.push(json(query::stale(view, &stale).unwrap().entries));
    out
}

#[test]
fn p2_idx_03_the_index_reads_as_the_arena_it_was_written_from() {
    for attrs in [Attrs::Basic, Attrs::Full] {
        let env = TestEnv::new();
        let dir = env.tmp_dir();
        let (arena, rollup, root, index) = fixture_index(&dir, attrs);
        let arena_view = ArenaView::new(&arena, &rollup, root);
        assert_same_views(&arena_view, &index);
        assert!(index.failure().is_none(), "{:?}", index.failure());
    }
}

#[test]
fn p2_idx_03_queries_on_the_index_equal_queries_on_the_arena() {
    for attrs in [Attrs::Basic, Attrs::Full] {
        let env = TestEnv::new();
        let dir = env.tmp_dir();
        let (arena, rollup, root, index) = fixture_index(&dir, attrs);
        let arena_view = ArenaView::new(&arena, &rollup, root);
        let full = attrs == Attrs::Full;
        assert_eq!(answers(&index, full), answers(&arena_view, full));
        // Re-rooted at the root's first subdirectories, as a verb asked
        // about a path inside the scan reads it.
        for (dir, _) in arena_view.child_dirs(0).into_iter().take(2) {
            let on_index = Subtree::new(&index, dir);
            let on_arena = Subtree::new(&arena_view, dir);
            assert_same_views(&on_arena, &on_index);
            assert_eq!(answers(&on_index, full), answers(&on_arena, full), "{dir}");
        }
        assert!(index.failure().is_none(), "{:?}", index.failure());
    }
}

#[test]
fn p2_idx_03_a_subtree_reads_as_a_scan_of_that_directory() {
    let mut tree = MemProvider::new("/t");
    tree.file("a/b/c.txt", 10)
        .file("a/d.txt", 20)
        .file("e.txt", 5);
    let mut whole = Arena::new(Attrs::Basic);
    walk_with(&tree, Path::new("/t"), &WalkOptions::default(), |e| {
        whole.push(e).unwrap();
    })
    .unwrap();
    let whole_rollup = roll_up(&mut whole);
    let whole_view = ArenaView::new(&whole, &whole_rollup, PathBuf::from("/t"));
    let a = whole_view.child_dirs(0)[0].0;
    let sub = Subtree::new(&whole_view, a);
    let mut alone = Arena::new(Attrs::Basic);
    walk_with(&tree, Path::new("/t/a"), &WalkOptions::default(), |e| {
        alone.push(e).unwrap();
    })
    .unwrap();
    let alone_rollup = roll_up(&mut alone);
    let alone_view = ArenaView::new(&alone, &alone_rollup, PathBuf::from("/t/a"));
    assert_same_views(&alone_view, &sub);
}

#[test]
fn p2_idx_03_another_major_version_is_refused() {
    let env = TestEnv::new();
    let dir = env.tmp_dir();
    let file = &dir.join("v3.db");
    let db = Connection::open(file).unwrap();
    db.execute_batch(
        "CREATE TABLE scan_meta (key TEXT PRIMARY KEY, value); \
         INSERT INTO scan_meta VALUES ('index_version', '3.0');",
    )
    .unwrap();
    drop(db);
    match Index::open(file) {
        Err(CoreError::IndexVersion { found, .. }) => assert_eq!(found, "3.0"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn p2_idx_03_a_file_that_is_not_an_index_is_corrupt() {
    let env = TestEnv::new();
    let dir = env.tmp_dir();
    let file = &dir.join("x.db");
    fs::write(file, b"not a database, not even close to one, at all").unwrap();
    assert!(matches!(
        Index::open(file),
        Err(CoreError::IndexCorrupt { .. })
    ));
    // A database without the tables.
    let empty = &dir.join("empty.db");
    drop(Connection::open(empty).unwrap());
    assert!(matches!(
        Index::open(empty),
        Err(CoreError::IndexCorrupt { .. })
    ));
}

/// Writes a small index of `tree` under `dir` with id `id`, started at
/// `started_at`.
fn small_index(dir: &Path, tree: &MemProvider, id: &str, started_at: i64) -> PathBuf {
    let mut arena = Arena::new(Attrs::Basic);
    walk_with(tree, tree.root_path(), &WalkOptions::default(), |e| {
        arena.push(e).unwrap();
    })
    .unwrap();
    let rollup = roll_up(&mut arena);
    let id = ScanId::parse(id).unwrap();
    let meta = meta(id.clone(), tree.root_path(), started_at);
    IndexWriter::write(&arena, &rollup, &meta, dir).unwrap();
    index_path(dir, &id)
}

#[test]
fn p2_idx_03_a_path_with_uri_characters_opens() {
    let env = TestEnv::new();
    let dir = env.tmp_dir();
    let odd = &dir.join("a ?b#c%20d&mode=rw");
    let mut tree = MemProvider::new("/t");
    tree.file("x", 1);
    let file = small_index(odd, &tree, "abcdef012345", 1);
    let index = Index::open(&file).unwrap();
    assert_eq!(index.meta().scan_id.as_str(), "abcdef012345");
    assert_eq!(index.len(), 2);
}

#[test]
fn p2_idx_03_selectors_read_as_latest_a_prefix_or_a_path() {
    assert_eq!(ScanSelector::parse("latest").unwrap(), ScanSelector::Latest);
    assert_eq!(
        ScanSelector::parse("a3f9").unwrap(),
        ScanSelector::Prefix("a3f9".to_owned())
    );
    assert_eq!(
        ScanSelector::parse("a3f9c0d1e2f3").unwrap(),
        ScanSelector::Prefix("a3f9c0d1e2f3".to_owned())
    );
    for path in ["./x.db", "x.db", "/tmp/scans/a3f9", "../a"] {
        assert_eq!(
            ScanSelector::parse(path).unwrap(),
            ScanSelector::File(PathBuf::from(path))
        );
    }
    // Too short, too long, not lowercase hex, or a word: none is a
    // selector.
    for bad in ["a3f", "a3f9c0d1e2f3a", "A3F9", "a3g9", "newest", ""] {
        assert!(
            matches!(ScanSelector::parse(bad), Err(CoreError::Selector(_))),
            "{bad:?}"
        );
    }
}

fn q<'a>(dir: &'a Path, path: Option<&'a Path>, cwd: &'a Path, now: i64) -> ScanQuery<'a> {
    ScanQuery {
        index_dir: dir,
        path,
        cwd,
        max_age_ns: 7 * DAY,
        now,
        trust: &[],
    }
}

#[test]
fn p2_idx_03_a_prefix_names_one_scan() {
    let env = TestEnv::new();
    let dir = env.tmp_dir();
    let mut tree = MemProvider::new("/t");
    tree.file("sub/x", 1).file("y", 2);
    small_index(&dir, &tree, "a3f9c0000001", 1);
    small_index(&dir, &tree, "a3f9c0000002", 2);
    small_index(&dir, &tree, "b1000000000a", 3);
    let cwd = Path::new("/elsewhere");
    let found = |sel: &str, path: Option<&Path>| {
        resolve_scan(
            &ScanSelector::parse(sel).unwrap(),
            &q(&dir, path, cwd, 4),
            |_| true,
        )
    };
    // Unique: the root when no path is given, wherever the command runs.
    let r = found("b100", None).unwrap().unwrap();
    assert_eq!(r.index.meta().scan_id.as_str(), "b1000000000a");
    assert_eq!(r.base, 0);
    // A path inside the root re-roots at it.
    let r = found("a3f9c0000002", Some(Path::new("/t/sub")))
        .unwrap()
        .unwrap();
    assert_eq!(r.index.path(r.base), b"sub");
    // Ambiguous, unknown, and a path outside the root.
    assert!(
        matches!(found("a3f9", None), Err(CoreError::Selector(m)) if m.contains("2 saved scans"))
    );
    assert!(matches!(
        found("cccc", None),
        Err(CoreError::ScanNotFound(_))
    ));
    assert!(matches!(
        found("b100", Some(Path::new("/u"))),
        Err(CoreError::ScanNotFound(m)) if m.contains("is not in scan b1000000000a")
    ));
    assert!(matches!(
        found("b100", Some(Path::new("/t/missing"))),
        Err(CoreError::ScanNotFound(_))
    ));
}

#[test]
fn p2_idx_03_a_path_names_an_index_file() {
    let env = TestEnv::new();
    let dir = env.tmp_dir();
    let mut tree = MemProvider::new("/t");
    tree.file("x", 1);
    let file = small_index(&dir, &tree, "c0ffee000000", 1);
    let cwd = file.parent().unwrap();
    let sel = ScanSelector::parse("./c0ffee000000.db").unwrap();
    let r = resolve_scan(&sel, &q(&dir, None, cwd, 2), |_| true)
        .unwrap()
        .unwrap();
    assert_eq!(r.index.meta().scan_id.as_str(), "c0ffee000000");
    let missing = ScanSelector::parse("./nothing.db").unwrap();
    assert!(matches!(
        resolve_scan(&missing, &q(&dir, None, cwd, 2), |_| true),
        Err(CoreError::ScanNotFound(_))
    ));
}

#[test]
fn p2_idx_03_latest_is_the_newest_usable_scan_holding_the_path() {
    let env = TestEnv::new();
    let dir = env.tmp_dir();
    let mut whole = MemProvider::new("/t");
    whole.file("sub/x", 1).file("y", 2);
    let mut sub = MemProvider::new("/t/sub");
    sub.file("x", 1);
    let mut other = MemProvider::new("/u");
    other.file("z", 1);
    small_index(&dir, &whole, "100000000000", 10 * DAY);
    small_index(&dir, &sub, "200000000000", 11 * DAY);
    small_index(&dir, &other, "300000000000", 12 * DAY);
    let latest = |path: &Path, now: i64, usable: &dyn Fn(&Index) -> bool| {
        resolve_scan(
            &ScanSelector::Latest,
            &q(&dir, Some(path), Path::new("/"), now),
            usable,
        )
        .unwrap()
    };
    let all = |_: &Index| true;
    // /t/sub is in both scans of /t: the newer one, rooted at it, wins.
    let r = latest(Path::new("/t/sub"), 12 * DAY, &all).unwrap();
    assert_eq!(r.index.meta().scan_id.as_str(), "200000000000");
    assert_eq!((r.base, r.stale), (0, false));
    // /t itself only in the older one, re-rooted nowhere.
    let r = latest(Path::new("/t"), 12 * DAY, &all).unwrap();
    assert_eq!(r.index.meta().scan_id.as_str(), "100000000000");
    // A scan the verb cannot use is passed over.
    let not_sub = |i: &Index| i.meta().scan_id.as_str() != "200000000000";
    let r = latest(Path::new("/t/sub"), 12 * DAY, &not_sub).unwrap();
    assert_eq!(r.index.meta().scan_id.as_str(), "100000000000");
    assert_eq!(r.index.path(r.base), b"sub");
    // Older than --max-age: still read, marked stale.
    let r = latest(Path::new("/t/sub"), 30 * DAY, &all).unwrap();
    assert!(r.stale);
    // A file in a scan is read too, found in its directory's id range.
    let r = latest(Path::new("/t/y"), 12 * DAY, &all).unwrap();
    assert_eq!(&*r.index.name(r.base), b"y");
    assert!(!r.index.node(r.base).is_dir());
    // No scan holds the path: none, so the verb scans.
    assert!(latest(Path::new("/v"), 12 * DAY, &all).is_none());
    assert!(latest(Path::new("/t/sub/new"), 12 * DAY, &all).is_none());
    // Without a path, the working directory.
    let r = resolve_scan(
        &ScanSelector::Latest,
        &q(&dir, None, Path::new("/u"), 12 * DAY),
        all,
    )
    .unwrap()
    .unwrap();
    assert_eq!(r.index.meta().scan_id.as_str(), "300000000000");
}

/// A copy of a small index, changed by `sql` through a connection that may
/// write anything, as whoever made a hostile file could.
fn hostile(dir: &Path, name: &str, sql: &str) -> PathBuf {
    let mut tree = MemProvider::new("/t");
    tree.file("x", 1);
    let file = small_index(&dir.join(name), &tree, "abcdef012345", 1);
    let db = Connection::open(&file).unwrap();
    db.execute_batch(sql).unwrap();
    drop(db);
    file
}

fn refused(file: &Path) -> String {
    match Index::open(file) {
        Err(CoreError::IndexCorrupt { message, .. }) => message,
        other => panic!("{}: {other:?}", file.display()),
    }
}

#[test]
fn p2_sec_01_a_hostile_index_is_refused_or_neutralised() {
    let env = TestEnv::new();
    let dir = env.tmp_dir();
    // Views and triggers are switched off: the file opens, reads as the
    // index it was, and its views cannot be read.
    let file = hostile(
        &dir,
        "views",
        "CREATE VIEW peek AS SELECT sql FROM sqlite_schema; \
         CREATE TRIGGER t AFTER INSERT ON scan_meta BEGIN DELETE FROM dirs; END;",
    );
    let index = Index::open(&file).unwrap();
    assert_eq!(index.len(), 2);
    assert!(index.failure().is_none());
    let read = index
        .db()
        .query_row("SELECT count(*) FROM peek", [], |row| row.get::<_, i64>(0));
    assert!(
        read.unwrap_err().to_string().contains("prohibited"),
        "a view reads"
    );
    // A table the reader reads, made a view: refused, not followed.
    let file = hostile(
        &dir,
        "meta-view",
        "ALTER TABLE scan_meta RENAME TO real_meta; \
         CREATE VIEW scan_meta AS SELECT * FROM real_meta;",
    );
    assert!(refused(&file).contains("prohibited"));
    // A virtual table, however its statement is spelled: refused before
    // anything connects it. The bundled SQLite has no virtual table
    // modules to make one with (a system SQLite has), so the schema's row
    // is written as a hostile file's would be.
    for (name, sql) in [
        ("virtual", "CREATE VIRTUAL TABLE v USING rtree(id, a, b)"),
        (
            "virtual-spelled",
            "create/* a */ -- b\n\tVirtual table v using rtree(id, a, b)",
        ),
    ] {
        let file = hostile(
            &dir,
            name,
            &format!(
                "PRAGMA writable_schema = ON; \
                 INSERT INTO sqlite_schema VALUES ('table', 'v', 'v', 0, '{sql}');"
            ),
        );
        assert_eq!(
            refused(&file),
            "its schema has a virtual table, v",
            "{name}"
        );
    }
    // A large schema is refused once counted, a huge one while it loads.
    let mut tables = String::from("BEGIN;");
    for n in 0..300 {
        write!(tables, " CREATE TABLE t{n} (x);").unwrap();
    }
    tables.push_str(" COMMIT;");
    let file = hostile(&dir, "large", &tables);
    assert_eq!(refused(&file), "its schema has more than 256 objects");
    // 200,000 views, written as rows (made one by one, views take time
    // quadratic in their number).
    let file = hostile(
        &dir,
        "huge",
        "PRAGMA writable_schema = ON; \
         WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i + 1 FROM n WHERE i < 199999) \
         INSERT INTO sqlite_schema \
         SELECT 'view', 'v' || i, 'v' || i, 0, 'CREATE VIEW v' || i || ' AS SELECT ' || i FROM n;",
    );
    let started = std::time::Instant::now();
    assert_eq!(refused(&file), "its schema has more than 256 objects");
    let stopped = started.elapsed();
    let all = Connection::open(&file).unwrap();
    let started = std::time::Instant::now();
    all.query_row("SELECT count(*) FROM sqlite_schema", [], |_| Ok(()))
        .unwrap();
    let loaded = started.elapsed();
    assert!(
        stopped * 10 < loaded,
        "stopping took {stopped:?}, loading the whole schema {loaded:?}"
    );
    // One statement past the parser's limit.
    let long = format!(
        "CREATE TABLE long (x DEFAULT '{}');",
        "x".repeat(usize::try_from(super::SQL_LENGTH).unwrap())
    );
    let file = hostile(&dir, "long", &long);
    assert!(refused(&file).contains("too big"), "{}", refused(&file));
    // A value past the reader's limit reads as a damaged entry.
    let file = hostile(
        &dir,
        "big-name",
        &format!(
            "UPDATE files SET name = zeroblob({}) WHERE id = 1;",
            super::VALUE_LENGTH + 1
        ),
    );
    let index = Index::open(&file).unwrap();
    let _ = index.node(1);
    assert!(index.failure().unwrap().contains("too big"));
    // tool_version names a duscope version.
    for (name, value) in [("no-tool", None), ("odd-tool", Some("evil"))] {
        let sql = match value {
            None => "DELETE FROM scan_meta WHERE key = 'tool_version';".to_owned(),
            Some(v) => format!("UPDATE scan_meta SET value = '{v}' WHERE key = 'tool_version';"),
        };
        let file = hostile(&dir, name, &sql);
        assert!(refused(&file).contains("tool_version"), "{name}");
    }
}

#[test]
fn p2_sec_01_virtual_tables_are_told_from_their_text() {
    for sql in [
        "CREATE VIRTUAL TABLE v USING rtree(a)",
        "create virtual table v using fts5(a)",
        "CREATE/**/VIRTUAL TABLE v USING x",
        "CREATE -- c\n VIRTUAL TABLE v USING x",
        "CREATE\t\r\n\x0cVIRTUAL TABLE v USING x",
    ] {
        assert!(super::creates_virtual_table(sql), "{sql:?}");
    }
    for sql in [
        "CREATE TABLE virtual (x)",
        "CREATE TABLE v (virtual)",
        "CREATE VIRTUALS TABLE v",
        "CREATEVIRTUAL TABLE v",
        "CREATE /* VIRTUAL */ TABLE v (x)",
        "CREATE -- VIRTUAL\n TABLE v (x)",
        "CREATE",
        "",
    ] {
        assert!(!super::creates_virtual_table(sql), "{sql:?}");
    }
}

#[test]
fn p2_sec_01_tool_versions_are_versions() {
    for good in ["0.2.0", "1.10.3", "0.2.0-rc.1", "0.0.0+abc"] {
        assert!(super::is_version(good), "{good}");
    }
    for bad in ["", "0.2", "0.2.0.1", "v0.2.0", "0..1", "a.b.c", "0.2.x"] {
        assert!(!super::is_version(bad), "{bad}");
    }
}
