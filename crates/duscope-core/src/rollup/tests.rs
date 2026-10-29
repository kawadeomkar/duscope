//! P1-CORE-02: the roll-up is independent of scan order, and every subtree
//! is a contiguous id range.

use std::collections::BTreeMap;

use duscope_walk::{Attrs, Boundary, DirId, Entry, Flags, Kind, NameRef, ScanEvent, Share};
use proptest::prelude::*;

use super::roll_up;
use crate::arena::{Arena, EntryIdx};

/// A tree as paths: each node's path below the root, kind, sizes and mtime.
#[derive(Debug, Clone)]
struct Node {
    path: Vec<Vec<u8>>,
    dir: bool,
    apparent: u64,
    allocated: u64,
    mtime: i64,
}

/// A random tree: node `i` hangs below one of the directories before it.
/// Names come from a small alphabet so siblings collide in prefix and
/// sort order matters; a name is made unique within its directory.
fn tree(picks: &[(u16, bool, u16, u8)]) -> Vec<Node> {
    let mut nodes = vec![Node {
        path: Vec::new(),
        dir: true,
        apparent: 64,
        allocated: 4096,
        mtime: 100,
    }];
    let mut dirs = vec![0usize];
    for (i, &(pick, dir, size, letter)) in picks.iter().enumerate() {
        let parent = dirs[usize::from(pick) % dirs.len()];
        let mut path = nodes[parent].path.clone();
        // A name sorting by its letter first, unique by its index.
        let mut name = vec![b'a' + letter % 5];
        name.extend(format!("{}", i % 7).bytes());
        name.extend(format!("-{i}").bytes());
        path.push(name);
        nodes.push(Node {
            path,
            dir,
            apparent: u64::from(size),
            allocated: u64::from(size).div_ceil(512) * 512,
            mtime: i64::from(size) * 3 - 500,
        });
        if dir {
            dirs.push(nodes.len() - 1);
        }
    }
    nodes
}

fn entry(node: &Node, parent: DirId, dir: Option<DirId>) -> Entry {
    Entry {
        name: NameRef::new(node.path.last().map_or(b"root".as_slice(), Vec::as_slice)),
        parent,
        dir,
        kind: if node.dir { Kind::Dir } else { Kind::File },
        apparent: node.apparent,
        allocated: node.allocated,
        mtime_ns: node.mtime,
        ctime_ns: node.mtime,
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

/// The walk's events for `nodes`, reading the directories in an order
/// `seed` picks: any directory whose entry is out may be read next, its
/// children delivered in a shuffled order, as workers would. With seed 0
/// directories are read in order and children delivered sorted.
fn events(nodes: &[Node], seed: u64) -> Vec<ScanEvent> {
    let mut children: BTreeMap<Vec<Vec<u8>>, Vec<usize>> = BTreeMap::new();
    for (i, node) in nodes.iter().enumerate().skip(1) {
        let parent = node.path[..node.path.len() - 1].to_vec();
        children.entry(parent).or_default().push(i);
    }
    let mut rng = seed;
    // xorshift64: deterministic, good enough to shuffle; `next(n)` is below n.
    let mut next = |n: usize| {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        usize::try_from(rng % n as u64).unwrap()
    };
    let mut out = vec![ScanEvent::Entry(entry(
        &nodes[0],
        DirId::ROOT,
        Some(DirId::ROOT),
    ))];
    let mut ids = 1u32;
    // Directories delivered but not yet read: (node, its id).
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
            out.push(ScanEvent::Entry(entry(&nodes[kid], dir, id)));
        }
        out.push(ScanEvent::DirClosed { dir, errno: None });
    }
    out
}

/// One numbered entry: its path, its fields, and a directory's cumulative
/// fields with its ranges relative to its own id.
type Row = (
    Vec<Vec<u8>>,
    Kind,
    u64,
    u64,
    Option<(u64, u64, i64, u32, u32, u32, u32)>,
);

