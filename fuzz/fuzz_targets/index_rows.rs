//! P5-SEC-04: a saved index whose rows say anything about where an entry
//! sits. The index of a small tree is written once per process; each input
//! is a list of changes to a copy of it, `UPDATE dirs SET {parent_id,
//! last_id, files_end, depth, id} = v WHERE id = k`, `UPDATE files SET
//! {parent_id, id} = v WHERE id = k` and `n_entries` set to `v`. The copy
//! is opened as any saved scan is (a count past the highest id + 1 is
//! refused, `index_corrupt`),
//! and every entry's path, every directory's subdirectories, `top` (files,
//! with ties walked in path order and in every id's order by `--sort
//! mtime`, and directories), `tree` of the whole scan and of each
//! directory's subtree, `render` of each directory's subtree, and the
//! incremental rescan's load of the saved directories must answer, or
//! report the damage, without panicking or reading for ever. Each opens the
//! copy afresh, since an index reads nothing more once a read has failed.
//! A walk down the tree reads each entry once: `tree` lists the children of
//! at most as many directories as the scan has entries.

#![no_main]

use std::borrow::Cow;
use std::cell::Cell;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use duscope_core::CoreError;
use duscope_core::arena::Arena;
use duscope_core::index::{Index, IndexWriter, ScanId, ScanMeta, index_path};
use duscope_core::query::{
    self, Dataless, Node, SizeKind, SortKey, Subtree, TopQuery, TreeQuery, TreeView, VolumeRoot,
};
use duscope_core::rescan::{self, Checks, History, MAX_AGE_NS, Stop};
use duscope_core::rollup::roll_up;
use duscope_core::rules::Classes;
use duscope_treemap::Algorithm;
use duscope_treemap::packs::Pack;
use duscope_treemap::paint::{ColorBy, Glyphs, PaintOpts};
use duscope_treemap::picture::PictureOpts;
use duscope_treemap::svg::write_svg;
use duscope_walk::fsevents::Replay;
use duscope_walk::{
    Attrs, FsEventsError, FsEventsMark, MemProvider, ScanEvent, WalkOptions, walk_with,
};
use libfuzzer_sys::fuzz_target;
use rusqlite::Connection;

/// The columns an input changes, by table: where an entry sits, its id,
/// and the count of entries `scan_meta` records.
const COLUMNS: [(&str, &str); 8] = [
    ("dirs", "parent_id"),
    ("dirs", "last_id"),
    ("dirs", "files_end"),
    ("dirs", "depth"),
    ("files", "parent_id"),
    ("dirs", "id"),
    ("files", "id"),
    ("scan_meta", "n_entries"),
];

/// The version the fixture's `scan_meta` names, which the rescan's checks
/// compare with their own.
const TOOL_VERSION: &str = "0.0.0";

/// The fixture's index, the ids of its two tables, and the file each input
/// is copied to.
struct Fixture {
    file: PathBuf,
    copy: PathBuf,
    dirs: Vec<i64>,
    files: Vec<i64>,
    meta: ScanMeta,
}

/// The mark the fixture's volume carries, so the rescan's checks reach the
/// saved directories.
fn mark() -> FsEventsMark {
    FsEventsMark {
        uuid: "5A1E0000-0000-0000-0000-000000000000".to_owned(),
        event_id: 1,
        os_build: "25G332".to_owned(),
    }
}

