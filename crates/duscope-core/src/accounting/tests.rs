//! P1-CORE-03: hard links and clones charged once, `du` parity, and the
//! same ids charged by every scan of an unchanged tree.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use duscope_testkit::TestEnv;
use duscope_walk::{
    Attrs, Boundary, DirId, Entry, Flags, Kind, NameRef, Normalization, ScanEvent, Share,
    VolumeClass, VolumeInfo, WalkOptions, walk,
};

use proptest::prelude::*;

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

/// What `du -s -k` prints for `root`: the total in KiB, and its stderr.
fn du_sk(env: &TestEnv, root: &Path) -> (u64, String) {
    let out = env
        .command("du")
        .args(["-s", "-k"])
        .arg(root)
        .output()
        .unwrap();
    let total = String::from_utf8(out.stdout)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .parse()
        .unwrap();
    (total, String::from_utf8_lossy(&out.stderr).into_owned())
}

/// The scan's total as `du -s -k` counts it, in KiB.
fn du_kib(arena: &Arena, rollup: &RollUp) -> u64 {
    let mut ours = rollup.charges().du_compatible(arena, DirId::ROOT);
    // Apple's du leaves out a directory it cannot read, its own blocks
    // included; GNU du counts them.
    if cfg!(target_os = "macos") {
        for dir in arena.unreadable().keys() {
            ours -= arena
                .entry(EntryIdx::from_raw(arena.dir(*dir).entry))
                .allocated;
        }
    }
    ours.div_ceil(1024)
}

/// The entries whose allocation differs from `lstat`'s now, one a line: a
/// scan that misread an entry, or a tree that changed after it.
fn changed_since(arena: &Arena, root: &Path) -> String {
    let paths = paths(arena);
    let mut lines = String::new();
    for view in arena.entries() {
        if matches!(
            view.kind,
            Kind::Socket | Kind::Fifo | Kind::Char | Kind::Block
        ) {
            continue;
        }
        // Entries below an unreadable directory have no lstat to compare.
        let path = root.join(&paths[view.idx.index()]);
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        let now = std::os::unix::fs::MetadataExt::blocks(&meta) * 512;
        if now != view.allocated {
            let _ = writeln!(
                lines,
                "{}: {} in the scan, {now} now",
                path.display(),
                view.allocated
            );
        }
    }
    lines
}

#[test]
fn p1_core_03_du_parity_on_the_fixture_tree_with_hard_links() {
    let env = TestEnv::new();
    let root = env.fixtures().root();
    // du reads the tree before and after the scan. Readings that differ mean
    // du's view of the tree changed while the test ran, so it reads again; a
    // scan that disagrees with a steady du fails at once.
    let mut unsteady = Vec::new();
    let (arena, rollup, du, stderr) = loop {
        let (before, _) = du_sk(&env, &root);
        let (arena, rollup) = scan(&root, 0);
        let (after, stderr) = du_sk(&env, &root);
        if before == after {
            break (arena, rollup, after, stderr);
        }
        unsteady.push((before, after));
        assert!(
            unsteady.len() < 3,
            "du read different totals before and after each scan: {unsteady:?}"
        );
    };
    assert_eq!(
        du_kib(&arena, &rollup),
        du,
        "du -sk (after unsteady readings {unsteady:?})\nentries whose allocation changed:\n{}du's stderr:\n{stderr}",
        changed_since(&arena, &root)
    );
    // The hard-links case: one 256 KiB inode with four links counts once.
    let groups: Vec<_> = rollup.charges().hardlinks(&arena).collect();
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
            .hardlinks(&arena)
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
    let clones: Vec<_> = charges.clones(&arena).collect();
    assert_eq!(clones.len(), 1);
    assert_eq!(clones[0].members.len(), 3);
    // The group's 2 MiB once in cum_allocated, three times in what du sees.
    let shared = 2 * 1024 * 1024;
    let du = charges.du_compatible(&arena, DirId::ROOT);
    assert_eq!(du - arena.dir(DirId::ROOT).cum_allocated, 2 * shared);
    let carries: Vec<bool> = clones[0]
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
    let group = &charges.hardlinks(&arena).next().unwrap();
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
    let group = &charges.clones(&arena).next().unwrap();
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
    assert_eq!(rollup.charges().hardlinks(&arena).count(), 0);
    assert_eq!(arena.dir(DirId::ROOT).cum_allocated, 2 * 4096);
}

