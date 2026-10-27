//! Charging shared blocks once: hard links and APFS clones.
//!
//! A hard-linked non-directory (`nlink > 1`, GNU du's rule) is one inode
//! reached from several places: its bytes are charged to the link with the
//! lowest pre-order id, so two scans of an unchanged tree charge the same
//! directory and `diff` reports no phantom moves. A clone group (one APFS
//! clone id) is charged the same way: the lowest member carries the bytes,
//! and every other member that shares all its blocks with the group is not
//! charged. A file that may share blocks but names no group makes its
//! ancestors' totals an upper bound.
//!
//! Every link site is recorded, for `explain`'s `links_outside_subtree`.

use std::collections::{HashMap, HashSet};
use std::mem::size_of;

use duscope_walk::{DirId, Flags, VolumeClass};

use crate::arena::{Arena, DirRec, EntryIdx};
use crate::rollup::RollUp;

/// The link sites of one hard-linked inode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HardlinkGroup {
    /// The inode's device.
    pub dev: u64,
    /// The inode number.
    pub ino: u64,
    /// The link count the filesystem reported.
    pub nlink: u32,
    /// Every link site the scan saw, lowest pre-order id first; the first
    /// carries the bytes.
    pub sites: Vec<EntryIdx>,
}

/// The members of one clone group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloneGroup {
    /// The members' device.
    pub dev: u64,
    /// The clone id.
    pub clone_id: u64,
    /// Every member, lowest pre-order id first; the first carries the bytes.
    pub members: Vec<EntryIdx>,
}

/// What charging decided. It keeps 4 bytes per hard link and per clone,
/// and a bit per entry: the groups are read back from the arena.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Charges {
    /// Entries whose bytes are not in their ancestors' `cum_allocated`.
    uncharged: EntrySet,
    /// Entries uncharged as later links of an inode, whose apparent size
    /// is also left out of `cum_apparent`.
    linked_away: EntrySet,
    /// Indexes of the arena's hard-linked files, by device, inode number
    /// and pre-order id; an overlay's are left out.
    links: Vec<u32>,
    /// Indexes of the arena's clone members, by device, clone id and
    /// pre-order id; later links of an inode are left out.
    clones: Vec<u32>,
    /// `cum_du_compatible` of the directories where it differs from
    /// `cum_allocated`: the ancestors of an uncharged clone.
    du_compatible: HashMap<DirId, u64>,
}

impl Charges {
    /// Whether `entry`'s bytes count in its ancestors' totals.
    pub fn charged(&self, entry: EntryIdx) -> bool {
        !self.uncharged.contains(entry)
    }

    /// The hard-linked inodes, by device and inode number.
    pub fn hardlinks<'a>(&'a self, arena: &'a Arena) -> impl Iterator<Item = HardlinkGroup> + 'a {
        groups(&self.links, move |i| {
            let link = arena.linked_at(i);
            ((link.dev, link.ino), link.entry)
        })
        .map(|((dev, ino), sites)| HardlinkGroup {
            dev,
            ino,
            nlink: arena.entry(sites[0]).nlink,
            sites,
        })
    }

    /// The clone groups, by device and clone id.
    pub fn clones<'a>(&'a self, arena: &'a Arena) -> impl Iterator<Item = CloneGroup> + 'a {
        groups(&self.clones, move |i| {
            let clone = arena.cloned_at(i);
            ((clone.dev, clone.clone_id), clone.entry)
        })
        .map(|((dev, clone_id), members)| CloneGroup {
            dev,
            clone_id,
            members,
        })
    }

    /// The directory's allocated bytes deduplicated by hard link but not by
    /// clone: what `du` reports, on APFS and ext4 alike.
    pub fn du_compatible(&self, arena: &Arena, dir: DirId) -> u64 {
        self.du_compatible
            .get(&dir)
            .copied()
            .unwrap_or_else(|| arena.dir(dir).cum_allocated)
    }

    /// The bytes the charges hold on the heap.
    pub fn heap_bytes(&self) -> usize {
        self.uncharged.heap_bytes()
            + self.linked_away.heap_bytes()
            + (self.links.capacity() + self.clones.capacity()) * size_of::<u32>()
            + self.du_compatible.capacity() * size_of::<(DirId, u64)>()
    }
}

/// A set of entries, a bit each.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct EntrySet(Vec<u64>);

impl EntrySet {
    /// An empty set for an arena of `len` entries.
    fn new(len: usize) -> EntrySet {
        EntrySet(vec![0; len.div_ceil(64)])
    }

    fn insert(&mut self, entry: EntryIdx) {
        let i = entry.index();
        if let Some(word) = self.0.get_mut(i / 64) {
            *word |= 1 << (i % 64);
        }
    }

    fn contains(&self, entry: EntryIdx) -> bool {
        let i = entry.index();
        self.0
            .get(i / 64)
            .is_some_and(|word| word & (1 << (i % 64)) != 0)
    }

    fn heap_bytes(&self) -> usize {
        self.0.capacity() * size_of::<u64>()
    }
}

