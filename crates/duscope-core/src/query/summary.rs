//! `summary`'s bytes: under the root of a view, its totals as the scan
//! counts them and as `du` does (hard links once, clones each time), the
//! logical size of its dataless files, and each volume's share.
//!
//! A volume's share is what lies on it: the totals of each directory where
//! the volume starts under the root (the root's own volume starts at the
//! root), less those of the volumes that start below it. A mount point the
//! scan did not enter starts nothing: its directory's own bytes stay in the
//! volume around it.
//!
//! The sidebar of subvolumes (btrfs subvolumes, ZFS datasets) counts each
//! the same way: its totals, less those of the subvolumes and the volumes
//! that start below it. A file read alone starts nothing: its bytes lie on
//! its directory's volume.

use std::path::Path;

use super::{Node, TreeView};

/// A directory where a volume starts, with its subtree's totals; or a file
/// read alone ([`file_root`]).
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

/// The volume root of `file` when it is the root of a view: the file
/// itself, on its directory's device `dev`, its bytes as `du` counts them
/// its allocated ones. `None` when the device could not be read.
pub fn file_root(file: &Node, dev: Option<u64>) -> Option<VolumeRoot> {
    dev.map(|dev| VolumeRoot {
        id: file.id,
        last: file.id,
        dev,
        allocated: file.allocated,
        apparent: file.apparent,
        du_compatible: file.allocated,
    })
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

/// The bytes one subvolume holds under the root.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SubvolumeBytes {
    /// The directory where it starts, in the view.
    pub id: u32,
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
    /// Each subvolume's share, in id order.
    pub subvolumes: Vec<SubvolumeBytes>,
}

/// The bytes under the root of `view`; `unentered` names the devices of the
/// mount points the scan did not enter.
pub fn summary(view: &(impl TreeView + ?Sized), unentered: &[u64]) -> SummaryBytes {
    if view.is_empty() {
        return SummaryBytes {
            allocated: 0,
            apparent: 0,
            du_compatible: 0,
            dataless: 0,
            volumes: Vec::new(),
            subvolumes: Vec::new(),
        };
    }
    let node = view.node(0);
    let roots = view.volume_roots(0);
    SummaryBytes {
        allocated: node.allocated,
        apparent: node.apparent,
        du_compatible: roots.first().map_or(node.allocated, |r| r.du_compatible),
        dataless: view.dataless_bytes(0),
        volumes: shares(&roots, unentered),
        subvolumes: subvolume_shares(&view.subvolume_roots(0), &roots, unentered),
    }
}

/// Each subvolume's share of `subvolumes`: its totals less those of the
/// subvolumes and the entered volumes (`volumes`, the root's own first,
/// which starts nothing below the root) that start directly below it. A
/// subvolume mounted on its own is in both lists, once.
pub fn subvolume_shares(
    subvolumes: &[VolumeRoot],
    volumes: &[VolumeRoot],
    unentered: &[u64],
) -> Vec<SubvolumeBytes> {
    let mut starts: Vec<(VolumeRoot, bool)> = subvolumes.iter().map(|r| (*r, true)).collect();
    starts.extend(
        volumes
            .iter()
            .skip(1)
            .filter(|r| !unentered.contains(&r.dev) && !subvolumes.iter().any(|s| s.id == r.id))
            .map(|r| (*r, false)),
    );
    starts.sort_unstable_by_key(|(r, _)| r.id);
    let mut out = Vec::new();
    // The starts in id order, each less the starts directly below it.
    let mut open: Vec<(VolumeRoot, bool, SubvolumeBytes)> = Vec::new();
    let close = |open: &mut Vec<(VolumeRoot, bool, SubvolumeBytes)>,
                 out: &mut Vec<SubvolumeBytes>| {
        if let Some((_, true, own)) = open.pop() {
            out.push(own);
        }
    };
    for (root, subvolume) in starts {
        while open
            .last()
            .is_some_and(|(outer, _, _)| root.id > outer.last)
        {
            close(&mut open, &mut out);
        }
        if let Some((_, _, outer)) = open.last_mut() {
            outer.allocated = outer.allocated.saturating_sub(root.allocated);
            outer.apparent = outer.apparent.saturating_sub(root.apparent);
            outer.du_compatible = outer.du_compatible.saturating_sub(root.du_compatible);
        }
        let own = SubvolumeBytes {
            id: root.id,
            allocated: root.allocated,
            apparent: root.apparent,
            du_compatible: root.du_compatible,
        };
        open.push((root, subvolume, own));
    }
    while !open.is_empty() {
        close(&mut open, &mut out);
    }
    out.sort_unstable_by_key(|s| s.id);
    out
}

/// Each volume's share of `roots` (the root first), one per device; the
/// roots on an `unentered` device start nothing.
pub fn shares(roots: &[VolumeRoot], unentered: &[u64]) -> Vec<VolumeBytes> {
    let roots: Vec<&VolumeRoot> = roots
        .iter()
        .enumerate()
        .filter(|(i, r)| *i == 0 || !unentered.contains(&r.dev))
        .map(|(_, r)| r)
        .collect();
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
    for root in roots {
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
    volumes
}

/// What a filesystem says the subvolume mounted on its own with `total`
/// and `free` bytes holds itself: a ZFS dataset reports its referenced
/// bytes and its available ones as its size and its free space. `None`
/// elsewhere: a btrfs subvolume reports the whole filesystem's.
pub fn referenced(fs_type: &str, total: u64, free: u64) -> Option<u64> {
    (fs_type == "zfs" && total > 0).then(|| total.saturating_sub(free))
}

/// A volume as [`unaccounted`] weighs it.
#[derive(Debug, Clone, Copy)]
pub struct VolumeUse<'a> {
    /// Where it is mounted.
    pub mount_point: &'a Path,
    /// Its filesystem.
    pub fs_type: &'a str,
    /// Its size, as `statfs` reported it.
    pub total: Option<u64>,
    /// Its free space.
    pub free: Option<u64>,
    /// Whether the scan entered it.
    pub entered: bool,
    /// Its mount options.
    pub options: &'a [String],
    /// The bytes the scan found on it.
    pub found: u64,
}

