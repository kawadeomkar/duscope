//! Mounts: `/proc/self/mountinfo`, the platform matrix's class of each
//! filesystem type, and the volume holding a path.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use super::statx;
use crate::platform::posix;
use crate::platform::posix::unlink::CheckedDir;
use crate::{DirHandle, Entry, Kind, NameRef, Normalization, VolumeClass, VolumeInfo};

/// One line of `/proc/self/mountinfo`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Mount {
    /// The mount id (the reusable one, `STATX_MNT_ID`).
    pub(super) id: u64,
    /// The mounted filesystem's device (`major:minor`), which every mount
    /// of one filesystem shares: its bind mounts, and a btrfs filesystem's
    /// subvolumes mounted on their own.
    pub(super) fs_dev: u64,
    /// Where it is mounted.
    pub(super) path: PathBuf,
    pub(super) fs_type: String,
    /// What is mounted: a device, or for ZFS the dataset (`pool/name`).
    pub(super) source: String,
    /// The per-mount options, then the superblock's.
    pub(super) options: Vec<String>,
}

/// The mounts in a `/proc/self/mountinfo` text: `id parent major:minor root
/// mount-point options [optional fields...] - fs-type source super-options`,
/// with spaces, tabs, newlines and backslashes in paths escaped as `\ooo`.
pub(super) fn parse_mountinfo(text: &[u8]) -> Vec<Mount> {
    text.split(|&b| b == b'\n')
        .filter_map(|line| {
            let fields: Vec<&[u8]> = line.split(|&b| b == b' ').collect();
            let id = std::str::from_utf8(fields.first()?).ok()?.parse().ok()?;
            let (major, minor) = std::str::from_utf8(fields.get(2)?).ok()?.split_once(':')?;
            let fs_dev = libc::makedev(major.parse().ok()?, minor.parse().ok()?);
            let path = PathBuf::from(OsStr::from_bytes(&unescape(fields.get(4)?)));
            let dash = fields.iter().position(|&f| f == b"-")?;
            let fs_type = String::from_utf8_lossy(fields.get(dash + 1)?).into_owned();
            let source = fields
                .get(dash + 2)
                .map(|s| String::from_utf8_lossy(&unescape(s)).into_owned())
                .unwrap_or_default();
            let mut options = split_options(fields.get(5)?);
            options.extend(
                fields
                    .get(dash + 3)
                    .map(|o| split_options(o))
                    .unwrap_or_default(),
            );
            Some(Mount {
                id,
                fs_dev,
                path,
                fs_type,
                source,
                options,
            })
        })
        .collect()
}

fn split_options(field: &[u8]) -> Vec<String> {
    field
        .split(|&b| b == b',')
        .filter(|o| !o.is_empty())
        .map(|o| String::from_utf8_lossy(o).into_owned())
        .collect()
}

/// A mountinfo path with its `\ooo` escapes decoded.
fn unescape(field: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(field.len());
    let mut i = 0;
    while i < field.len() {
        let octal = field.get(i + 1..i + 4).and_then(|digits| {
            let digits = std::str::from_utf8(digits).ok()?;
            u8::from_str_radix(digits, 8).ok()
        });
        match (field[i], octal) {
            (b'\\', Some(byte)) => {
                out.push(byte);
                i += 4;
            }
            (byte, _) => {
                out.push(byte);
                i += 1;
            }
        }
    }
    out
}

/// The platform matrix's class of a Linux filesystem type. The kernel
/// list is ncdu's `--exclude-kernfs` set, by type rather than path.
pub(super) fn class(fs_type: &str) -> VolumeClass {
    match fs_type {
        "binfmt_misc" | "bpf" | "cgroup" | "cgroup2" | "debugfs" | "devpts" | "proc" | "pstore"
        | "securityfs" | "selinuxfs" | "sysfs" | "tracefs" => VolumeClass::Kernel,
        // FUSE on a block device (ntfs-3g, exfat-fuse) is a local disk.
        "fuseblk" => VolumeClass::Foreign,
        "cifs" | "smb3" | "smbfs" | "9p" | "virtiofs" | "afp" | "afpfs" | "autofs" | "ceph"
        | "lustre" | "gpfs" | "afs" | "openafs" | "beegfs" | "pvfs2" => VolumeClass::Network,
        t if t.starts_with("nfs") || t.starts_with("fuse") => VolumeClass::Network,
        "tmpfs" | "ramfs" | "devtmpfs" => VolumeClass::Memory,
        "overlay" => VolumeClass::Overlay,
        "exfat" | "vfat" | "msdos" | "fat" | "ntfs" | "ntfs3" | "hfs" | "hfsplus" => {
            VolumeClass::Foreign
        }
        "squashfs" | "iso9660" | "erofs" | "cramfs" | "udf" => VolumeClass::ReadOnlyImage,
        _ => VolumeClass::Local,
    }
}

