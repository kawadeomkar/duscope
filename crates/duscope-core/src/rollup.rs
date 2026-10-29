//! The post-scan roll-up: pre-order ids and the directories' cumulative
//! fields.
//!
//! Siblings are ordered bytewise by name, files (every non-directory)
//! before subdirectories, so the id stream is canonical: two scans of an
//! unchanged tree number it the same whatever order the workers read it in,
//! which `diff` relies on. A directory's subtree is the contiguous range
//! `id ..= last_id`, and its own files are `id + 1 ..= files_end`.

use duscope_walk::DirId;

use crate::accounting::{Charges, charge};
use crate::arena::{Arena, DirRec, EntryIdx};

/// The pre-order numbering of an arena, and what charging decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollUp {
    /// The entry at each pre-order id.
    order: Vec<u32>,
    /// The pre-order id of each entry, by its scan-order index.
    ids: Vec<u32>,
    charges: Charges,
}

impl RollUp {
    /// Entries numbered.
    pub fn len(&self) -> usize {
        self.order.len()
    }

    /// Whether nothing was numbered.
    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// The entry with pre-order id `id`.
    pub fn entry_at(&self, id: u32) -> EntryIdx {
        EntryIdx::from_raw(self.order[id as usize])
    }

    /// The pre-order id of `entry`.
    pub fn id_of(&self, entry: EntryIdx) -> u32 {
        self.ids[entry.index()]
    }

    /// Every entry in pre-order.
    pub fn preorder(&self) -> impl Iterator<Item = EntryIdx> + '_ {
        self.order.iter().map(|&raw| EntryIdx::from_raw(raw))
    }

    /// How hard links and clones were charged.
    pub fn charges(&self) -> &Charges {
        &self.charges
    }
}

/// Numbers the arena's entries in pre-order, fills every directory's
/// cumulative fields — totals of apparent and allocated bytes (the
/// directory itself included), file and directory counts, the recursive
/// `max_mtime`, `last_id`, `files_end` — and charges hard links and clones
/// once ([`charge`]): a later link's bytes leave `cum_apparent` and
/// `cum_allocated`, a later clone's leave `cum_allocated`.
pub fn roll_up(arena: &mut Arena) -> RollUp {
    let mut rollup = number_and_total(arena);
    rollup.charges = charge(arena, &rollup);
    rollup
}

/// The roll-up with every entry's bytes counted.
fn number_and_total(arena: &mut Arena) -> RollUp {
    let n = arena.len();
    if n == 0 {
        return RollUp {
            order: Vec::new(),
            ids: Vec::new(),
            charges: Charges::default(),
        };
    }
    let children = Children::of(arena);
    let mut order = Vec::with_capacity(n);

    // Pre-order: a directory, its files in name order, then each
    // subdirectory's subtree in name order. The stack holds subdirectories
    // still to visit, last first.
    let mut stack: Vec<u32> = vec![EntryIdx::ROOT.get()];
    // One directory's files and subdirectories, as sort keys.
    let (mut files, mut subdirs) = (Vec::new(), Vec::new());
    // Each directory and its own id, in pre-order, for the cumulative pass.
    let mut dirs: Vec<(DirId, u32)> = Vec::with_capacity(arena.dir_count());
    while let Some(raw) = stack.pop() {
        let id = number(&mut order, raw);
        let Some(dir) = arena.record(EntryIdx::from_raw(raw)).dir() else {
            continue;
        };
        // Each part of the children sorted by name, in 16 bytes a child of
        // this directory alone.
        let key = |&entry: &u32| SortKey { tail: 0, entry };
        files.clear();
        files.extend(children.files(dir).iter().map(key));
        subdirs.clear();
        subdirs.extend(children.subdirs(dir).iter().map(key));
        SortKey::sort(&mut files, arena);
        SortKey::sort(&mut subdirs, arena);
        for key in &files {
            number(&mut order, key.entry);
        }
        stack.extend(subdirs.iter().rev().map(|key| key.entry));
        let files_end = id + u32::try_from(files.len()).unwrap_or(u32::MAX);
        arena.dir_mut(dir).files_end = files_end;
        dirs.push((dir, id));
    }
    // Each entry's id, made once the children index is gone, so the two
    // never take memory at the same time: a scan's peak is here.
    drop((children, files, subdirs, stack));
    let mut ids = vec![u32::MAX; n];
    for (id, &raw) in order.iter().enumerate() {
        ids[raw as usize] = u32::try_from(id).unwrap_or(u32::MAX);
    }

    // Each directory starts from its own entry: its sizes and its mtime.
    for &(dir, _) in &dirs {
        let own = arena.record(EntryIdx::from_raw(arena.dir(dir).entry));
        let ((apparent, allocated), mtime) = (own.sizes(), own.mtime_ns());
        let rec = arena.dir_mut(dir);
        rec.cum_apparent = apparent;
        rec.cum_allocated = allocated;
        rec.max_mtime_ns = mtime;
        rec.n_files = 0;
        rec.n_dirs = 0;
        rec.flags &= !DirRec::UPPER_BOUND;
    }
    // Every numbered entry below the root adds into its directory, in index
    // order, so the records are read one after another: a file its bytes,
    // its mtime and itself as a file, a subdirectory itself as a directory.
    let (records, mut dir_recs) = arena.records_and_dirs();
    for (own, &id) in records.zip(&ids) {
        if id == 0 || id == u32::MAX {
            continue;
        }
        let rec = dir_recs.get(own.parent());
        if own.dir().is_some() {
            rec.n_dirs = rec.n_dirs.saturating_add(1);
        } else {
            let (apparent, allocated) = own.sizes();
            rec.cum_apparent = rec.cum_apparent.saturating_add(apparent);
            rec.cum_allocated = rec.cum_allocated.saturating_add(allocated);
            rec.max_mtime_ns = rec.max_mtime_ns.max(own.mtime_ns());
            rec.n_files = rec.n_files.saturating_add(1);
        }
    }
    // Then each directory's subtree into its parent's, children before
    // parents: in reverse pre-order every directory is complete when it is
    // reached. Its subtree is the ids from its own to the last of its
    // descendants.
    for (k, &(dir, id)) in dirs.iter().enumerate().rev() {
        let rec = *arena.dir(dir);
        let last_id = id.saturating_add(rec.n_files).saturating_add(rec.n_dirs);
        arena.dir_mut(dir).last_id = last_id;
        // The root, first in pre-order, has no parent to add into.
        if k == 0 {
            continue;
        }
        let parent = arena.record(EntryIdx::from_raw(rec.entry)).parent();
        let up = arena.dir_mut(parent);
        up.cum_apparent = up.cum_apparent.saturating_add(rec.cum_apparent);
        up.cum_allocated = up.cum_allocated.saturating_add(rec.cum_allocated);
        up.max_mtime_ns = up.max_mtime_ns.max(rec.max_mtime_ns);
        up.n_files = up.n_files.saturating_add(rec.n_files);
        up.n_dirs = up.n_dirs.saturating_add(rec.n_dirs);
    }
    RollUp {
        order,
        ids,
        charges: Charges::default(),
    }
}