fn fixture() -> &'static Fixture {
    static FIXTURE: OnceLock<Fixture> = OnceLock::new();
    FIXTURE.get_or_init(|| {
        let mut tree = MemProvider::new("/r");
        tree.file("top.txt", 5)
            .file("a/x.txt", 10)
            .file("a/y.bin", 4096)
            .file("a/d/z.txt", 10)
            .file("b/c/w.txt", 1 << 20)
            .file(Path::new(OsStr::from_bytes(b"b/\xff.bin")), 7)
            .link("a/x-link.txt", "a/x.txt")
            .symlink("b/s", 3)
            .dir("empty");
        let mut arena = Arena::new(Attrs::Basic);
        walk_with(&tree, tree.root_path(), &WalkOptions::default(), |mut e| {
            if let ScanEvent::Volume(volume) = &mut e {
                volume.fsevents = volume.entered().then(mark);
            }
            arena.push(e).expect("the tree fits the arena");
        })
        .expect("the tree in memory reads");
        let rollup = roll_up(&mut arena);
        let root = tree.root_path().as_os_str().as_bytes().to_vec();
        let meta = ScanMeta {
            scan_id: ScanId::new(b"fuzz", 1, [0; 8]),
            root: root.clone(),
            root_as_given: root,
            started_at: 1,
            finished_at: 2,
            options: "{}".to_owned(),
            platform: "fuzz".to_owned(),
            scanning_uid: 0,
            thread_count: 1,
            size_kind: "allocated",
            volume_total: 1 << 30,
            volume_free: 1 << 29,
            unaccounted_bytes: None,
            error_count: 0,
            vanished_count: 0,
            excluded_count: 0,
            clones_unavailable: false,
            incremental: false,
            parent_scan_id: None,
            source: "scan".to_owned(),
            tool_version: TOOL_VERSION.to_owned(),
        };
        let dir =
            std::env::temp_dir().join(format!("duscope-fuzz-index-rows-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("the temporary directory is writable");
        IndexWriter::write(&arena, &rollup, &meta, &dir).expect("the index writes");
        let file = index_path(&dir, &meta.scan_id);
        let db = Connection::open(&file).expect("the index opens");
        let ids = |table: &str| -> Vec<i64> {
            db.prepare(&format!("SELECT id FROM {table} ORDER BY id"))
                .and_then(|mut stmt| stmt.query_map([], |row| row.get(0))?.collect())
                .expect("the ids read")
        };
        let (dirs, files) = (ids("dirs"), ids("files"));
        drop(db);
        Fixture {
            copy: dir.join("input.db"),
            file,
            dirs,
            files,
            meta,
        }
    })
}

/// A value for a column from an input's two bytes: near the fixture's ids
/// (a few below zero, a few past its entries) most often, near `u32::MAX`,
/// past what a `u32` holds, or NULL.
fn value(kind: u8, v: u8) -> Option<i64> {
    match kind >> 5 {
        0..=4 => Some(i64::from(v) - 4),
        5 => Some(i64::from(u32::MAX) - i64::from(v)),
        6 => Some(i64::from(u32::MAX) + 1 + i64::from(v)),
        _ => None,
    }
}

/// A view that counts the directories a walk lists the children of.
struct Counted<'v> {
    view: &'v dyn TreeView,
    listed: Cell<u64>,
}

impl TreeView for Counted<'_> {
    fn len(&self) -> u32 {
        self.view.len()
    }

    fn node(&self, id: u32) -> Node {
        self.view.node(id)
    }

    fn root_path(&self) -> &Path {
        self.view.root_path()
    }

    fn full_attrs(&self) -> bool {
        self.view.full_attrs()
    }

    fn name(&self, id: u32) -> Cow<'_, [u8]> {
        self.view.name(id)
    }

    fn path(&self, id: u32) -> Vec<u8> {
        self.view.path(id)
    }

    fn child_dirs(&self, dir: u32) -> Vec<(u32, u32)> {
        self.listed.set(self.listed.get() + 1);
        self.view.child_dirs(dir)
    }

    fn as_index(&self) -> Option<(&Index, u32)> {
        self.view.as_index()
    }

    fn volume_roots(&self, dir: u32) -> Vec<VolumeRoot> {
        self.view.volume_roots(dir)
    }

    fn subvolume_roots(&self, dir: u32) -> Vec<VolumeRoot> {
        self.view.subvolume_roots(dir)
    }

    fn owner(&self, id: u32) -> Option<(u32, u32)> {
        self.view.owner(id)
    }

    fn dataless(&self, dir: u32) -> Dataless {
        self.view.dataless(dir)
    }
}

/// `tree` of `view`, as deep as it goes, which must list the children of
/// no more directories than `view` has entries.
fn tree(view: &dyn TreeView) {
    let counted = Counted {
        view,
        listed: Cell::new(0),
    };
    query::tree(
        &counted,
        &TreeQuery {
            depth: Some(u32::MAX),
            ..TreeQuery::default()
        },
    );
    assert!(
        counted.listed.get() <= u64::from(view.len()),
        "tree listed the children of {} directories in a view of {} entries",
        counted.listed.get(),
        view.len()
    );
}

