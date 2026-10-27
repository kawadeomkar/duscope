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

use std::collections::{BTreeMap, HashMap, HashSet};

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

/// What charging decided.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Charges {
    /// Entries whose bytes are not in their ancestors' `cum_allocated`.
    uncharged: HashSet<EntryIdx>,
    /// Entries uncharged as later links of an inode, whose apparent size
    /// is also left out of `cum_apparent`.
    linked_away: HashSet<EntryIdx>,
    hardlinks: Vec<HardlinkGroup>,
    clones: Vec<CloneGroup>,
    /// `cum_du_compatible` of the directories where it differs from
    /// `cum_allocated`: the ancestors of an uncharged clone.
    du_compatible: HashMap<DirId, u64>,
}

impl Charges {
    /// Whether `entry`'s bytes count in its ancestors' totals.
    pub fn charged(&self, entry: EntryIdx) -> bool {
        !self.uncharged.contains(&entry)
    }

    /// The hard-linked inodes, by device and inode number.
    pub fn hardlinks(&self) -> &[HardlinkGroup] {
        &self.hardlinks
    }

    /// The clone groups, by device and clone id.
    pub fn clones(&self) -> &[CloneGroup] {
        &self.clones
    }

    /// The directory's allocated bytes deduplicated by hard link but not by
    /// clone: what `du` reports, on APFS and ext4 alike.
    pub fn du_compatible(&self, arena: &Arena, dir: DirId) -> u64 {
        self.du_compatible
            .get(&dir)
            .copied()
            .unwrap_or_else(|| arena.dir(dir).cum_allocated)
    }
}

/// Charges hard links and clones once, adjusting the cumulative fields the
/// roll-up computed with every entry's bytes counted.
pub fn charge(arena: &mut Arena, rollup: &RollUp) -> Charges {
    let mut charges = Charges::default();
    // Inode numbers are not stable across an overlay's layers, so its
    // links are not deduplicated.
    let overlays: HashSet<u64> = arena
        .volumes()
        .iter()
        .filter(|v| v.class == VolumeClass::Overlay)
        .map(|v| v.dev)
        .collect();

    let mut inodes: BTreeMap<(u64, u64), Vec<EntryIdx>> = BTreeMap::new();
    for link in arena.linked() {
        if !overlays.contains(&link.dev) {
            inodes
                .entry((link.dev, link.ino))
                .or_default()
                .push(link.entry);
        }
    }
    for ((dev, ino), mut sites) in inodes {
        sites.sort_by_key(|&site| rollup.id_of(site));
        let nlink = arena.entry(sites[0]).nlink;
        for &later in &sites[1..] {
            charges.uncharged.insert(later);
            charges.linked_away.insert(later);
        }
        charges.hardlinks.push(HardlinkGroup {
            dev,
            ino,
            nlink,
            sites,
        });
    }

    let mut groups: BTreeMap<(u64, u64), Vec<EntryIdx>> = BTreeMap::new();
    for clone in arena.cloned() {
        // A later link of a cloned inode is uncharged already.
        if !charges.linked_away.contains(&clone.entry) {
            groups
                .entry((clone.dev, clone.clone_id))
                .or_default()
                .push(clone.entry);
        }
    }
    let mut clone_uncharged = Vec::new();
    for ((dev, clone_id), mut members) in groups {
        members.sort_by_key(|&member| rollup.id_of(member));
        for &later in &members[1..] {
            // A member that no longer shares every block keeps its own
            // bytes; what it still shares makes the totals an upper bound.
            if arena.entry(later).flags.contains(Flags::SHARES_ALL) {
                charges.uncharged.insert(later);
                clone_uncharged.push(later);
            } else {
                mark_upper_bound(arena, later);
            }
        }
        charges.clones.push(CloneGroup {
            dev,
            clone_id,
            members,
        });
    }
    for &entry in &arena.may_share().to_vec() {
        mark_upper_bound(arena, entry);
    }

    // Take the uncharged bytes out of every ancestor. The links' first:
    // their bytes leave cum_du_compatible too, which is cum_allocated
    // before the clones' come out.
    let linked: Vec<EntryIdx> = charges.linked_away.iter().copied().collect();
    for entry in linked {
        let view = arena.entry(entry);
        let (apparent, allocated) = (view.apparent, view.allocated);
        for dir in ancestors(arena, entry) {
            let rec = arena.dir_mut(dir);
            rec.cum_apparent = rec.cum_apparent.saturating_sub(apparent);
            rec.cum_allocated = rec.cum_allocated.saturating_sub(allocated);
        }
    }
    for &entry in &clone_uncharged {
        for dir in ancestors(arena, entry) {
            let cum = arena.dir(dir).cum_allocated;
            charges.du_compatible.entry(dir).or_insert(cum);
        }
    }
    for entry in clone_uncharged {
        let allocated = arena.entry(entry).allocated;
        for dir in ancestors(arena, entry) {
            let rec = arena.dir_mut(dir);
            rec.cum_allocated = rec.cum_allocated.saturating_sub(allocated);
        }
    }
    charges
}

/// The directories above `entry`, its parent first, up to the root; none
/// when the root is not a directory.
fn ancestors(arena: &Arena, entry: EntryIdx) -> Vec<DirId> {
    let mut dirs = Vec::new();
    if arena.dir_count() == 0 {
        return dirs;
    }
    let mut dir = arena.entry(entry).parent;
    loop {
        dirs.push(dir);
        if dir == DirId::ROOT {
            return dirs;
        }
        dir = arena.entry(EntryIdx::from_raw(arena.dir(dir).entry)).parent;
    }
}

/// Marks every directory above `entry` as an upper bound.
fn mark_upper_bound(arena: &mut Arena, entry: EntryIdx) {
    for dir in ancestors(arena, entry) {
        arena.dir_mut(dir).flags |= DirRec::UPPER_BOUND;
    }
}

#[cfg(test)]
mod tests;