/// Whether names on a filesystem differ by case alone, from its type and
/// its mount options: not on exFAT, FAT and HFS, not on NTFS mounted with
/// `nocase`, not on a ZFS dataset whose options say `caseinsensitive`
/// (`casemixed` compares bytes on Linux); unknown for HFS+, whose HFSX
/// variant is case-sensitive, which only its volume header says; yes
/// elsewhere.
pub(super) fn case_sensitive(fs_type: &str, options: &[String]) -> Option<bool> {
    let has = |option: &str| options.iter().any(|o| o == option);
    match fs_type {
        "exfat" | "vfat" | "msdos" | "fat" | "hfs" => Some(false),
        "ntfs" | "ntfs3" => Some(!has("nocase")),
        "zfs" => Some(!has("caseinsensitive")),
        "hfsplus" => None,
        _ => Some(true),
    }
}

/// Whether `inner`, mounted inside `outer`, is another subvolume of the
/// filesystem `outer` is on: for btrfs, the same filesystem (its device)
/// with another subvolume (`subvolid`, or `subvol` where the kernel lists
/// no id); for ZFS, another dataset or snapshot of the same pool. A bind
/// mount of the same subvolume or dataset is not one.
pub(super) fn subvolume_of(outer: &Mount, inner: &Mount) -> bool {
    if outer.fs_type != inner.fs_type {
        return false;
    }
    match inner.fs_type.as_str() {
        "btrfs" => {
            fn subvolume(m: &Mount) -> Option<&str> {
                option(m, "subvolid").or_else(|| option(m, "subvol"))
            }
            outer.fs_dev == inner.fs_dev
                && matches!((subvolume(outer), subvolume(inner)), (Some(a), Some(b)) if a != b)
        }
        "zfs" => {
            let pool = |m: &Mount| m.source.split(['/', '@']).next().map(str::to_owned);
            pool(outer) == pool(inner) && outer.source != inner.source
        }
        _ => false,
    }
}

/// The value of the mount option `key=value` of `mount`.
fn option<'m>(mount: &'m Mount, key: &str) -> Option<&'m str> {
    mount
        .options
        .iter()
        .find_map(|o| o.strip_prefix(key)?.strip_prefix('='))
}

/// The mount table one provider reads from `/proc/self/mountinfo`, for one
/// walk: read when first needed, and read again when a lookup misses or
/// finds a mount at another place than asked, since a freed mount id is
/// handed to the next mount at once.
#[derive(Debug, Default)]
pub(in crate::platform) struct Mounts {
    table: Mutex<Vec<Mount>>,
    /// The walk root as given and its real path, for matching paths
    /// against mount points.
    root: Mutex<Option<(PathBuf, PathBuf)>>,
}

impl Mounts {
    /// The mount holding `path`: by `mnt_id` (the reusable id, as
    /// `STATX_MNT_ID` gives it) when there is one, else by the longest
    /// mount point the real path `real` starts with. With `at_point`, the
    /// mount must be mounted at `real`.
    fn lookup(&self, real: &Path, mnt_id: Option<u64>, at_point: bool) -> io::Result<Mount> {
        let find = |table: &[Mount]| -> Option<Mount> {
            let found = match mnt_id {
                Some(id) => table.iter().find(|m| m.id == id),
                None => table
                    .iter()
                    .filter(|m| real.starts_with(&m.path))
                    .max_by_key(|m| m.path.as_os_str().len()),
            };
            found.filter(|m| !at_point || m.path == real).cloned()
        };
        let mut table = self.table.lock().unwrap_or_else(PoisonError::into_inner);
        if !table.is_empty()
            && let Some(found) = find(&table)
        {
            return Ok(found);
        }
        *table = parse_mountinfo(&fs::read("/proc/self/mountinfo")?);
        find(&table)
            .or_else(|| {
                (at_point && mnt_id.is_some())
                    .then(|| table.iter().find(|m| Some(m.id) == mnt_id).cloned())
                    .flatten()
            })
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no mount holds the path"))
    }

