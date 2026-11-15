//! P5-WALK-06: an automount point the walk did not enter is flagged on its
//! directory (bit 14, `automount`), in the arena and in a saved scan, which
//! count such points under a path for the `mounts_excluded` warning and read
//! them as not entered; an ncdu export marks one as ncdu marks a filesystem
//! it did not enter.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use duscope_core::arena::Arena;
use duscope_core::index::{AUTOMOUNT, Index, IndexWriter, ScanId, ScanMeta, index_path};
use duscope_core::interchange::import::{ImportFormat, import};
use duscope_core::interchange::ncdu::{self, NcduOpts};
use duscope_core::query::{ArenaView, Subtree, TreeView, flag_names};
use duscope_core::rollup::roll_up;
use duscope_testkit::TestEnv;
use duscope_walk::{
    Boundary, DirHandle, Entry, Flags, Kind, MemProvider, Provider, ScanError, Skip, VolumeInfo,
    WalkOptions, walk_with,
};

/// The tree, whose directories named `auto` read as Linux reads an
/// automount point not yet triggered: a mount boundary in the mount around
/// it, no mount root.
struct Points<'a>(&'a MemProvider);

impl Provider for Points<'_> {
    fn read_dir(
        &self,
        dir: &DirHandle,
        opts: &WalkOptions,
        out: &mut Vec<Entry>,
        errs: &mut Vec<ScanError>,
    ) -> io::Result<()> {
        let start = out.len();
        self.0.read_dir(dir, opts, out, errs)?;
        for entry in &mut out[start..] {
            if entry.name.as_bytes() == b"auto" {
                entry.boundary = Boundary::Mount;
            }
        }
        Ok(())
    }

    fn untriggered_automount(&self, entry: &Entry) -> bool {
        entry.boundary == Boundary::Mount && !entry.flags.contains(Flags::MOUNT_ROOT)
    }

    fn volume(&self, path: &Path) -> io::Result<VolumeInfo> {
        self.0.volume(path)
    }

    fn root(&self, path: &Path, opts: &WalkOptions) -> io::Result<Entry> {
        self.0.root(path, opts)
    }
}