/// A child's place among siblings that are sorted together: 8 bytes of its
/// name as a big-endian number, then the entry. The 8 bytes follow the
/// leading run every one of those siblings shares (`IMG_2024_0` in
/// `IMG_2024_0000123.jpg`), zero-padded. Names hold no NUL byte, so equal
/// leading runs and padding leave the numbers ordered as the names are,
/// bytewise; siblings whose numbers are equal are compared by their whole
/// names. Most comparisons are then of two numbers, not two arena lookups:
/// one directory of 1,000,000 files rolled up in 176 ms comparing names.
#[derive(Clone, Copy)]
struct SortKey {
    tail: u64,
    entry: u32,
}

impl SortKey {
    /// Sorts `keys` by their entries' names, bytewise.
    fn sort(keys: &mut [SortKey], arena: &Arena) {
        let name = |key: &SortKey| arena.name(EntryIdx::from_raw(key.entry));
        let Some(first) = keys.first().map(name) else {
            return;
        };
        let mut shared = first.len();
        for key in keys.iter().skip(1) {
            if shared == 0 {
                break;
            }
            shared = first[..shared]
                .iter()
                .zip(name(key))
                .take_while(|(a, b)| a == b)
                .count();
        }
        for key in keys.iter_mut() {
            let tail = name(key).get(shared..).unwrap_or_default();
            let mut bytes = [0u8; 8];
            let len = tail.len().min(8);
            bytes[..len].copy_from_slice(&tail[..len]);
            key.tail = u64::from_be_bytes(bytes);
        }
        keys.sort_unstable_by(|a, b| a.tail.cmp(&b.tail).then_with(|| name(a).cmp(name(b))));
    }
}

/// Gives `raw` the next pre-order id.
fn number(order: &mut Vec<u32>, raw: u32) -> u32 {
    let id = u32::try_from(order.len()).unwrap_or(u32::MAX);
    order.push(raw);
    id
}

/// Each directory's children, as entry indexes, its files before its
/// subdirectories: a counting sort of the entries by parent, 4 bytes per
/// entry and 8 per directory, read from the records in index order.
struct Children {
    /// Where each directory's children start in `flat`; one more than the
    /// directories.
    starts: Vec<u32>,
    /// Where each directory's subdirectories start, after its files.
    subdirs_at: Vec<u32>,
    flat: Vec<u32>,
}

impl Children {
    fn of(arena: &Arena) -> Children {
        let dirs = arena.dir_count();
        let mut starts = vec![0u32; dirs + 1];
        for rec in arena.records().skip(1) {
            starts[rec.parent().index() + 1] += 1;
        }
        for i in 1..starts.len() {
            starts[i] += starts[i - 1];
        }
        // Files fill a directory's range from its start, subdirectories from
        // its end; where the two meet, the subdirectories start.
        let mut front = starts.clone();
        let mut back = starts[1..].to_vec();
        let mut flat = vec![0u32; arena.len().saturating_sub(1)];
        for (i, rec) in arena.records().enumerate().skip(1) {
            let dir = rec.parent().index();
            let slot = if rec.dir().is_some() {
                back[dir] -= 1;
                back[dir]
            } else {
                front[dir] += 1;
                front[dir] - 1
            };
            flat[slot as usize] = u32::try_from(i).unwrap_or(u32::MAX);
        }
        front.truncate(dirs);
        Children {
            starts,
            subdirs_at: front,
            flat,
        }
    }

    fn files(&self, dir: DirId) -> &[u32] {
        &self.flat[self.starts[dir.index()] as usize..self.subdirs_at[dir.index()] as usize]
    }

    fn subdirs(&self, dir: DirId) -> &[u32] {
        &self.flat[self.subdirs_at[dir.index()] as usize..self.starts[dir.index() + 1] as usize]
    }
}

#[cfg(test)]
mod tests;