    /// The real path of `name` in `dir`: the walk root's real path, read
    /// once, joined with the names below it. A directory reached through a
    /// link followed under `--follow-symlinks` has its resolved path already.
    fn real(&self, dir: &DirHandle, name: &[u8]) -> io::Result<PathBuf> {
        let path = dir.path();
        let given = dir
            .ancestors()
            .last()
            .map_or_else(|| path.clone(), DirHandle::path);
        let Ok(below) = path.strip_prefix(&given) else {
            return Ok(path.join(OsStr::from_bytes(name)));
        };
        let mut root = self.root.lock().unwrap_or_else(PoisonError::into_inner);
        let real_root = match &*root {
            Some((cached, real)) if *cached == given => real.clone(),
            _ => {
                let real = fs::canonicalize(&given)?;
                *root = Some((given, real.clone()));
                real
            }
        };
        Ok(real_root.join(below).join(OsStr::from_bytes(name)))
    }

    /// Whether the mount root `name` in `dir` is another subvolume of the
    /// filesystem `dir` is on ([`subvolume_of`]): a btrfs subvolume or a
    /// ZFS dataset mounted on its own. Both mounts are found by path in the
    /// table, which the mount root's own `statx` cannot name (its unique
    /// mount id is not the one mountinfo lists); false where either is not
    /// found.
    pub(in crate::platform) fn subvolume_mount(&self, dir: &DirHandle, name: &[u8]) -> bool {
        let Ok(inner) = self.real(dir, name) else {
            return false;
        };
        let Some(outer) = inner.parent() else {
            return false;
        };
        match (
            self.lookup(outer, None, false),
            self.lookup(&inner, None, true),
        ) {
            (Ok(outer), Ok(inner)) => outer.id != inner.id && subvolume_of(&outer, &inner),
            _ => false,
        }
    }

    /// The mount id of the mount holding `path`, by path, for kernels whose
    /// `statx` reports none (before 5.8) and for the fallback provider; 0
    /// when it cannot be found.
    pub(in crate::platform) fn mnt_id_of(&self, path: &Path) -> u64 {
        fs::canonicalize(path)
            .and_then(|real| self.lookup(&real, None, false))
            .map_or(0, |m| m.id)
    }

    /// Gives an entry whose mount id `statx` did not report (before Linux
    /// 5.8, or read by `stat`) its mount's id. A directory is looked up by
    /// path, so a bind mount on the same device is still a mount of its
    /// own; any other entry on its directory's device takes its
    /// directory's.
    pub(in crate::platform) fn fill_mnt_id(&self, entry: &mut Entry, dir: &DirHandle) {
        if entry.mnt_id != 0 {
            return;
        }
        entry.mnt_id = if entry.kind != Kind::Dir && entry.dev == dir.dev() {
            dir.mnt_id()
        } else {
            self.real(dir, entry.name.as_bytes())
                .and_then(|real| self.lookup(&real, None, false))
                .map_or(dir.mnt_id(), |m| m.id)
        };
    }

    /// The volume mounted at `path` from its mountinfo line, the mounted
    /// filesystem asked nothing (`statx` with `AT_NO_AUTOMOUNT` and
    /// `AT_STATX_DONT_SYNC` reads only what the kernel holds); the sizes of
    /// a quiet class come from `statvfs`, else 0. `path` is the walk's
    /// path: under a root given with `..` or relatively it is not the
    /// mount point mountinfo lists, so the lookup by path misses and the
    /// table is read again, once per mount (the walk keeps each mount's
    /// answer). It is not resolved here, since resolving it would ask the
    /// mounted filesystem; the binary resolves the root before the walk.
    pub(in crate::platform) fn mount_info(&self, path: &Path) -> io::Result<VolumeInfo> {
        let c_path = posix::c_path(path)?;
        let stx = statx::statx(None, &c_path, libc::STATX_MNT_ID | libc::STATX_INO)?;
        let mnt_id = (stx.stx_mask & libc::STATX_MNT_ID != 0).then_some(stx.stx_mnt_id);
        let mount = self.lookup(path, mnt_id, true)?;
        // Sizes as macOS's mount table has them: read where that cannot
        // wait on a server or describe a kernel table, else 0.
        let quiet = !matches!(
            class(&mount.fs_type.to_lowercase()),
            VolumeClass::Network | VolumeClass::Kernel
        );
        let (total, free) = if quiet {
            sizes(&c_path).unwrap_or((0, 0))
        } else {
            (0, 0)
        };
        Ok(describe(
            libc::makedev(stx.stx_dev_major, stx.stx_dev_minor),
            mount,
            total,
            free,
        ))
    }