#[test]
fn p1_core_03_charging_a_tree_of_hard_links_keeps_4_bytes_a_link() {
    // A store of hard links: 300k files in a, each linked again in b.
    let files = 300_000u32;
    let mut entries = Vec::with_capacity(2 * files as usize);
    for f in 0..files {
        for parent in [1, 2] {
            entries.push(Entry {
                nlink: 2,
                ino: u64::from(f),
                share: Share::Hardlink,
                ..entry(&format!("file-{f:06}"), parent, Kind::File)
            });
        }
    }
    let (arena, rollup) = rolled(entries, None);
    let charges = rollup.charges();
    let per_link = charges.heap_bytes() * 10 / arena.linked_len();
    eprintln!("charges: {}.{} bytes a link", per_link / 10, per_link % 10);
    assert!(per_link <= 45, "{per_link} tenths of a byte a link");
    // a's copy carries the bytes: it comes first in pre-order.
    let a = arena.dir(DirId::new(1));
    let b = arena.dir(DirId::new(2));
    assert_eq!(a.cum_allocated, u64::from(files) * 4096);
    assert_eq!(b.cum_allocated, 0);
    assert_eq!(charges.hardlinks(&arena).count(), files as usize);
    assert!(
        charges
            .hardlinks(&arena)
            .all(|g| g.sites.len() == 2 && arena.entry(g.sites[0]).parent == DirId::new(1))
    );
}

/// A path below the root, as components.
type PathParts = Vec<Vec<u8>>;

/// A node of a random tree for the order-independence property: what the
/// walk would report for it.
#[derive(Debug, Clone)]
struct Node {
    path: PathParts,
    dir: bool,
    apparent: u64,
    allocated: u64,
    mtime: i64,
    dev: u64,
    ino: u64,
    nlink: u32,
    share: Share,
    flags: Flags,
}

/// A random tree: node `i` hangs below one of the directories before it,
/// and `what` makes it a directory, a plain file, a link of one of four
/// inodes on two devices (the same inode numbers on both), a clone that
/// shares every block or no longer does, a link of one of two cloned
/// inodes, or a file that may share blocks without a clone id.
fn shared_tree(picks: &[(u16, u8, u16, u8)]) -> Vec<Node> {
    let mut nodes = vec![Node {
        path: Vec::new(),
        dir: true,
        apparent: 64,
        allocated: 4096,
        mtime: 1,
        dev: 7,
        ino: 1,
        nlink: 1,
        share: Share::None,
        flags: Flags::empty(),
    }];
    let mut dirs = vec![0usize];
    for (i, &(pick, what, size, group)) in picks.iter().enumerate() {
        let parent = dirs[usize::from(pick) % dirs.len()];
        let mut path = nodes[parent].path.clone();
        // Names sort by their first letter, unique by their index.
        let mut name = vec![b'a' + u8::try_from(size % 5).unwrap()];
        name.extend(format!("-{i}").bytes());
        path.push(name);
        let size = u64::from(size);
        let g = u64::from(group % 4);
        let plain = Node {
            path,
            dir: false,
            apparent: size,
            allocated: size.div_ceil(512) * 512,
            mtime: i64::try_from(size).unwrap() - 300,
            dev: 7,
            ino: 100 + i as u64,
            nlink: 1,
            share: Share::None,
            flags: Flags::empty(),
        };
        let node = match what % 8 {
            0 | 1 => Node {
                dir: true,
                apparent: 64,
                allocated: 4096,
                ..plain
            },
            2 => plain,
            3 => Node {
                dev: 7 + g % 2,
                ino: 1_000_000 + g / 2,
                nlink: 4,
                share: Share::Hardlink,
                apparent: 1000 * (g + 1),
                allocated: 4096 * (g + 1),
                ..plain
            },
            4 => Node {
                share: Share::Clone(1 + g % 3),
                flags: Flags::MAY_SHARE | Flags::SHARES_ALL,
                ..plain
            },
            5 => Node {
                share: Share::Clone(1 + g % 3),
                flags: Flags::MAY_SHARE,
                ..plain
            },
            6 => {
                // One inode per clone group 1 and 2; every link of an inode
                // has its sizes and flags.
                let c = g % 2;
                let flags = if c == 0 {
                    Flags::MAY_SHARE | Flags::SHARES_ALL
                } else {
                    Flags::MAY_SHARE
                };
                Node {
                    ino: 2_000_000 + c,
                    nlink: 3,
                    share: Share::Clone(1 + c),
                    flags,
                    apparent: 7000,
                    allocated: 8192 * (c + 1),
                    ..plain
                }
            }
            _ => Node {
                share: Share::MayShare,
                flags: Flags::MAY_SHARE,
                ..plain
            },
        };
        if node.dir {
            dirs.push(nodes.len());
        }
        nodes.push(node);
    }
    nodes
}