/// What the volume mounted at `root` uses that the scan did not find: its
/// size less its free space, less the bytes found on it and on every
/// volume that reports the same filesystem (the volumes of one APFS
/// container, or bind mounts of one filesystem, report its size and free
/// space alike). `None` where `root` is no entered volume's mount point,
/// the sizes are unknown, or the mount is a btrfs subvolume's, whose size
/// and free space are the whole filesystem's.
pub fn unaccounted(root: &Path, volumes: &[VolumeUse<'_>]) -> Option<u64> {
    let mount = volumes
        .iter()
        .find(|v| v.entered && v.mount_point == root)?;
    let subvolume = mount
        .options
        .iter()
        .filter_map(|o| o.strip_prefix("subvol="))
        .any(|s| s != "/");
    if subvolume {
        return None;
    }
    let (total, free) = (mount.total?, mount.free?);
    let used = total.checked_sub(free)?;
    let found: u64 = volumes
        .iter()
        .filter(|v| {
            v.entered
                && v.fs_type == mount.fs_type
                && v.total == mount.total
                && v.free == mount.free
        })
        .map(|v| v.found)
        .sum();
    Some(used.saturating_sub(found))
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
        let bytes = summary(&roots, &[]);
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

#[cfg(test)]
mod unaccounted_tests {
    use std::path::Path;

    use super::{VolumeUse, unaccounted};

    fn volume<'a>(
        at: &'a str,
        total: u64,
        free: u64,
        found: u64,
        options: &'a [String],
    ) -> VolumeUse<'a> {
        VolumeUse {
            mount_point: Path::new(at),
            fs_type: "apfs",
            total: Some(total),
            free: Some(free),
            entered: true,
            options,
            found,
        }
    }

    #[test]
    fn p2_qry_03_the_volumes_of_one_container_are_found_together() {
        // / and its Data volume share a container: 100 used, 90 found
        // across the two.
        let both = [
            volume("/", 200, 100, 20, &[]),
            volume("/System/Volumes/Data", 200, 100, 70, &[]),
        ];
        assert_eq!(unaccounted(Path::new("/"), &both), Some(10));
        // Another filesystem mounted below counts on its own.
        let other = [
            volume("/", 200, 100, 90, &[]),
            volume("/Volumes/USB", 64, 32, 30, &[]),
        ];
        assert_eq!(unaccounted(Path::new("/"), &other), Some(10));
        // Not a mount point; a btrfs subvolume.
        assert_eq!(unaccounted(Path::new("/home"), &both), None);
        let subvol = ["rw".to_owned(), "subvol=/@home".to_owned()];
        let home = [volume("/home", 200, 100, 40, &subvol)];
        assert_eq!(unaccounted(Path::new("/home"), &home), None);
    }
}

#[cfg(test)]
mod subvolume_tests {
    use super::{SubvolumeBytes, VolumeRoot, referenced, subvolume_shares};

    fn root(id: u32, last: u32, dev: u64, n: u64) -> VolumeRoot {
        VolumeRoot {
            id,
            last,
            dev,
            allocated: n,
            apparent: n,
            du_compatible: n,
        }
    }

    fn own(id: u32, n: u64) -> SubvolumeBytes {
        SubvolumeBytes {
            id,
            allocated: n,
            apparent: n,
            du_compatible: n,
        }
    }

    #[test]
    fn p5_walk_02_a_subvolume_holds_what_starts_on_it_less_what_starts_below() {
        // The root's volume (0..=20); a btrfs subvolume at 2 holding a
        // nested one at 4 and a tmpfs mounted at 8; a ZFS dataset mounted at
        // 12, a subvolume and a volume both; a mount point at 18 the scan did
        // not enter.
        let volumes = [
            root(0, 20, 1, 1_000),
            root(8, 9, 2, 50),
            root(12, 17, 3, 300),
            root(18, 18, 4, 4),
        ];
        let subvolumes = [
            root(2, 10, 0, 500),
            root(4, 6, 0, 100),
            root(12, 17, 0, 300),
            root(14, 15, 0, 20),
        ];
        assert_eq!(
            subvolume_shares(&subvolumes, &volumes, &[4]),
            [own(2, 350), own(4, 100), own(12, 280), own(14, 20)]
        );
        // The path itself a subvolume: what lies below it starts nothing
        // above it.
        let at_root = [root(0, 20, 0, 1_000), root(4, 6, 0, 100)];
        assert_eq!(
            subvolume_shares(&at_root, &[root(0, 20, 1, 1_000)], &[]),
            [own(0, 900), own(4, 100)]
        );
        assert_eq!(subvolume_shares(&[], &volumes, &[]), []);
    }

    #[test]
    fn p5_walk_02_a_zfs_dataset_references_its_size_less_its_free_space() {
        assert_eq!(referenced("zfs", 1_000, 400), Some(600));
        assert_eq!(referenced("zfs", 0, 0), None, "sizes unknown");
        assert_eq!(referenced("btrfs", 1_000, 400), None, "the filesystem's");
    }
}