/// The picture `render` draws of `view`, written nowhere.
fn render(view: &dyn TreeView) {
    static CLASSES: OnceLock<Classes> = OnceLock::new();
    let pack = Pack::mocha();
    let opts = PictureOpts {
        paint: PaintOpts {
            pack: &pack,
            color_by: ColorBy::Type,
            glyphs: Glyphs::Unicode,
            depth: None,
            min_tile: 1,
            pool_files: false,
            size: SizeKind::Allocated,
            now: 0,
            classes: CLASSES.get_or_init(Classes::vendored),
        },
        algorithm: Algorithm::Squarify,
        width: 320,
        height: 200,
        title: String::new(),
        legend: Vec::new(),
        summary: String::new(),
    };
    write_svg(view, &opts, &mut io::sink()).expect("a sink takes every byte");
}

/// A history that reports one directory changed and one subtree to read
/// whole, as `FSEvents` would.
struct Script;

impl History for Script {
    fn now(&self) -> Option<u64> {
        Some(2)
    }

    fn replay(
        &self,
        _dev: u64,
        _mark: &FsEventsMark,
        under: &Path,
    ) -> Result<Replay, FsEventsError> {
        Ok(Replay {
            root: under.to_owned(),
            dirs: vec![under.join("a")],
            must_scan_subdirs: vec![under.join("b")],
            history_done: true,
            ..Replay::default()
        })
    }

    fn mark(&self, _dev: u64, now: u64) -> Option<FsEventsMark> {
        Some(FsEventsMark {
            event_id: now,
            ..mark()
        })
    }

    fn reusable(&self, _fs_type: &str) -> bool {
        true
    }
}

fuzz_target!(|data: &[u8]| {
    let fixture = fixture();
    fs::copy(&fixture.file, &fixture.copy).expect("the index copies");
    let db = Connection::open(&fixture.copy).expect("the copy opens");
    for change in data.chunks_exact(3) {
        let (table, column) = COLUMNS[usize::from(change[0]) % COLUMNS.len()];
        let value = value(change[0], change[2]);
        let ids = match table {
            "dirs" => &fixture.dirs,
            "files" => &fixture.files,
            _ => {
                let _ = db.execute(
                    "UPDATE scan_meta SET value = ?1 WHERE key = 'n_entries'",
                    [value],
                );
                continue;
            }
        };
        let id = ids[usize::from(change[1]) % ids.len()];
        // A change a column's constraint refuses (NULL where it may not be,
        // an id another row has) is not made.
        let _ = db.execute(
            &format!("UPDATE {table} SET {column} = ?1 WHERE id = ?2"),
            rusqlite::params![value, id],
        );
    }
    drop(db);
    match Index::open(&fixture.copy) {
        Ok(_) | Err(CoreError::IndexCorrupt { .. }) => {}
        Err(other) => panic!("{other:?}"),
    }
    let Ok(index) = Index::open(&fixture.copy) else {
        return;
    };
    let open = || Index::open(&fixture.copy).expect("the copy opened before");
    for id in query::ids(&index, 0, index.len().saturating_sub(1)) {
        index.path(id);
        index.child_dirs(id);
    }
    for files in [
        TopQuery {
            files: true,
            limit: Some(2),
            ..TopQuery::default()
        },
        TopQuery {
            files: true,
            sort: SortKey::Mtime,
            limit: Some(2),
            ..TopQuery::default()
        },
        TopQuery::default(),
    ] {
        query::top(&open(), &files);
    }
    tree(&open());
    // Each directory's subtree, as a verb asked about a path below the
    // root reads it.
    for &dir in &fixture.dirs {
        let Ok(dir) = u32::try_from(dir) else {
            continue;
        };
        let index = open();
        tree(&Subtree::new(&index, dir));
        let index = open();
        render(&Subtree::new(&index, dir));
    }
    let index = open();
    let checks = Checks {
        now_ns: fixture.meta.started_at,
        max_age_ns: MAX_AGE_NS,
        tool_version: TOOL_VERSION,
    };
    match rescan::plan(&index, &checks, &Script) {
        Ok(_) | Err(Stop::Fallback(..) | Stop::Index(CoreError::IndexCorrupt { .. })) => {}
        Err(other) => panic!("{other:?}"),
    }
});