fn node_entry(node: &Node, parent: DirId, dir: Option<DirId>) -> Entry {
    Entry {
        name: NameRef::new(node.path.last().map_or(b"root".as_slice(), Vec::as_slice)),
        parent,
        dir,
        kind: if node.dir { Kind::Dir } else { Kind::File },
        apparent: node.apparent,
        allocated: node.allocated,
        mtime_ns: node.mtime,
        ctime_ns: node.mtime,
        nlink: node.nlink,
        dev: node.dev,
        ino: node.ino,
        mnt_id: 7,
        share: node.share,
        boundary: Boundary::None,
        flags: node.flags,
        full: None,
    }
}

/// The walk's events for `nodes`. With seed 0 directories are read in
/// order and children delivered sorted; otherwise any directory whose
/// entry is out may be read next and its children come shuffled, with
/// directory ids handed out in delivery order, as workers would.
fn shared_events(nodes: &[Node], seed: u64) -> Vec<ScanEvent> {
    let mut children: BTreeMap<PathParts, Vec<usize>> = BTreeMap::new();
    for (i, node) in nodes.iter().enumerate().skip(1) {
        let parent = node.path[..node.path.len() - 1].to_vec();
        children.entry(parent).or_default().push(i);
    }
    let mut rng = seed;
    // xorshift64: `next(n)` is below n.
    let mut next = |n: usize| {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        usize::try_from(rng % n as u64).unwrap()
    };
    let mut out = vec![ScanEvent::Entry(node_entry(
        &nodes[0],
        DirId::ROOT,
        Some(DirId::ROOT),
    ))];
    let mut ids = 1u32;
    let mut pending = vec![(0usize, DirId::ROOT)];
    while !pending.is_empty() {
        let pick = if seed == 0 { 0 } else { next(pending.len()) };
        let (node, dir) = pending.remove(pick);
        let mut kids = children.get(&nodes[node].path).cloned().unwrap_or_default();
        if seed != 0 {
            for i in (1..kids.len()).rev() {
                kids.swap(i, next(i + 1));
            }
        }
        for kid in kids {
            let id = nodes[kid].dir.then(|| {
                let id = DirId::new(ids);
                ids += 1;
                pending.push((kid, id));
                id
            });
            out.push(ScanEvent::Entry(node_entry(&nodes[kid], dir, id)));
        }
        out.push(ScanEvent::DirClosed { dir, errno: None });
    }
    out
}

/// A directory's fields after charging: `cum_apparent`, `cum_allocated`,
/// `max_mtime_ns`, `n_files`, `n_dirs`, the upper-bound flag and
/// `du_compatible`.
type DirTotals = (u64, u64, i64, u32, u32, bool, u64);

/// A hard-link or clone group: its device, its inode number or clone id,
/// and its members, lowest pre-order id first.
type Group<T> = (u64, u64, Vec<T>);

/// What charging decided, by path, so scans delivered in different orders
/// compare.
#[derive(Debug, PartialEq, Eq)]
struct ByPath {
    /// Each hard-linked inode's sites; the first is charged.
    links: Vec<Group<PathParts>>,
    /// Each clone group's members, and whether each is charged.
    clones: Vec<Group<(PathParts, bool)>>,
    uncharged: BTreeSet<PathParts>,
    dirs: BTreeMap<PathParts, DirTotals>,
}

/// A rolled-up stream with each entry's path, by scan-order index.
struct Rolled<'a> {
    arena: Arena,
    rollup: RollUp,
    paths: Vec<PathParts>,
    node_of: BTreeMap<&'a PathParts, &'a Node>,
}