    /// The volume holding `path`; sizes from `statvfs`, free bytes being
    /// `f_bavail × f_frsize`. The walk enters it, so an automount there is
    /// triggered and the mounted volume described. `mnt_id` is the reusable
    /// id mountinfo lists; the walk puts the entries' id in its place.
    pub(in crate::platform) fn volume(&self, path: &Path) -> io::Result<VolumeInfo> {
        let c_path = posix::c_path(path)?;
        let stx = statx::statx_through(&c_path, statx::BASIC & !libc::STATX_MNT_ID_UNIQUE)?;
        let entry: Entry = statx::entry(NameRef::new(b""), &stx, crate::Attrs::Basic);
        let mnt_id = (stx.stx_mask & libc::STATX_MNT_ID != 0).then_some(stx.stx_mnt_id);
        let real = fs::canonicalize(path)?;
        let mount = self.lookup(&real, mnt_id, false)?;
        let (total, free) = sizes(&c_path)?;
        Ok(describe(entry.dev, mount, total, free))
    }
}

/// The mount point nearest `dir` at it or under it in this process's
/// mount table, read now: how `clean --permanent` tells a bind mount of
/// the same device under a directory where neither `statx` nor a file
/// handle can. `dir` is a real path, as the table's are.
///
/// # Errors
///
/// When the table cannot be read, or lists no mount holding `dir` (not a
/// table to trust).
pub(in crate::platform) fn mount_point_under(dir: &Path) -> io::Result<Option<PathBuf>> {
    nearest_under(&parse_mountinfo(&fs::read("/proc/self/mountinfo")?), dir)
}

/// Whether the entry at `path`, absolute and resolved, read as `meta`
/// without following a link, is a mount point to `clean`. A directory on
/// another device than its parent is one, as before: a mount, or a
/// btrfs subvolume, which `--permanent` never enters either. Otherwise the
/// entry is one when it is the root of a mount, or an automount point,
/// by what [`CheckedDir::mount_root`] reads through its directory's
/// descriptor (`statx`'s mount-root attribute, the two mount ids, the
/// mount table); where none of them answers, when its device is not its
/// directory's, as before. A non-directory's own device says nothing of a
/// mount: overlayfs reports a directory on the overlay's device and a file
/// on the device of the layer that holds it, unless `xino` maps them all
/// to the overlay's (the overlayfs documentation's "Inode properties").
/// `/`, which has no directory, is the caller's to judge.
pub(in crate::platform) fn mount_point(path: &Path, meta: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return false;
    };
    let other_device = crate::platform::other_device(parent, meta.dev());
    if meta.is_dir() && other_device {
        return true;
    }
    let root = CheckedDir::open(parent)
        .ok()
        .and_then(|dir| dir.mount_root(name));
    is_mount_point(meta.is_dir(), other_device, root)
}

/// [`mount_point`]'s rule, for an entry that is a directory (`dir`) or not,
/// on another device than its directory or not, and the root of a mount by
/// what the system tells (`root`, `None` where nothing answers).
pub(super) fn is_mount_point(dir: bool, other_device: bool, root: Option<bool>) -> bool {
    (dir && other_device) || root.unwrap_or(other_device)
}

/// The mount point in `table` nearest `dir` at it or under it, comparing
/// whole components.
fn nearest_under(table: &[Mount], dir: &Path) -> io::Result<Option<PathBuf>> {
    if !table.iter().any(|m| dir.starts_with(&m.path)) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the mount table lists no mount holding the directory",
        ));
    }
    Ok(table
        .iter()
        .filter(|m| m.path.starts_with(dir))
        .min_by_key(|m| m.path.components().count())
        .map(|m| m.path.clone()))
}

/// The size and the bytes free of the filesystem holding `path`, from
/// `statvfs`: free bytes are `f_bavail × f_frsize`.
fn sizes(path: &std::ffi::CStr) -> io::Result<(u64, u64)> {
    let vfs = posix::statvfs(path)?;
    // The fields are 32-bit on 32-bit targets.
    #[allow(clippy::useless_conversion)]
    let (frsize, blocks, avail) = (
        u64::from(vfs.f_frsize),
        u64::from(vfs.f_blocks),
        u64::from(vfs.f_bavail),
    );
    Ok((blocks.saturating_mul(frsize), avail.saturating_mul(frsize)))
}