/// The roll-up of a stream, in pre-order, as rows comparable across scan
/// orders; checks the ranges on the way.
fn rolled(nodes: &[Node], stream: Vec<ScanEvent>) -> Vec<Row> {
    let mut arena = Arena::new(Attrs::Basic);
    for event in stream {
        arena.push(event).unwrap();
    }
    let rollup = roll_up(&mut arena);
    assert_eq!(rollup.len(), nodes.len());
    // Paths by id, rebuilt from the parents.
    let mut paths: Vec<Vec<Vec<u8>>> = Vec::with_capacity(rollup.len());
    let mut dir_paths: BTreeMap<DirId, Vec<Vec<u8>>> = BTreeMap::new();
    let mut rows = Vec::new();
    for (id, idx) in rollup.preorder().enumerate() {
        assert_eq!(rollup.id_of(idx) as usize, id);
        assert_eq!(rollup.entry_at(rollup.id_of(idx)), idx);
        let view = arena.entry(idx);
        let path = if idx == EntryIdx::ROOT {
            Vec::new()
        } else {
            let mut p = dir_paths[&view.parent].clone();
            p.push(view.name.to_vec());
            p
        };
        if let Some(dir) = view.dir {
            dir_paths.insert(dir, path.clone());
        }
        paths.push(path.clone());
        let cum = view.dir.map(|dir| {
            let rec = arena.dir(dir);
            let id = u32::try_from(id).unwrap();
            (
                rec.cum_apparent,
                rec.cum_allocated,
                rec.max_mtime_ns,
                rec.n_files,
                rec.n_dirs,
                rec.last_id - id,
                rec.files_end - id,
            )
        });
        rows.push((path, view.kind, view.apparent, view.allocated, cum));
    }
    // Every subtree is the contiguous range id ..= last_id, its own files
    // id+1 ..= files_end, and its totals the sums over that range.
    for (id, row) in rows.iter().enumerate() {
        let Some((cum_app, cum_alloc, max_mtime, n_files, n_dirs, last, files_end)) = row.4 else {
            continue;
        };
        let last = id + last as usize;
        let inside: Vec<usize> = (0..paths.len())
            .filter(|&j| paths[j].starts_with(&paths[id]))
            .collect();
        assert_eq!(
            inside,
            (id..=last).collect::<Vec<_>>(),
            "subtree of {:?}",
            paths[id]
        );
        let own_files: Vec<usize> = (id + 1..=last)
            .filter(|&j| paths[j].len() == paths[id].len() + 1 && rows[j].1 != Kind::Dir)
            .collect();
        assert_eq!(
            own_files,
            (id + 1..=id + files_end as usize).collect::<Vec<_>>()
        );
        let sum = |f: fn(&Row) -> u64| (id..=last).map(|j| f(&rows[j])).sum::<u64>();
        assert_eq!(cum_app, sum(|r| r.2));
        assert_eq!(cum_alloc, sum(|r| r.3));
        assert_eq!(
            u64::from(n_files),
            (id + 1..=last).filter(|&j| rows[j].1 != Kind::Dir).count() as u64
        );
        assert_eq!(
            u64::from(n_dirs),
            (id + 1..=last).filter(|&j| rows[j].1 == Kind::Dir).count() as u64
        );
        let newest = (id..=last)
            .map(|j| nodes_mtime(nodes, &paths[j]))
            .max()
            .unwrap();
        assert_eq!(max_mtime, newest);
    }
    // Siblings bytewise, files first.
    for window in rows.windows(2) {
        let (a, b) = (&window[0], &window[1]);
        if a.0.len() == b.0.len()
            && a.0[..a.0.len().saturating_sub(1)] == b.0[..b.0.len().saturating_sub(1)]
        {
            assert!(
                (a.1 == Kind::Dir, &a.0) < (b.1 == Kind::Dir, &b.0),
                "{:?} before {:?}",
                a.0,
                b.0
            );
        }
    }
    rows
}

fn nodes_mtime(nodes: &[Node], path: &[Vec<u8>]) -> i64 {
    nodes.iter().find(|n| n.path == path).unwrap().mtime
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn p1_core_02_shuffled_scan_order_rolls_up_like_sorted_order(
        picks in prop::collection::vec((any::<u16>(), prop::bool::weighted(0.3), 0u16..5000, any::<u8>()), 0..300),
        seed in 1u64..u64::MAX,
    ) {
        let nodes = tree(&picks);
        let sorted = rolled(&nodes, events(&nodes, 0));
        let shuffled = rolled(&nodes, events(&nodes, seed));
        prop_assert_eq!(sorted, shuffled);
    }
}

#[test]
fn p1_core_02_a_file_root_is_numbered_alone() {
    let mut arena = Arena::new(Attrs::Basic);
    let node = Node {
        path: Vec::new(),
        dir: false,
        apparent: 10,
        allocated: 4096,
        mtime: 1,
    };
    arena
        .push(ScanEvent::Entry(entry(&node, DirId::ROOT, None)))
        .unwrap();
    let rollup = roll_up(&mut arena);
    assert_eq!(rollup.len(), 1);
    assert_eq!(rollup.entry_at(0), EntryIdx::ROOT);
}

