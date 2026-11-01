//! P2-QRY-06: `diff` on trees in memory.

use duscope_walk::{Attrs, MemProvider, WalkOptions, walk_with};

use super::{DEFAULT_MIN_DELTA, DiffQuery, diff};
use crate::arena::Arena;
use crate::envelope::{DiffEntry, DiffTotals};
use crate::query::{ArenaView, Page, SizeKind};
use crate::rollup::{RollUp, roll_up};

const MIB: u64 = 1 << 20;
/// A MiB, as a delta.
const DELTA_MIB: i64 = 1 << 20;

fn scan(tree: &MemProvider) -> (Arena, RollUp) {
    let mut arena = Arena::new(Attrs::Basic);
    walk_with(tree, tree.root_path(), &WalkOptions::default(), |e| {
        arena.push(e).unwrap();
    })
    .unwrap();
    let rollup = roll_up(&mut arena);
    (arena, rollup)
}

fn query() -> DiffQuery {
    DiffQuery {
        min_delta: DEFAULT_MIN_DELTA,
        depth: None,
        size: SizeKind::Apparent,
        limit: None,
    }
}

fn run(old: &MemProvider, new: &MemProvider, q: &DiffQuery) -> (Page<DiffEntry>, DiffTotals) {
    let (oa, or) = scan(old);
    let (na, nr) = scan(new);
    let ov = ArenaView::new(&oa, &or, old.root_path().to_owned());
    let nv = ArenaView::new(&na, &nr, new.root_path().to_owned());
    diff(&ov, &nv, q)
}

/// Each row: its path under /r, its change, its delta.
fn rows(page: &Page<DiffEntry>) -> Vec<(String, &'static str, i64)> {
    page.entries
        .iter()
        .map(|e| {
            (
                e.path.strip_prefix("/r/").unwrap_or(&e.path).to_owned(),
                e.change,
                e.bytes_delta,
            )
        })
        .collect()
}

fn tree() -> MemProvider {
    let mut tree = MemProvider::new("/r");
    tree.file("docs/a.txt", 3 * MIB)
        .file("docs/b.txt", 5 * MIB)
        .file("docs/tiny.txt", 1_000)
        .file("old/x/1.bin", MIB)
        .file("old/x/2.bin", MIB)
        .file("photos/p.jpg", 4 * MIB)
        .file("thing", 10);
    tree
}

#[test]
fn p2_qry_06_an_unchanged_tree_diffs_to_nothing() {
    let (page, totals) = run(&tree(), &tree(), &query());
    assert!(page.entries.is_empty());
    assert_eq!(totals.net_bytes, 0);
    assert_eq!(
        (
            totals.entries_added,
            totals.entries_removed,
            totals.entries_changed
        ),
        (0, 0, 0)
    );
}

#[test]
fn p2_qry_06_a_scripted_change_set() {
    let old = tree();
    let mut new = MemProvider::new("/r");
    new.file("docs/a.txt", 5 * MIB) // grown by 2 MiB
        .file("docs/b.txt", 2 * MIB) // shrunk by 3 MiB
        .file("docs/tiny.txt", 2_000) // grown below --min-delta
        .file("docs/new.txt", 7 * MIB) // created
        .file("pictures/p.jpg", 4 * MIB) // photos/ renamed
        .file("thing/inside", 20) // a file become a directory
        .link("docs/again.txt", "a.txt"); // a symbolic link, of 5 bytes
    // old/ deleted, two files and a directory in it.
    let (page, totals) = run(&old, &new, &query());
    assert_eq!(
        rows(&page),
        [
            ("docs/new.txt".to_owned(), "added", 7 * DELTA_MIB),
            // Equal changes by path.
            ("photos".to_owned(), "removed", -4 * DELTA_MIB),
            ("pictures".to_owned(), "added", 4 * DELTA_MIB),
            ("docs/b.txt".to_owned(), "shrunk", -3 * DELTA_MIB),
            ("docs/a.txt".to_owned(), "grown", 2 * DELTA_MIB),
            ("old".to_owned(), "removed", -2 * DELTA_MIB),
            ("thing".to_owned(), "kind_changed", 10),
            ("docs/again.txt".to_owned(), "added", 5),
        ]
    );
    let old_row = page.entries.iter().find(|e| e.path == "/r/old").unwrap();
    // old/, old/x/ and its two files.
    assert_eq!(old_row.collapsed_count, Some(4));
    assert_eq!(old_row.new_bytes, None);
    // The tiny growth is counted though no row shows it.
    assert_eq!(totals.entries_changed, 3);
    assert_eq!(totals.added_bytes, 11 * MIB + 20 + 5);
}

#[test]
fn p2_qry_06_depth_attributes_deeper_changes_to_the_directory_at_it() {
    let old = tree();
    let mut new = tree();
    new.file("docs/c.txt", 2 * MIB)
        .file("photos/deeper/q.jpg", 3 * MIB);
    let q = DiffQuery {
        depth: Some(1),
        ..query()
    };
    let (page, _) = run(&old, &new, &q);
    assert_eq!(
        rows(&page),
        [
            ("photos".to_owned(), "grown", 3 * DELTA_MIB),
            ("docs".to_owned(), "grown", 2 * DELTA_MIB),
        ]
    );
    // Without --depth, the files themselves.
    let (page, _) = run(&old, &new, &query());
    assert_eq!(
        rows(&page),
        [
            ("photos/deeper".to_owned(), "added", 3 * DELTA_MIB),
            ("docs/c.txt".to_owned(), "added", 2 * DELTA_MIB),
        ]
    );
}

#[test]
fn p2_qry_06_limit_keeps_the_largest_and_counts_all() {
    let old = tree();
    let mut new = tree();
    new.file("a.bin", MIB)
        .file("b.bin", 2 * MIB)
        .file("c.bin", 2 * MIB);
    let q = DiffQuery {
        limit: Some(2),
        ..query()
    };
    let (page, _) = run(&old, &new, &q);
    // Equal changes by path.
    assert_eq!(
        rows(&page),
        [
            ("b.bin".to_owned(), "added", 2 * DELTA_MIB),
            ("c.bin".to_owned(), "added", 2 * DELTA_MIB),
        ]
    );
    assert_eq!(page.total_count, 3);
    assert!(page.truncated);
}
