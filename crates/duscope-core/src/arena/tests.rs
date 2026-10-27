//! P1-CORE-01: the arena's record sizes, what it holds, and its memory.

use std::mem::size_of;
use std::path::PathBuf;

use duscope_testkit::TestEnv;
use duscope_walk::{
    Attrs, Boundary, DirId, Entry, Flags, FullAttrs, Kind, NameRef, ScanEvent, Share, WalkOptions,
    walk,
};

use super::{Arena, CHUNK, Chunked, DirRec, EntryIdx, EntryRec, SharedRec};

#[test]
fn p1_core_01_records_are_48_bytes() {
    assert_eq!(size_of::<EntryRec>(), 48);
    assert_eq!(size_of::<DirRec>(), 48);
    assert_eq!(size_of::<EntryIdx>(), 4);
    // A hard-linked or cloned file's identity beside its record.
    assert_eq!(size_of::<SharedRec>(), 16);
}

fn walk_into(root: &std::path::Path, attrs: Attrs) -> (Arena, Vec<Entry>) {
    let mut arena = Arena::new(attrs);
    let mut entries = Vec::new();
    let opts = WalkOptions {
        attrs,
        ..WalkOptions::default()
    };
    walk(root, &opts, |event| {
        if let ScanEvent::Entry(entry) = &event {
            entries.push(entry.clone());
        }
        arena.push(event).unwrap();
    })
    .unwrap();
    (arena, entries)
}

#[test]
fn p1_core_01_the_arena_holds_what_the_walk_delivered() {
    let env = TestEnv::new();
    let root = env.fixtures().root();
    for attrs in [Attrs::Basic, Attrs::Full] {
        let (arena, entries) = walk_into(&root, attrs);
        assert_eq!(arena.len(), entries.len());
        assert_eq!(
            arena.dir_count(),
            entries.iter().filter(|e| e.kind == Kind::Dir).count()
        );
        for (view, entry) in arena.entries().zip(&entries) {
            assert_eq!(view.name, entry.name.as_bytes());
            assert_eq!(view.kind, entry.kind);
            assert_eq!(
                (view.apparent, view.allocated),
                (entry.apparent, entry.allocated)
            );
            assert_eq!(
                (view.mtime_ns, view.ctime_ns),
                (entry.mtime_ns, entry.ctime_ns)
            );
            assert_eq!(view.flags, entry.flags);
            assert_eq!((view.parent, view.dir), (entry.parent, entry.dir));
            if entry.kind != Kind::Dir {
                assert_eq!(view.nlink, entry.nlink);
            }
            assert_eq!(arena.full(view.idx), entry.full);
            if let Some(dir) = entry.dir {
                assert_eq!(arena.dir(dir).entry, view.idx.get());
            }
        }
        // Hard-linked files keep their identity beside the record.
        let linked = entries
            .iter()
            .filter(|e| e.kind != Kind::Dir && e.nlink > 1)
            .count();
        assert_eq!(arena.linked_len(), linked);
        assert_eq!(arena.linked().count(), linked);
        assert!(linked >= 4, "the fixture's hard-links case");
        for link in arena.linked() {
            let entry = &entries[link.entry.index()];
            assert_eq!((link.dev, link.ino), (entry.dev, entry.ino));
        }
        let clones = entries
            .iter()
            .filter(|e| matches!(e.share, Share::Clone(_)))
            .count();
        assert_eq!(arena.cloned_len(), clones);
        for (i, clone) in arena.cloned().enumerate() {
            assert_eq!(arena.cloned_at(i), clone);
            let entry = &entries[clone.entry.index()];
            assert_eq!(
                (clone.dev, Share::Clone(clone.clone_id)),
                (entry.dev, entry.share)
            );
        }
        assert_eq!(
            arena
                .volumes()
                .first()
                .map(duscope_walk::VolumeInfo::entered),
            Some(true)
        );
        assert_eq!(arena.root_volume(), (entries[0].dev, entries[0].mnt_id));
    }
}