impl Rolled<'_> {
    fn new(nodes: &[Node], stream: Vec<ScanEvent>) -> Rolled<'_> {
        let mut arena = Arena::new(Attrs::Basic);
        for event in stream {
            arena.push(event).unwrap();
        }
        let rollup = roll_up(&mut arena);
        let mut dir_paths: BTreeMap<DirId, PathParts> = BTreeMap::new();
        let mut paths = Vec::with_capacity(arena.len());
        for view in arena.entries() {
            let path = if view.idx == EntryIdx::ROOT {
                Vec::new()
            } else {
                let mut p = dir_paths[&view.parent].clone();
                p.push(view.name.to_vec());
                p
            };
            if let Some(dir) = view.dir {
                dir_paths.insert(dir, path.clone());
            }
            paths.push(path);
        }
        let node_of = nodes.iter().map(|n| (&n.path, n)).collect();
        Rolled {
            arena,
            rollup,
            paths,
            node_of,
        }
    }

    fn node(&self, i: usize) -> &Node {
        self.node_of[&self.paths[i]]
    }

    fn paths_of(&self, members: &[usize]) -> Vec<PathParts> {
        members.iter().map(|&i| self.paths[i].clone()).collect()
    }

    /// `members` ordered by pre-order id.
    fn by_id(&self, mut members: Vec<usize>) -> Vec<usize> {
        members.sort_by_key(|&i| self.rollup.id_of(idx(i)));
        members
    }
}

fn idx(i: usize) -> EntryIdx {
    EntryIdx::from_raw(u32::try_from(i).unwrap())
}

/// What charging should decide, worked out from the tree: a later link of
/// an inode (by pre-order id) is linked away; a later member of a clone
/// group, links away left out, is uncharged when it shares every block and
/// marks its ancestors an upper bound when it does not; so does a file that
/// may share blocks without a group.
struct Model {
    inodes: BTreeMap<(u64, u64), Vec<usize>>,
    groups: BTreeMap<(u64, u64), Vec<usize>>,
    linked_away: BTreeSet<usize>,
    clone_uncharged: BTreeSet<usize>,
    uncharged: BTreeSet<usize>,
    markers: Vec<usize>,
}

impl Model {
    fn new(rolled: &Rolled<'_>) -> Model {
        let mut inodes: BTreeMap<(u64, u64), Vec<usize>> = BTreeMap::new();
        for i in 0..rolled.paths.len() {
            let node = rolled.node(i);
            if !node.dir && node.nlink > 1 {
                inodes.entry((node.dev, node.ino)).or_default().push(i);
            }
        }
        let inodes: BTreeMap<_, _> = inodes
            .into_iter()
            .map(|(key, v)| (key, rolled.by_id(v)))
            .collect();
        let linked_away: BTreeSet<usize> = inodes.values().flat_map(|v| v[1..].to_vec()).collect();
        let mut groups: BTreeMap<(u64, u64), Vec<usize>> = BTreeMap::new();
        let mut markers = Vec::new();
        for i in 0..rolled.paths.len() {
            let node = rolled.node(i);
            match node.share {
                Share::Clone(id) if !linked_away.contains(&i) => {
                    groups.entry((node.dev, id)).or_default().push(i);
                }
                Share::MayShare => markers.push(i),
                _ => {}
            }
        }
        let groups: BTreeMap<_, _> = groups
            .into_iter()
            .map(|(key, v)| (key, rolled.by_id(v)))
            .collect();
        let mut clone_uncharged = BTreeSet::new();
        for members in groups.values() {
            for &m in &members[1..] {
                if rolled.node(m).flags.contains(Flags::SHARES_ALL) {
                    clone_uncharged.insert(m);
                } else {
                    markers.push(m);
                }
            }
        }
        let uncharged = linked_away.union(&clone_uncharged).copied().collect();
        Model {
            inodes,
            groups,
            linked_away,
            clone_uncharged,
            uncharged,
            markers,
        }
    }
}

