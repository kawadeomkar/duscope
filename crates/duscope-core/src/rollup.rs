//! The post-scan roll-up: pre-order ids and the directories' cumulative
//! fields.
//!
//! Siblings are ordered bytewise by name, files (every non-directory)
//! before subdirectories, so the id stream is canonical: two scans of an
//! unchanged tree number it the same whatever order the workers read it in,
//! which `diff` relies on. A directory's subtree is the contiguous range
//! `id ..= last_id`, and its own files are `id + 1 ..= files_end`.

use duscope_walk::{DirId, Kind};

use crate::arena::{Arena, DirRec, EntryIdx};

/// The pre-order numbering of an arena.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollUp {
    /// The entry at each pre-order id.
    order: Vec<u32>,
    /// The pre-order id of each entry, by its scan-order index.
    ids: Vec<u32>,
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
}

/// Numbers the arena's entries in pre-order and fills every directory's
/// cumulative fields: totals of apparent and allocated bytes (the directory
/// itself included), file and directory counts, the recursive `max_mtime`,
/// `last_id`, `files_end`. Every entry's bytes count here; hard links and
/// clones are charged once by the accounting that follows (P1-CORE-03).
pub fn roll_up(arena: &mut Arena) -> RollUp {
    let n = arena.len();
    if n == 0 {
        return RollUp {
            order: Vec::new(),
            ids: Vec::new(),
        };
    }
    let children = Children::of(arena);
    let mut order = Vec::with_capacity(n);
    let mut ids = vec![u32::MAX; n];

    // Pre-order: a directory, its files in name order, then each
    // subdirectory's subtree in name order. The stack holds subdirectories
    // still to visit, last first.
    let mut stack: Vec<u32> = vec![EntryIdx::ROOT.get()];
    let mut sorted = Vec::new();
    // Each directory and its own id, for the cumulative pass.
    let mut dirs: Vec<(DirId, u32)> = Vec::with_capacity(arena.dir_count());
    while let Some(raw) = stack.pop() {
        let view = arena.entry(EntryIdx::from_raw(raw));
        let id = number(&mut order, &mut ids, raw);
        let Some(dir) = view.dir else {
            continue;
        };
        sorted.clear();
        sorted.extend_from_slice(children.of_dir(dir));
        sorted.sort_unstable_by(|&a, &b| {
            let (a, b) = (
                arena.entry(EntryIdx::from_raw(a)),
                arena.entry(EntryIdx::from_raw(b)),
            );
            (a.kind == Kind::Dir, a.name).cmp(&(b.kind == Kind::Dir, b.name))
        });
        let files = sorted
            .iter()
            .take_while(|&&c| arena.entry(EntryIdx::from_raw(c)).kind != Kind::Dir)
            .count();
        for &file in &sorted[..files] {
            number(&mut order, &mut ids, file);
        }
        let files_end = id + u32::try_from(files).unwrap_or(u32::MAX);
        arena.dir_mut(dir).files_end = files_end;
        dirs.push((dir, id));
        stack.extend(sorted[files..].iter().rev());
    }

    // Each directory starts from its own entry: its sizes, its mtime, its
    // own id as the end of its range.
    for &(dir, id) in &dirs {
        let own = arena.entry(EntryIdx::from_raw(arena.dir(dir).entry));
        let (apparent, allocated, mtime) = (own.apparent, own.allocated, own.mtime_ns);
        let rec = arena.dir_mut(dir);
        rec.cum_apparent = apparent;
        rec.cum_allocated = allocated;
        rec.max_mtime_ns = mtime;
        rec.n_files = 0;
        rec.n_dirs = 0;
        rec.last_id = id;
        rec.flags &= !DirRec::UPPER_BOUND;
    }
    // Then children before parents: walking the ids backwards, every
    // directory is complete when it is added into its parent.
    for id in (1..order.len()).rev() {
        let view = arena.entry(EntryIdx::from_raw(order[id]));
        let parent = view.parent;
        let (apparent, allocated, mtime, files, dirs, last) = match view.dir {
            Some(dir) => {
                let rec = *arena.dir(dir);
                (
                    rec.cum_apparent,
                    rec.cum_allocated,
                    rec.max_mtime_ns,
                    rec.n_files,
                    rec.n_dirs + 1,
                    rec.last_id,
                )
            }
            None => (
                view.apparent,
                view.allocated,
                view.mtime_ns,
                1,
                0,
                u32::try_from(id).unwrap_or(u32::MAX),
            ),
        };
        let rec = arena.dir_mut(parent);
        rec.cum_apparent = rec.cum_apparent.saturating_add(apparent);
        rec.cum_allocated = rec.cum_allocated.saturating_add(allocated);
        rec.max_mtime_ns = rec.max_mtime_ns.max(mtime);
        rec.n_files = rec.n_files.saturating_add(files);
        rec.n_dirs = rec.n_dirs.saturating_add(dirs);
        rec.last_id = rec.last_id.max(last);
    }
    RollUp { order, ids }
}

/// Gives `raw` the next pre-order id.
fn number(order: &mut Vec<u32>, ids: &mut [u32], raw: u32) -> u32 {
    let id = u32::try_from(order.len()).unwrap_or(u32::MAX);
    order.push(raw);
    ids[raw as usize] = id;
    id
}

/// Each directory's children, as entry indexes: a counting sort of the
/// entries by parent, 4 bytes per entry and per directory.
struct Children {
    /// Where each directory's children start in `flat`; one more than the
    /// directories.
    starts: Vec<u32>,
    flat: Vec<u32>,
}

impl Children {
    fn of(arena: &Arena) -> Children {
        let dirs = arena.dir_count();
        let mut starts = vec![0u32; dirs + 1];
        for view in arena.entries().skip(1) {
            starts[view.parent.index() + 1] += 1;
        }
        for i in 1..starts.len() {
            starts[i] += starts[i - 1];
        }
        let mut next = starts.clone();
        let mut flat = vec![0u32; arena.len().saturating_sub(1)];
        for view in arena.entries().skip(1) {
            let slot = &mut next[view.parent.index()];
            flat[*slot as usize] = view.idx.get();
            *slot += 1;
        }
        Children { starts, flat }
    }

    fn of_dir(&self, dir: DirId) -> &[u32] {
        let start = self.starts[dir.index()] as usize;
        let end = self.starts[dir.index() + 1] as usize;
        &self.flat[start..end]
    }
}

#[cfg(test)]
mod tests;
