//! P1-CORE-03: hard links and clones charged once, `du` parity, and the
//! same ids charged by every scan of an unchanged tree.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use duscope_testkit::TestEnv;
use duscope_walk::{
    Attrs, Boundary, DirId, Entry, Flags, Kind, NameRef, Normalization, ScanEvent, Share,
    VolumeClass, VolumeInfo, WalkOptions, walk,
};

use crate::arena::{Arena, DirRec, EntryIdx};
use crate::rollup::{RollUp, roll_up};

fn scan(root: &Path, threads: usize) -> (Arena, RollUp) {
    let mut arena = Arena::new(Attrs::Basic);
    let opts = WalkOptions {
        threads,
        ..WalkOptions::default()
    };
    walk(root, &opts, |event| arena.push(event).unwrap()).unwrap();
    let rollup = roll_up(&mut arena);
    (arena, rollup)
}

/// Each entry's path below the root, by index.
fn paths(arena: &Arena) -> Vec<PathBuf> {
    let mut dirs: BTreeMap<DirId, PathBuf> = BTreeMap::new();
    let mut out = Vec::with_capacity(arena.len());
    for view in arena.entries() {
        let path = if view.idx == EntryIdx::ROOT {
            PathBuf::new()
        } else {
            dirs[&view.parent].join(std::str::from_utf8(view.name).unwrap_or("?"))
        };
        if let Some(dir) = view.dir {
            dirs.insert(dir, path.clone());
        }
        out.push(path);
    }
    out
}

#[test]
fn p1_core_03_du_parity_on_the_fixture_tree_with_hard_links() {
    let env = TestEnv::new();
    let root = env.fixtures().root();
    let (arena, rollup) = scan(&root, 0);
    let mut ours = rollup.charges().du_compatible(&arena, DirId::ROOT);
    // Apple's du leaves out a directory it cannot read, its own blocks
    // included; GNU du counts them.
    if cfg!(target_os = "macos") {
        for dir in arena.unreadable().keys() {
            ours -= arena
                .entry(EntryIdx::from_raw(arena.dir(*dir).entry))
                .allocated;
        }
    }
    let out = env
        .command("du")
        .args(["-s", "-k"])
        .arg(&root)
        .output()
        .unwrap();
    let du: u64 = String::from_utf8(out.stdout)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(ours.div_ceil(1024), du, "du -sk");
    // The hard-links case: one 256 KiB inode with four links counts once.
    let groups = rollup.charges().hardlinks();
    assert!(
        groups.iter().any(|g| g.sites.len() == 4 && g.nlink == 4),
        "{groups:?}"
    );
}