/// Rolls up a stream, checks the charging against the model and every
/// directory's totals against sums over its subtree, and returns the
/// decisions by path.
fn charged_by_path(nodes: &[Node], stream: Vec<ScanEvent>) -> ByPath {
    let rolled = Rolled::new(nodes, stream);
    let model = Model::new(&rolled);
    let (arena, charges, paths) = (&rolled.arena, rolled.rollup.charges(), &rolled.paths);
    for (i, path) in paths.iter().enumerate() {
        assert_eq!(
            charges.charged(idx(i)),
            !model.uncharged.contains(&i),
            "{path:?}"
        );
    }
    let links: Vec<Group<PathParts>> = charges
        .hardlinks(arena)
        .map(|g| {
            let sites: Vec<usize> = g.sites.iter().map(|s| s.index()).collect();
            (g.dev, g.ino, rolled.paths_of(&sites))
        })
        .collect();
    let expected: Vec<Group<PathParts>> = model
        .inodes
        .iter()
        .map(|(&(dev, ino), v)| (dev, ino, rolled.paths_of(v)))
        .collect();
    assert_eq!(links, expected, "hard-link sites, lowest id first");
    let clones: Vec<Group<(PathParts, bool)>> = charges
        .clones(arena)
        .map(|g| {
            let members = g
                .members
                .iter()
                .map(|&m| (paths[m.index()].clone(), charges.charged(m)))
                .collect();
            (g.dev, g.clone_id, members)
        })
        .collect();
    let members: Vec<Group<PathParts>> = clones
        .iter()
        .map(|(dev, id, m)| (*dev, *id, m.iter().map(|(p, _)| p.clone()).collect()))
        .collect();
    let expected: Vec<Group<PathParts>> = model
        .groups
        .iter()
        .map(|(&(dev, id), v)| (dev, id, rolled.paths_of(v)))
        .collect();
    assert_eq!(members, expected, "clone groups, lowest id first");
    ByPath {
        links,
        clones,
        uncharged: model.uncharged.iter().map(|&i| paths[i].clone()).collect(),
        dirs: checked_dirs(nodes, &rolled, &model),
    }
}

/// Every directory's totals, checked against sums over its subtree; and
/// the root's against the tree's distinct inodes.
fn checked_dirs(
    nodes: &[Node],
    rolled: &Rolled<'_>,
    model: &Model,
) -> BTreeMap<PathParts, DirTotals> {
    let (arena, charges, paths) = (&rolled.arena, rolled.rollup.charges(), &rolled.paths);
    let mut dirs = BTreeMap::new();
    for (i, path) in paths.iter().enumerate() {
        let Some(dir) = arena.entry(idx(i)).dir else {
            continue;
        };
        let inside: Vec<usize> = (0..paths.len())
            .filter(|&j| paths[j].starts_with(path))
            .collect();
        let sum = |leave_out: &BTreeSet<usize>, size: fn(&Node) -> u64| {
            inside
                .iter()
                .filter(|j| !leave_out.contains(j))
                .map(|&j| size(rolled.node(j)))
                .sum::<u64>()
        };
        let rec = arena.dir(dir);
        let du = charges.du_compatible(arena, dir);
        assert_eq!(
            rec.cum_apparent,
            sum(&model.linked_away, |n| n.apparent),
            "cum_apparent of {path:?}"
        );
        assert_eq!(
            rec.cum_allocated,
            sum(&model.uncharged, |n| n.allocated),
            "cum_allocated of {path:?}"
        );
        assert_eq!(
            du,
            sum(&model.linked_away, |n| n.allocated),
            "du_compatible of {path:?}"
        );
        let upper = rec.flags & DirRec::UPPER_BOUND != 0;
        let marked = model.markers.iter().any(|&m| m != i && inside.contains(&m));
        assert_eq!(upper, marked, "upper bound of {path:?}");
        let totals = (
            rec.cum_apparent,
            rec.cum_allocated,
            rec.max_mtime_ns,
            rec.n_files,
            rec.n_dirs,
            upper,
            du,
        );
        dirs.insert(path.clone(), totals);
    }
    // The root holds each inode's blocks once: du counts every (dev, ino)
    // once, and cum_allocated also leaves out the clones that share all
    // their blocks.
    let mut distinct: BTreeMap<(u64, u64), u64> = BTreeMap::new();
    for node in nodes {
        distinct.insert((node.dev, node.ino), node.allocated);
    }
    let distinct: u64 = distinct.values().sum();
    let cloned_away: u64 = model
        .clone_uncharged
        .iter()
        .map(|&i| rolled.node(i).allocated)
        .sum();
    assert_eq!(charges.du_compatible(arena, DirId::ROOT), distinct);
    assert_eq!(arena.dir(DirId::ROOT).cum_allocated, distinct - cloned_away);
    dirs
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn p1_core_03_charging_is_independent_of_delivery_order(
        picks in prop::collection::vec(
            (any::<u16>(), any::<u8>(), 1u16..5000, any::<u8>()),
            0..200,
        ),
        seed in 1u64..u64::MAX,
    ) {
        let nodes = shared_tree(&picks);
        let sorted = charged_by_path(&nodes, shared_events(&nodes, 0));
        let shuffled = charged_by_path(&nodes, shared_events(&nodes, seed));
        prop_assert_eq!(sorted, shuffled);
    }
}