#[test]
fn p1_core_02_entries_below_a_missing_directory_stay_out_of_the_totals() {
    // A truncated stream: the root holds a file and a directory, and the
    // entries of directory 2 arrive though its own entry never does.
    let node = |name: &str, dir: bool, apparent: u64| Node {
        path: vec![name.as_bytes().to_vec()],
        dir,
        apparent,
        allocated: apparent,
        mtime: 7,
    };
    let root = Node {
        path: Vec::new(),
        dir: true,
        apparent: 1,
        allocated: 1,
        mtime: 1,
    };
    let (missing, one) = (DirId::new(2), DirId::new(1));
    let mut arena = Arena::new(Attrs::Basic);
    for event in [
        ScanEvent::Entry(entry(&root, DirId::ROOT, Some(DirId::ROOT))),
        ScanEvent::Entry(entry(&node("a", false, 10), DirId::ROOT, None)),
        ScanEvent::Entry(entry(&node("d", true, 100), DirId::ROOT, Some(one))),
        ScanEvent::Entry(entry(&node("lost", false, 1000), missing, None)),
        ScanEvent::Entry(entry(
            &node("lost-dir", true, 1000),
            missing,
            Some(DirId::new(3)),
        )),
    ] {
        arena.push(event).unwrap();
    }
    let rollup = roll_up(&mut arena);
    assert_eq!(rollup.len(), 3, "the root, a and d are numbered");
    let rec = arena.dir(DirId::ROOT);
    assert_eq!((rec.cum_apparent, rec.cum_allocated), (111, 111));
    assert_eq!((rec.n_files, rec.n_dirs, rec.last_id), (1, 1, 2));
    assert_eq!(rec.max_mtime_ns, 7);
}

/// One directory holding `names` as files, delivered in the order given.
fn flat(names: &[Vec<u8>]) -> Arena {
    let mut arena = Arena::new(Attrs::Basic);
    let root = Node {
        path: Vec::new(),
        dir: true,
        apparent: 0,
        allocated: 0,
        mtime: 0,
    };
    arena
        .push(ScanEvent::Entry(entry(
            &root,
            DirId::ROOT,
            Some(DirId::ROOT),
        )))
        .unwrap();
    for name in names {
        let node = Node {
            path: vec![name.clone()],
            dir: false,
            apparent: 0,
            allocated: 0,
            mtime: 0,
        };
        arena
            .push(ScanEvent::Entry(entry(&node, DirId::ROOT, None)))
            .unwrap();
    }
    arena
        .push(ScanEvent::DirClosed {
            dir: DirId::ROOT,
            errno: None,
        })
        .unwrap();
    arena
}

/// T3's shape: one directory of 1,000,000 files named `f0000000` to
/// `f0999999`, delivered shuffled, as a hashed directory lists them. Timing,
/// so ignored; run it with `--release -- --ignored --nocapture`.
#[test]
#[ignore = "a timing; run in a release build by name"]
fn p1_core_02_a_million_siblings_roll_up_within_100_ms() {
    let mut names: Vec<Vec<u8>> = (0..1_000_000)
        .map(|i| format!("f{i:07}").into_bytes())
        .collect();
    let mut rng = 0x9e37_79b9_7f4a_7c15u64;
    for i in (1..names.len()).rev() {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        names.swap(i, usize::try_from(rng % (i as u64 + 1)).unwrap());
    }
    let mut arena = flat(&names);
    let started = std::time::Instant::now();
    let rollup = roll_up(&mut arena);
    let elapsed = started.elapsed();
    eprintln!("roll-up of 1,000,000 siblings: {elapsed:?}");
    assert_eq!(arena.entry(rollup.entry_at(1)).name, b"f0000000");
    assert_eq!(arena.entry(rollup.entry_at(1_000_000)).name, b"f0999999");
    assert!(elapsed.as_millis() < 100, "{elapsed:?}");
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// Siblings come out in bytewise order whatever leading run they share,
    /// whether one name begins another, and whether they differ only past
    /// the 8 bytes the sort key holds.
    #[test]
    fn p1_core_02_siblings_sort_bytewise_whatever_they_share(
        shared in prop::collection::vec(1u8..=255, 0..12),
        tails in prop::collection::vec(
            (
                prop::collection::vec(prop::sample::select(vec![b'a', b'b', 0x01, 0xfe]), 0..3),
                prop::collection::vec(1u8..=255, 0..12),
            ),
            1..60,
        ),
        seed in any::<u64>(),
    ) {
        // A shared run, then up to 2 bytes from a 4-letter alphabet (so many
        // names agree well past 8 bytes when the run is long), then any.
        let mut names: Vec<Vec<u8>> = tails
            .into_iter()
            .map(|(common, rest)| {
                let mut name = shared.clone();
                name.extend(common);
                name.extend(rest);
                name.retain(|&b| b != b'/');
                name
            })
            .filter(|name| !name.is_empty())
            .collect();
        names.sort();
        names.dedup();
        let mut delivered = names.clone();
        let mut rng = seed | 1;
        for i in (1..delivered.len()).rev() {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            delivered.swap(i, usize::try_from(rng % (i as u64 + 1)).unwrap());
        }
        let mut arena = flat(&delivered);
        let rollup = roll_up(&mut arena);
        let order: Vec<Vec<u8>> = rollup
            .preorder()
            .skip(1)
            .map(|idx| arena.entry(idx).name.to_vec())
            .collect();
        prop_assert_eq!(order, names);
    }
}