#[test]
fn p1_core_03_two_scans_of_an_unchanged_tree_charge_the_same_ids() {
    let env = TestEnv::new();
    let root = env.fixtures().root();
    let charged = |threads| {
        let (arena, rollup) = scan(&root, threads);
        let paths = paths(&arena);
        let mut groups: Vec<(PathBuf, Vec<PathBuf>, u32)> = rollup
            .charges()
            .hardlinks()
            .iter()
            .map(|g| {
                let sites: Vec<PathBuf> =
                    g.sites.iter().map(|s| paths[s.index()].clone()).collect();
                (sites[0].clone(), sites, rollup.id_of(g.sites[0]))
            })
            .collect();
        groups.sort();
        let uncharged: Vec<PathBuf> = arena
            .entries()
            .filter(|v| !rollup.charges().charged(v.idx))
            .map(|v| paths[v.idx.index()].clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        (groups, uncharged)
    };
    // One worker and eight read the tree in different orders.
    assert_eq!(charged(1), charged(8));
}

#[test]
fn p1_core_03_a_clone_group_is_counted_once() {
    let env = TestEnv::new();
    if !env.fixtures().built("clones") || cfg!(not(target_os = "macos")) {
        // Clone ids are read on APFS only.
        return;
    }
    let root = env.fixtures().case_root("clones");
    let (arena, rollup) = scan(&root, 0);
    let charges = rollup.charges();
    assert_eq!(charges.clones().len(), 1);
    assert_eq!(charges.clones()[0].members.len(), 3);
    // The group's 2 MiB once in cum_allocated, three times in what du sees.
    let shared = 2 * 1024 * 1024;
    let du = charges.du_compatible(&arena, DirId::ROOT);
    assert_eq!(du - arena.dir(DirId::ROOT).cum_allocated, 2 * shared);
    let carries: Vec<bool> = charges.clones()[0]
        .members
        .iter()
        .map(|&m| charges.charged(m))
        .collect();
    assert_eq!(
        carries,
        [true, false, false],
        "the lowest pre-order id carries the bytes"
    );
}

/// An entry for the synthetic streams below.
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

fn dir(name: &str, parent: u32, id: u32) -> Entry {
    Entry {
        dir: Some(DirId::new(id)),
        apparent: 64,
        allocated: 0,
        ..entry(name, parent, Kind::Dir)
    }
}

/// /root with dirs a (1) and b (2), b holding c (3).
fn rolled(files: Vec<Entry>, volume: Option<VolumeInfo>) -> (Arena, RollUp) {
    let mut arena = Arena::new(Attrs::Basic);
    let mut events = vec![ScanEvent::Entry(dir("root", 0, 0))];
    events.extend(volume.map(ScanEvent::Volume));
    for e in [dir("b", 0, 2), dir("a", 0, 1), dir("c", 2, 3)] {
        events.push(ScanEvent::Entry(e));
    }
    events.extend(files.into_iter().map(ScanEvent::Entry));
    for event in events {
        arena.push(event).unwrap();
    }
    let rollup = roll_up(&mut arena);
    (arena, rollup)
}

fn link(name: &str, parent: u32) -> Entry {
    Entry {
        nlink: 3,
        ino: 99,
        share: Share::Hardlink,
        ..entry(name, parent, Kind::File)
    }
}

#[test]
fn p1_core_03_a_hard_link_is_charged_to_its_lowest_pre_order_id() {
    // Delivered c's link first; a/x has the lowest pre-order id.
    let (arena, rollup) = rolled(vec![link("z", 3), link("y", 2), link("x", 1)], None);
    let charges = rollup.charges();
    let group = &charges.hardlinks()[0];
    let names: Vec<&[u8]> = group.sites.iter().map(|&s| arena.entry(s).name).collect();
    assert_eq!(names, [b"x".as_slice(), b"y", b"z"]);
    assert!(charges.charged(group.sites[0]));
    assert!(!charges.charged(group.sites[1]) && !charges.charged(group.sites[2]));
    // Once in the root's totals: allocated and, as GNU du --apparent-size
    // does, apparent.
    let root = arena.dir(DirId::ROOT);
    assert_eq!(root.cum_allocated, 4096);
    assert_eq!(root.cum_apparent, 4 * 64 + 1000);
    // b holds an uncharged link: b's totals leave it out too.
    assert_eq!(arena.dir(DirId::new(2)).cum_allocated, 0);
    assert_eq!(charges.du_compatible(&arena, DirId::ROOT), 4096);
}

#[test]
fn p1_core_03_clones_and_may_share() {
    let clone = |name: &str, parent, flags| Entry {
        share: Share::Clone(5),
        flags,
        ..entry(name, parent, Kind::File)
    };
    let all = Flags::MAY_SHARE | Flags::SHARES_ALL;
    let maybe = Entry {
        share: Share::MayShare,
        flags: Flags::MAY_SHARE,
        ..entry("m", 3, Kind::File)
    };
    let (arena, rollup) = rolled(
        vec![
            clone("o", 1, all),
            clone("k", 2, all),
            clone("p", 2, Flags::MAY_SHARE),
            maybe,
        ],
        None,
    );
    let charges = rollup.charges();
    let group = &charges.clones()[0];
    let carries: Vec<(&[u8], bool)> = group
        .members
        .iter()
        .map(|&m| (arena.entry(m).name, charges.charged(m)))
        .collect();
    // a/o carries the bytes; b/k shares all of them; b/p no longer does and
    // keeps its own.
    assert_eq!(
        carries,
        [
            (b"o".as_slice(), true),
            (b"k".as_slice(), false),
            (b"p".as_slice(), true)
        ]
    );
    let root = arena.dir(DirId::ROOT);
    assert_eq!(root.cum_allocated, 3 * 4096);
    assert_eq!(charges.du_compatible(&arena, DirId::ROOT), 4 * 4096);
    // p and m may share blocks no group accounts for: their ancestors are
    // upper bounds, a is not.
    let upper = |id: u32| arena.dir(DirId::new(id)).flags & DirRec::UPPER_BOUND != 0;
    assert!(upper(0) && upper(2) && upper(3));
    assert!(!upper(1));
}

#[test]
fn p1_core_03_links_on_an_overlay_are_not_deduplicated() {
    let overlay = VolumeInfo {
        dev: 7,
        mnt_id: 7,
        mount_point: PathBuf::from("/"),
        fs_type: "overlay".to_owned(),
        class: VolumeClass::Overlay,
        read_only: false,
        case_sensitive: Some(true),
        normalization: Normalization::None,
        options: Vec::new(),
        total_bytes: 0,
        free_bytes: 0,
        reason: None,
    };
    let (arena, rollup) = rolled(vec![link("x", 1), link("y", 2)], Some(overlay));
    assert!(rollup.charges().hardlinks().is_empty());
    assert_eq!(arena.dir(DirId::ROOT).cum_allocated, 2 * 4096);
}