#[test]
fn p1_core_01_depths_and_closes_are_recorded() {
    let root = DirId::ROOT;
    let dir = |name: &str, parent: DirId, id: u32| Entry {
        dir: Some(DirId::new(id)),
        kind: Kind::Dir,
        ..file(name, parent)
    };
    let mut arena = Arena::new(Attrs::Basic);
    let events = [
        ScanEvent::Entry(dir("root", root, 0)),
        ScanEvent::Entry(dir("a", root, 1)),
        ScanEvent::Entry(file("f", root)),
        ScanEvent::DirClosed {
            dir: root,
            errno: None,
        },
        ScanEvent::Entry(dir("b", DirId::new(1), 2)),
        ScanEvent::DirClosed {
            dir: DirId::new(1),
            errno: None,
        },
        ScanEvent::DirClosed {
            dir: DirId::new(2),
            errno: Some(13),
        },
    ];
    for event in events {
        arena.push(event).unwrap();
    }
    assert_eq!(arena.dir(DirId::new(0)).depth, 0);
    assert_eq!(arena.dir(DirId::new(1)).depth, 1);
    assert_eq!(arena.dir(DirId::new(2)).depth, 2);
    assert_eq!(
        arena.dir(DirId::new(2)).flags & DirRec::UNREADABLE,
        DirRec::UNREADABLE
    );
    assert_eq!(arena.unreadable().get(&DirId::new(2)), Some(&13));
}

#[test]
fn p1_core_01_an_entry_without_full_attributes_reads_as_none() {
    // In a full scan, an entry the walk delivered without the full set is
    // not collected (NULL in the index), not owner 0 at the epoch.
    let full = FullAttrs {
        atime_ns: 5,
        btime_ns: None,
        mode: 0o40755,
        uid: 501,
        gid: 20,
    };
    let mut arena = Arena::new(Attrs::Full);
    let mut root = file("root", DirId::ROOT);
    root.kind = Kind::Dir;
    root.dir = Some(DirId::ROOT);
    root.full = Some(full);
    arena.push(ScanEvent::Entry(root)).unwrap();
    arena
        .push(ScanEvent::Entry(file("bare", DirId::ROOT)))
        .unwrap();
    let mut zeros = file("zeros", DirId::ROOT);
    zeros.full = Some(FullAttrs {
        atime_ns: 0,
        btime_ns: None,
        mode: 0,
        uid: 0,
        gid: 0,
    });
    arena.push(ScanEvent::Entry(zeros.clone())).unwrap();
    assert_eq!(arena.full(EntryIdx::ROOT), Some(full));
    assert_eq!(arena.full(EntryIdx(1)), None);
    assert_eq!(arena.full(EntryIdx(2)), zeros.full, "real zeros stay facts");
    // A basic scan has none at all.
    let mut basic = Arena::new(Attrs::Basic);
    basic.push(ScanEvent::Entry(zeros)).unwrap();
    assert_eq!(basic.full(EntryIdx::ROOT), None);
}

fn file(name: &str, parent: DirId) -> Entry {
    Entry {
        name: NameRef::from(name),
        parent,
        dir: None,
        kind: Kind::File,
        apparent: 1,
        allocated: 4096,
        mtime_ns: 0,
        ctime_ns: 0,
        nlink: 1,
        dev: 1,
        ino: 1,
        mnt_id: 1,
        share: Share::None,
        boundary: Boundary::None,
        flags: Flags::empty(),
        full: None,
    }
}

#[test]
fn p1_core_01_chunks_never_move() {
    let mut store = Chunked::default();
    for i in 0..(CHUNK * 3 + 7) {
        store.push(i);
    }
    assert_eq!(store.len(), CHUNK * 3 + 7);
    assert_eq!(store.get(CHUNK + 5), Some(&(CHUNK + 5)));
    // Four chunks of exactly CHUNK elements: growth never doubles a chunk.
    assert!(store.chunks.iter().all(|c| c.capacity() == CHUNK));
    assert_eq!(store.chunks.len(), 4);
}