/// A volume record from a mount and its sizes.
fn describe(dev: u64, mount: Mount, total_bytes: u64, free_bytes: u64) -> VolumeInfo {
    let fs_type = mount.fs_type.to_lowercase();
    VolumeInfo {
        dev,
        mnt_id: mount.id,
        mount_point: mount.path,
        class: class(&fs_type),
        read_only: mount.options.iter().any(|o| o == "ro"),
        case_sensitive: case_sensitive(&fs_type, &mount.options),
        normalization: if fs_type == "hfsplus" || fs_type == "hfs" {
            Normalization::Insensitive
        } else {
            Normalization::None
        },
        options: mount.options,
        fs_type,
        total_bytes,
        free_bytes,
        reason: None,
        fsevents: None,
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        Mount, case_sensitive, is_mount_point, nearest_under, parse_mountinfo, subvolume_of,
    };

    /// The mounts of a pool `tank` (its root dataset, a child, a snapshot
    /// mounted under `.zfs`, and a bind mount of the child), a second pool,
    /// and a btrfs filesystem with a subvolume mounted on its own, as
    /// ZFS and btrfs list them.
    const TABLE: &[u8] = b"\
29 1 259:2 / / rw,relatime shared:1 - ext4 /dev/nvme0n1p2 rw\n\
40 29 0:50 / /tank rw,noatime shared:20 - zfs tank rw,xattr,noacl,casesensitive\n\
41 40 0:51 / /tank/home rw,noatime shared:21 - zfs tank/home rw,xattr,noacl,caseinsensitive\n\
42 41 0:52 / /tank/home/.zfs/snapshot/daily rw,relatime shared:22 - zfs tank/home@daily ro,xattr,noacl,casesensitive\n\
43 29 0:51 / /srv/home rw,noatime shared:21 - zfs tank/home rw,xattr,noacl,caseinsensitive\n\
44 40 0:53 / /tank/other rw,noatime shared:23 - zfs other rw,xattr,noacl,casemixed\n\
50 29 0:42 / /data rw,relatime shared:30 - btrfs /dev/loop0 rw,space_cache=v2,subvolid=5,subvol=/\n\
51 50 0:42 /sub /data/mnt rw,relatime shared:31 - btrfs /dev/loop0 rw,space_cache=v2,subvolid=256,subvol=/sub\n\
52 50 0:42 /dir /data/bound rw,relatime shared:30 - btrfs /dev/loop0 rw,space_cache=v2,subvolid=5,subvol=/\n\
53 50 0:61 / /data/other rw,relatime shared:32 - btrfs /dev/loop1 rw,space_cache=v2,subvolid=256,subvol=/sub\n";

    fn mount(table: &[Mount], path: &str) -> Mount {
        table
            .iter()
            .find(|m| m.path == Path::new(path))
            .unwrap()
            .clone()
    }

    #[test]
    fn p5_walk_02_mountinfo_names_the_filesystem_and_its_source() {
        let table = parse_mountinfo(TABLE);
        assert_eq!(table.len(), 10);
        let home = mount(&table, "/tank/home");
        assert_eq!(
            (home.id, home.fs_dev, home.source.as_str()),
            (41, libc::makedev(0, 51), "tank/home")
        );
        let sub = mount(&table, "/data/mnt");
        assert_eq!(
            (sub.fs_dev, sub.source.as_str()),
            (libc::makedev(0, 42), "/dev/loop0")
        );
        assert!(sub.options.iter().any(|o| o == "subvolid=256"));
    }

    #[test]
    fn p5_walk_02_zfs_datasets_of_one_pool_are_subvolumes() {
        let table = parse_mountinfo(TABLE);
        let at = |path| mount(&table, path);
        // A child dataset and a snapshot of the same pool.
        assert!(subvolume_of(&at("/tank"), &at("/tank/home")));
        assert!(subvolume_of(
            &at("/tank/home"),
            &at("/tank/home/.zfs/snapshot/daily")
        ));
        // Another pool's dataset, and a pool entered from ext4, are mounts.
        assert!(!subvolume_of(&at("/tank"), &at("/tank/other")));
        assert!(!subvolume_of(&at("/"), &at("/tank")));
        // The same dataset mounted twice is not another dataset.
        assert!(!subvolume_of(&at("/tank/home"), &at("/srv/home")));
    }

    #[test]
    fn p5_walk_02_a_btrfs_subvolume_mounted_on_its_own_is_a_subvolume() {
        let table = parse_mountinfo(TABLE);
        let at = |path| mount(&table, path);
        assert!(subvolume_of(&at("/data"), &at("/data/mnt")));
        // A bind mount inside the same subvolume, and another filesystem's
        // subvolume, are mounts.
        assert!(!subvolume_of(&at("/data"), &at("/data/bound")));
        assert!(!subvolume_of(&at("/data"), &at("/data/other")));
        assert!(!subvolume_of(&at("/"), &at("/data")));
        // Where the kernel lists no subvolume id, the subvolume's path.
        let mut outer = at("/data");
        let mut inner = at("/data/mnt");
        for m in [&mut outer, &mut inner] {
            m.options.retain(|o| !o.starts_with("subvolid="));
        }
        assert!(subvolume_of(&outer, &inner));
    }

    #[test]
    fn p5_core_03_the_mount_table_names_a_mount_point_at_or_under_a_directory() {
        let table = parse_mountinfo(TABLE);
        let under = |dir: &str| nearest_under(&table, Path::new(dir)).unwrap();
        // The directory itself, and the nearest of several under it.
        assert_eq!(under("/data/mnt"), Some("/data/mnt".into()));
        assert_eq!(under("/tank"), Some("/tank".into()));
        assert_eq!(under("/srv"), Some("/srv/home".into()));
        assert_eq!(
            under("/tank/home/.zfs"),
            Some("/tank/home/.zfs/snapshot/daily".into())
        );
        // Whole components: /tank/ho is not above /tank/home.
        assert_eq!(under("/tank/ho"), None);
        assert_eq!(under("/data/bound/dir"), None);
        assert_eq!(under("/home/u"), None);
        // A table without a mount holding the directory is not trusted.
        assert!(nearest_under(&[], Path::new("/home/u")).is_err());
        let data_only: Vec<Mount> = table
            .into_iter()
            .filter(|m| m.path == Path::new("/data"))
            .collect();
        assert!(nearest_under(&data_only, Path::new("/home/u")).is_err());
    }

    #[test]
    fn p5_walk_02_case_comes_from_the_type_and_the_mount_options() {
        let table = parse_mountinfo(TABLE);
        let case = |path| {
            let m = mount(&table, path);
            case_sensitive(&m.fs_type, &m.options)
        };
        assert_eq!(case("/tank"), Some(true));
        assert_eq!(case("/tank/home"), Some(false), "caseinsensitive");
        assert_eq!(case("/tank/other"), Some(true), "casemixed");
        assert_eq!(case("/data"), Some(true));
        for foreign in ["exfat", "vfat", "msdos", "hfs"] {
            assert_eq!(case_sensitive(foreign, &[]), Some(false), "{foreign}");
        }
        assert_eq!(case_sensitive("ntfs3", &[]), Some(true));
        assert_eq!(case_sensitive("ntfs3", &["nocase".to_owned()]), Some(false));
        assert_eq!(case_sensitive("hfsplus", &[]), None, "HFSX is sensitive");
    }

    /// P5-CORE-10: `clean`'s mount points, by what an entry is, whether its
    /// device is its directory's, and whether the system says it is the
    /// root of a mount (`None`: nothing answers).
    #[test]
    fn p5_core_10_a_file_on_another_device_is_a_mount_point_only_as_a_mount_root() {
        let cases = [
            // A file of an overlay without xino: its layer's device, no mount.
            ((false, true, Some(false)), false),
            // A file bound over another, from another filesystem or the same.
            ((false, true, Some(true)), true),
            ((false, false, Some(true)), true),
            ((false, false, Some(false)), false),
            // A file where nothing answers: its device decides, as before.
            ((false, true, None), true),
            ((false, false, None), false),
            // A directory on another device: a mount, or a btrfs subvolume.
            ((true, true, Some(false)), true),
            ((true, true, Some(true)), true),
            ((true, true, None), true),
            // A directory bound over another of the same device.
            ((true, false, Some(true)), true),
            ((true, false, Some(false)), false),
            ((true, false, None), false),
        ];
        for ((dir, other_device, root), expected) in cases {
            assert_eq!(
                is_mount_point(dir, other_device, root),
                expected,
                "directory {dir}, another device {other_device}, mount root {root:?}"
            );
        }
    }
}
