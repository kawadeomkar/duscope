//! `summary`'s bytes: under the root of a view, its totals as the scan
//! counts them and as `du` does (hard links once, clones each time), the
//! logical size of its dataless files, and each volume's share.
//!
//! A volume's share is what lies on it: the totals of each directory where
//! the volume starts under the root (the root's own volume starts at the
//! root), less those of the volumes that start below it.

use super::TreeView;

/// A directory where a volume starts, with its subtree's totals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VolumeRoot {
    /// Its id in the view.
    pub id: u32,
    /// The last id of its subtree.
    pub last: u32,
    /// The volume's device.
    pub dev: u64,
    /// Bytes on disk, hard links and clones counted once.
    pub allocated: u64,
    /// Apparent bytes.
    pub apparent: u64,
    /// Bytes on disk as `du` counts them: hard links once, clones each.
    pub du_compatible: u64,
}

/// The bytes one volume holds under the root.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VolumeBytes {
    /// The volume's device.
    pub dev: u64,
    /// Bytes on disk.
    pub allocated: u64,
    /// Apparent bytes.
    pub apparent: u64,
    /// Bytes as `du` counts them.
    pub du_compatible: u64,
}

/// What `summary` counts under the root of a view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryBytes {
    /// Bytes on disk.
    pub allocated: u64,
    /// Apparent bytes.
    pub apparent: u64,
    /// Bytes as `du` counts them.
    pub du_compatible: u64,
    /// The logical size of the dataless files (iCloud and other
    /// placeholders), which their allocated bytes leave out.
    pub dataless: u64,
    /// Each volume's share, one per device.
    pub volumes: Vec<VolumeBytes>,
}

/// The bytes under the root of `view`.
pub fn summary(view: &(impl TreeView + ?Sized)) -> SummaryBytes {
    if view.is_empty() {
        return SummaryBytes {
            allocated: 0,
            apparent: 0,
            du_compatible: 0,
            dataless: 0,
            volumes: Vec::new(),
        };
    }
    let roots = view.volume_roots(0);
    let mut volumes: Vec<VolumeBytes> = Vec::new();
    // The roots in id order, each less the roots directly below it.
    let mut open: Vec<(VolumeRoot, VolumeBytes)> = Vec::new();
    let close = |open: &mut Vec<(VolumeRoot, VolumeBytes)>, volumes: &mut Vec<VolumeBytes>| {
        if let Some((_, own)) = open.pop() {
            match volumes.iter_mut().find(|v| v.dev == own.dev) {
                Some(v) => {
                    v.allocated += own.allocated;
                    v.apparent += own.apparent;
                    v.du_compatible += own.du_compatible;
                }
                None => volumes.push(own),
            }
        }
    };
    for root in &roots {
        while open.last().is_some_and(|(outer, _)| root.id > outer.last) {
            close(&mut open, &mut volumes);
        }
        if let Some((_, outer)) = open.last_mut() {
            outer.allocated = outer.allocated.saturating_sub(root.allocated);
            outer.apparent = outer.apparent.saturating_sub(root.apparent);
            outer.du_compatible = outer.du_compatible.saturating_sub(root.du_compatible);
        }
        open.push((
            *root,
            VolumeBytes {
                dev: root.dev,
                allocated: root.allocated,
                apparent: root.apparent,
                du_compatible: root.du_compatible,
            },
        ));
    }
    while !open.is_empty() {
        close(&mut open, &mut volumes);
    }
    let top = roots.first();
    let node = view.node(0);
    SummaryBytes {
        allocated: node.allocated,
        apparent: node.apparent,
        du_compatible: top.map_or(node.allocated, |r| r.du_compatible),
        dataless: view.dataless_bytes(0),
        volumes,
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use duscope_walk::{Attrs, MemProvider, WalkOptions, walk_with};

    use super::{VolumeRoot, summary};
    use crate::arena::Arena;
    use crate::query::{ArenaView, TreeView};
    use crate::rollup::roll_up;

    /// A view of `inner`, whose volume roots are given.
    struct Roots<'v, V: TreeView> {
        inner: &'v V,
        roots: Vec<VolumeRoot>,
    }

    impl<V: TreeView> TreeView for Roots<'_, V> {
        fn len(&self) -> u32 {
            self.inner.len()
        }
        fn node(&self, id: u32) -> crate::query::Node {
            self.inner.node(id)
        }
        fn root_path(&self) -> &Path {
            self.inner.root_path()
        }
        fn full_attrs(&self) -> bool {
            false
        }
        fn name(&self, id: u32) -> std::borrow::Cow<'_, [u8]> {
            self.inner.name(id)
        }
        fn volume_roots(&self, _: u32) -> Vec<VolumeRoot> {
            self.roots.clone()
        }
    }

    #[test]
    fn p2_qry_03_a_volume_holds_what_starts_on_it_less_the_volumes_below() {
        let mut tree = MemProvider::new("/r");
        tree.file("a.bin", 1_000).file("m/b.bin", 2_000);
        let mut arena = Arena::new(Attrs::Basic);
        walk_with(&tree, tree.root_path(), &WalkOptions::default(), |e| {
            arena.push(e).unwrap();
        })
        .unwrap();
        let rollup = roll_up(&mut arena);
        let view = ArenaView::new(&arena, &rollup, "/r".into());
        // The root's volume, a second mounted inside it, a third inside
        // that, and the first again inside the third (a bind mount).
        let root = |id, last, dev, n| VolumeRoot {
            id,
            last,
            dev,
            allocated: n,
            apparent: n,
            du_compatible: n,
        };
        let roots = Roots {
            inner: &view,
            roots: vec![
                root(0, 9, 1, 1_000),
                root(2, 9, 2, 600),
                root(4, 6, 3, 100),
                root(5, 6, 1, 10),
                root(7, 9, 3, 50),
            ],
        };
        let bytes = summary(&roots);
        let shares: Vec<(u64, u64)> = bytes.volumes.iter().map(|v| (v.dev, v.allocated)).collect();
        // 1: 400 at the root, 10 in the bind mount; 2: 600 - 100 - 50;
        // 3: 90 and 50.
        assert_eq!(shares, [(1, 410), (3, 140), (2, 450)]);
        assert_eq!(
            bytes.volumes.iter().map(|v| v.allocated).sum::<u64>(),
            1_000
        );
    }
}