/// Pushes a synthetic scan of `dirs` directories of `files` files each, with
/// names of the length real trees have and every file's link count
/// `nlink`, and returns the arena.
fn synthetic(dirs: u32, files: u32, nlink: u32) -> Arena {
    let mut arena = Arena::new(Attrs::Basic);
    let mut root = file("root", DirId::ROOT);
    root.kind = Kind::Dir;
    root.dir = Some(DirId::ROOT);
    arena.push(ScanEvent::Entry(root)).unwrap();
    let mut entry = file("", DirId::ROOT);
    for d in 1..=dirs {
        let parent = DirId::new(d / 8);
        entry.name = NameRef::from(format!("directory-{d:07}").as_str());
        entry.parent = parent;
        entry.kind = Kind::Dir;
        entry.dir = Some(DirId::new(d));
        arena.push(ScanEvent::Entry(entry.clone())).unwrap();
        entry.kind = Kind::File;
        entry.dir = None;
        entry.parent = DirId::new(d);
        entry.nlink = nlink;
        for f in 0..files {
            entry.name = NameRef::from(format!("file-{f:04}.dat").as_str());
            entry.ino = u64::from(d) << 16 | u64::from(f);
            arena.push(ScanEvent::Entry(entry.clone())).unwrap();
        }
    }
    arena
}

#[test]
fn p1_core_01_a_10m_entry_stream_stays_under_1_gb() {
    // 1M directories of 9 files: 10M entries and the root.
    let arena = synthetic(1_000_000, 9, 1);
    assert_eq!(arena.len(), 10_000_001);
    let bytes = arena.heap_bytes();
    let tenths = bytes * 10 / arena.len();
    eprintln!(
        "10M entries: {bytes} heap bytes, {}.{} per entry",
        tenths / 10,
        tenths % 10
    );
    assert!(bytes < 1 << 30, "{bytes} bytes");
}

#[test]
fn p1_core_01_a_tree_of_hard_links_grows_by_chunks() {
    // A store of hard links (pnpm, Nix, rsnapshot): every file has a
    // second link, outside the scan. Each keeps 16 bytes beside its record,
    // in chunks.
    let arena = synthetic(100_000, 9, 2);
    assert_eq!(arena.linked_len(), 900_000);
    let tenths = arena.heap_bytes() * 10 / arena.len();
    eprintln!(
        "hard links: {}.{} bytes per entry",
        tenths / 10,
        tenths % 10
    );
    assert!(tenths <= 900, "{tenths} tenths of a byte per entry");
    let link = arena.linked_at(899_999);
    assert_eq!((link.dev, link.ino), (1, 100_000 << 16 | 8));
}

/// The arena's bytes per entry over a benchmark tree, when it has been
/// built (`just bench build --trees T1,T3`): the arena's share of the
/// harness's RSS budget (100 bytes per entry on T1, 130 on T3).
#[test]
#[ignore = "needs bench/.trees/T1 and T3; DUSCOPE_TEST_PLATFORM=1"]
fn p1_core_01_arena_bytes_per_entry_on_the_bench_trees() {
    let _env = TestEnv::new();
    let trees = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../bench/.trees");
    for (tree, budget) in [("T1", 100), ("T3", 130)] {
        let root = trees.join(tree);
        if !root.is_dir() {
            eprintln!("skipped {tree}: not built");
            continue;
        }
        let (arena, _) = walk_into(&root, Attrs::Basic);
        let tenths = arena.heap_bytes() * 10 / arena.len();
        let shown = format!("{}.{}", tenths / 10, tenths % 10);
        eprintln!("{tree}: {} entries, {shown} bytes per entry", arena.len());
        assert!(tenths <= budget * 10, "{tree}: {shown} bytes per entry");
    }
}