/// The runs of `order` that share a key, with their entries: `item` gives
/// an index's key and entry.
fn groups<'a>(
    order: &'a [u32],
    item: impl Fn(usize) -> ((u64, u64), EntryIdx) + 'a,
) -> impl Iterator<Item = ((u64, u64), Vec<EntryIdx>)> + 'a {
    let mut rest = order;
    std::iter::from_fn(move || {
        let (key, _) = item(*rest.first()? as usize);
        let len = rest
            .iter()
            .take_while(|&&i| item(i as usize).0 == key)
            .count();
        let entries = rest[..len].iter().map(|&i| item(i as usize).1).collect();
        rest = &rest[len..];
        Some((key, entries))
    })
}

/// `0..len` as `u32` indexes; the arena holds fewer than 2^32 entries.
fn indexes(len: usize) -> impl Iterator<Item = u32> {
    (0..u32::try_from(len).unwrap_or(u32::MAX)).take(len)
}

/// Charges hard links and clones once, adjusting the cumulative fields the
/// roll-up computed with every entry's bytes counted.
pub fn charge(arena: &mut Arena, rollup: &RollUp) -> Charges {
    let mut charges = Charges {
        uncharged: EntrySet::new(arena.len()),
        linked_away: EntrySet::new(arena.len()),
        ..Charges::default()
    };
    // Inode numbers are not stable across an overlay's layers, so its
    // links are not deduplicated.
    let overlays: HashSet<u64> = arena
        .volumes()
        .iter()
        .filter(|v| v.class == VolumeClass::Overlay)
        .map(|v| v.dev)
        .collect();

    // Every link of an inode after its first leaves its ancestors'
    // cum_apparent and cum_allocated, and so cum_du_compatible, which is
    // cum_allocated before the clones' bytes come out.
    let mut links = Vec::with_capacity(arena.linked_len());
    links.extend(
        indexes(arena.linked_len())
            .filter(|&i| !overlays.contains(&arena.linked_at(i as usize).dev)),
    );
    links.sort_unstable_by_key(|&i| {
        let link = arena.linked_at(i as usize);
        (link.dev, link.ino, rollup.id_of(link.entry))
    });
    let mut previous = None;
    for &i in &links {
        let link = arena.linked_at(i as usize);
        if previous.replace((link.dev, link.ino)) == Some((link.dev, link.ino)) {
            charges.uncharged.insert(link.entry);
            charges.linked_away.insert(link.entry);
            let view = arena.entry(link.entry);
            let (apparent, allocated) = (view.apparent, view.allocated);
            for_each_ancestor(arena, link.entry, |_, rec| {
                rec.cum_apparent = rec.cum_apparent.saturating_sub(apparent);
                rec.cum_allocated = rec.cum_allocated.saturating_sub(allocated);
            });
        }
    }
    charges.links = links;

    // A later link of a cloned inode is uncharged already.
    let mut clones = Vec::with_capacity(arena.cloned_len());
    clones.extend(indexes(arena.cloned_len()).filter(|&i| {
        !charges
            .linked_away
            .contains(arena.cloned_at(i as usize).entry)
    }));
    clones.sort_unstable_by_key(|&i| {
        let clone = arena.cloned_at(i as usize);
        (clone.dev, clone.clone_id, rollup.id_of(clone.entry))
    });
    let mut clone_uncharged = Vec::new();
    let mut previous = None;
    for &i in &clones {
        let clone = arena.cloned_at(i as usize);
        if previous.replace((clone.dev, clone.clone_id)) != Some((clone.dev, clone.clone_id)) {
            continue;
        }
        // A member that no longer shares every block keeps its own
        // bytes; what it still shares makes the totals an upper bound.
        if arena.entry(clone.entry).flags.contains(Flags::SHARES_ALL) {
            charges.uncharged.insert(clone.entry);
            clone_uncharged.push(clone.entry);
        } else {
            mark_upper_bound(arena, clone.entry);
        }
    }
    charges.clones = clones;
    let may_share: Vec<EntryIdx> = arena.may_share().collect();
    for entry in may_share {
        mark_upper_bound(arena, entry);
    }

    // The uncharged clones' ancestors keep cum_du_compatible as it is now,
    // then their bytes leave cum_allocated.
    for &entry in &clone_uncharged {
        for_each_ancestor(arena, entry, |dir, rec| {
            charges
                .du_compatible
                .entry(dir)
                .or_insert(rec.cum_allocated);
        });
    }
    for entry in clone_uncharged {
        let allocated = arena.entry(entry).allocated;
        for_each_ancestor(arena, entry, |_, rec| {
            rec.cum_allocated = rec.cum_allocated.saturating_sub(allocated);
        });
    }
    charges
}

/// Calls `f` on every directory above `entry`, its parent first, up to the
/// root; on none when the root is not a directory.
fn for_each_ancestor(arena: &mut Arena, entry: EntryIdx, mut f: impl FnMut(DirId, &mut DirRec)) {
    if arena.dir_count() == 0 {
        return;
    }
    let mut dir = arena.entry(entry).parent;
    loop {
        f(dir, arena.dir_mut(dir));
        if dir == DirId::ROOT {
            return;
        }
        dir = arena.entry(EntryIdx::from_raw(arena.dir(dir).entry)).parent;
    }
}

/// Marks every directory above `entry` as an upper bound.
fn mark_upper_bound(arena: &mut Arena, entry: EntryIdx) {
    for_each_ancestor(arena, entry, |_, rec| rec.flags |= DirRec::UPPER_BOUND);
}

#[cfg(test)]
mod tests;