fn meta(root: &Path) -> ScanMeta {
    ScanMeta {
        scan_id: ScanId::new(b"automount", 1, [0; 8]),
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

/// The id of the entry at `rel` below the root of `view`.
fn id_of(view: &impl TreeView, rel: &str) -> u32 {
    (0..view.len())
        .find(|&id| view.path(id) == rel.as_bytes())
        .unwrap_or_else(|| panic!("no {rel}"))
}

#[test]
fn p5_walk_06_a_skipped_automount_point_is_flagged_and_counted() {
    let env = TestEnv::new();
    let mut tree = MemProvider::new("/r");
    tree.file("auto/inside", 4096)
        .file("a/b/auto/inside", 4096)
        .file("a/plain/x", 4096)
        .file("c/y", 4096);
    let provider = Points(&tree);
    let mut arena = Arena::new(duscope_walk::Attrs::Basic);
    walk_with(&provider, tree.root_path(), &WalkOptions::default(), |e| {
        arena.push(e).unwrap();
    })
    .unwrap();
    let rollup = roll_up(&mut arena);
    let fresh = ArenaView::new(&arena, &rollup, PathBuf::from("/r"));
    let meta = meta(tree.root_path());
    let dir = env.tmp_dir();
    IndexWriter::write(&arena, &rollup, &meta, &dir).unwrap();
    let file = index_path(&dir, &meta.scan_id);
    let saved = Index::open(&file).unwrap();
    // The same file, as a build before `dirs_automount` wrote it.
    let older = dir.join("older.db");
    fs::copy(&file, &older).unwrap();
    let db = rusqlite::Connection::open(&older).unwrap();
    db.execute_batch("DROP INDEX dirs_automount;").unwrap();
    drop(db);
    let without = Index::open(&older).unwrap();
    let rows = rusqlite::Connection::open(&file).unwrap();

    for (rel, points) in [("", 2), ("a", 1), ("a/b/auto", 1), ("a/plain", 0), ("c", 0)] {
        assert_eq!(
            fresh.automount_points(id_of(&fresh, rel)),
            points,
            "{rel:?}"
        );
        assert_eq!(
            saved.automount_points(id_of(&saved, rel)),
            points,
            "{rel:?}"
        );
        assert_eq!(
            without.automount_points(id_of(&saved, rel)),
            points,
            "{rel:?}"
        );
        // A saved scan read at a path counts what is under it.
        let at = Subtree::new(&saved, id_of(&saved, rel));
        assert_eq!(at.automount_points(0), points, "{rel:?}");
    }
    for (rel, point) in [("auto", true), ("a/b/auto", true), ("a/plain", false)] {
        let (arena_node, saved_id) = (fresh.node(id_of(&fresh, rel)), id_of(&saved, rel));
        let saved_node = saved.node(saved_id);
        assert_eq!(arena_node.automount, point, "{rel}");
        assert_eq!(saved_node.automount, point, "{rel}");
        assert_eq!(
            flag_names(&saved_node).contains(&"automount"),
            point,
            "{rel}"
        );
        // Listed and not entered: nothing under it was read.
        assert_eq!(saved.entered(saved_id), !point, "{rel}");
        let bits: u32 = rows
            .query_row("SELECT flags FROM dirs WHERE id = ?1", [saved_id], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(bits & AUTOMOUNT != 0, point, "{rel}: flags {bits}");
        if point {
            assert_eq!(saved_node.file_count, 0, "{rel}");
        }
    }
    // No volume of its own: the mount it carries is the root's.
    assert_eq!(saved.volumes().len(), 1);
    assert!(saved.failure().is_none() && without.failure().is_none());

    // Entered under --include-network, a point is a plain directory.
    let opts = WalkOptions {
        include_network: true,
        ..WalkOptions::default()
    };
    let mut arena = Arena::new(duscope_walk::Attrs::Basic);
    walk_with(&provider, tree.root_path(), &opts, |e| {
        arena.push(e).unwrap();
    })
    .unwrap();
    let rollup = roll_up(&mut arena);
    let entered = ArenaView::new(&arena, &rollup, PathBuf::from("/r"));
    assert_eq!(entered.automount_points(0), 0);
    assert!(!entered.node(id_of(&entered, "auto")).automount);
}

/// The line of `export` that holds the entry named `name`.
fn line<'a>(export: &'a str, name: &str) -> &'a str {
    let key = format!("{{\"name\":\"{name}\"");
    export
        .lines()
        .find(|line| line.contains(&key))
        .unwrap_or_else(|| panic!("no {name} in {export}"))
}

#[test]
fn p5_walk_06_an_ncdu_export_marks_a_skipped_automount_point_otherfs() {
    let env = TestEnv::new();
    let mut tree = MemProvider::new("/r");
    tree.file("auto/inside", 4096).file("a/plain/x", 4096);
    let provider = Points(&tree);
    let mut arena = Arena::new(duscope_walk::Attrs::Basic);
    walk_with(&provider, tree.root_path(), &WalkOptions::default(), |e| {
        arena.push(e).unwrap();
    })
    .unwrap();
    let rollup = roll_up(&mut arena);
    let meta = meta(tree.root_path());
    let dir = env.tmp_dir().join("scan");
    fs::create_dir_all(&dir).unwrap();
    IndexWriter::write(&arena, &rollup, &meta, &dir).unwrap();
    let saved = Index::open(&index_path(&dir, &meta.scan_id)).unwrap();
    let mut out = Vec::new();
    let opts = NcduOpts {
        progver: "0.0.0",
        timestamp: 1,
    };
    ncdu::write(&saved, 0, &opts, &mut out).unwrap();
    let export = String::from_utf8(out).unwrap();

    // The point, as ncdu writes a filesystem it did not enter: an info
    // object, not a directory's array, marked otherfs.
    let auto = line(&export, "auto");
    assert!(auto.starts_with('{'), "{auto}");
    assert!(auto.contains(r#""excluded":"otherfs""#), "{auto}");
    // A directory the scan entered is an array, unmarked.
    let plain = line(&export, "plain");
    assert!(plain.starts_with('['), "{plain}");
    assert!(!plain.contains("excluded"), "{plain}");

    // Imported again, the point is a directory listed and not entered for
    // a mount's reason, with the sizes the scan gave it.
    let imported = import(export.as_bytes(), Some(ImportFormat::Ncdu)).unwrap();
    assert_eq!((imported.skipped_dirs, imported.mounts_excluded), (1, 1));
    let skipped: Vec<Skip> = imported.arena.skipped().values().copied().collect();
    assert_eq!(skipped, [Skip::OneFileSystem]);
    let mut back = imported.arena;
    let rollup = roll_up(&mut back);
    let dir = env.tmp_dir().join("back");
    fs::create_dir_all(&dir).unwrap();
    IndexWriter::write(&back, &rollup, &meta, &dir).unwrap();
    let back = Index::open(&index_path(&dir, &meta.scan_id)).unwrap();
    let (was, now) = (id_of(&saved, "auto"), id_of(&back, "auto"));
    let (was, now) = (saved.node(was), back.node(now));
    assert_eq!(now.kind, Kind::Dir);
    assert_eq!(
        (now.apparent, now.allocated, now.file_count, now.dir_count),
        (was.apparent, was.allocated, was.file_count, was.dir_count)
    );
    assert_eq!(back.node(0).allocated, saved.node(0).allocated);
}
