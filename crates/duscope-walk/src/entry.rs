//! The record a provider fills for every directory entry, and the events a
//! walk delivers to its sink.
//!
//! `Entry` is the transport record. The arena stores a 48-byte record of its
//! own (P1-CORE-01) and keeps `dev`/`ino` and clone ids in side maps.

use std::ffi::OsStr;
use std::fmt;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

use bitflags::bitflags;

use crate::ScanError;

/// A directory's id within one walk. Ids are dense: the root is
/// [`DirId::ROOT`], and the walk numbers every other directory as it
/// discovers it, so a side table indexed by [`DirId::index`] has no holes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DirId(u32);

impl DirId {
    /// The walk root's id.
    pub const ROOT: DirId = DirId(0);

    /// The id with this number.
    #[must_use]
    pub const fn new(id: u32) -> DirId {
        DirId(id)
    }

    /// The id as a number.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }

    /// The id as an index into a side table.
    #[must_use]
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// An entry's name: the bytes the directory listing returned. Names are bytes
/// on both platforms and are never decoded here; the envelope decides how to
/// show a name that is not valid UTF-8.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NameRef(Box<[u8]>);

impl NameRef {
    /// A name holding a copy of these bytes.
    #[must_use]
    pub fn new(bytes: &[u8]) -> NameRef {
        NameRef(bytes.into())
    }

    /// The name's bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// The name as an `OsStr`, for joining onto a path.
    #[must_use]
    pub fn as_os_str(&self) -> &OsStr {
        OsStr::from_bytes(&self.0)
    }
}

impl fmt::Debug for NameRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "\"{}\"", self.0.escape_ascii())
    }
}

impl From<&[u8]> for NameRef {
    fn from(bytes: &[u8]) -> NameRef {
        NameRef::new(bytes)
    }
}

impl From<&OsStr> for NameRef {
    fn from(name: &OsStr) -> NameRef {
        NameRef::new(name.as_bytes())
    }
}

impl From<&str> for NameRef {
    fn from(name: &str) -> NameRef {
        NameRef::new(name.as_bytes())
    }
}

/// What an entry is. A symlink is its own entry and is never followed by
/// default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// A directory.
    Dir,
    /// A regular file.
    File,
    /// A symbolic link.
    Symlink,
    /// A Unix-domain socket.
    Socket,
    /// A named pipe.
    Fifo,
    /// A character device.
    Char,
    /// A block device.
    Block,
    /// A type the platform did not report.
    Unknown,
}

/// Whether an entry's blocks may be shared with another entry, for the
/// hard-link and clone charging of the roll-up. A hard-linked file that is
/// also a clone reports `Clone`: the clone group decides its sharing, and
/// its `nlink` above 1 still tells the roll-up it is linked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Share {
    /// Nothing is shared.
    None,
    /// A non-directory with more than one link.
    Hardlink,
    /// A member of the clone group with this id (APFS, under `--clones`).
    Clone(u64),
    /// The filesystem says the blocks may be shared but names no group.
    MayShare,
}

/// Whether an entry sits on a boundary between filesystems or trees.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Boundary {
    /// An ordinary entry.
    None,
    /// The root of another mount.
    Mount,
    /// A macOS firmlink into the Data volume.
    Firmlink,
    /// A btrfs subvolume.
    Subvolume,
    /// A bind mount.
    Bind,
}

bitflags! {
    /// Facts about an entry's storage. A platform leaves the bits it cannot
    /// know at 0. The bit values are stored in the index's `flags` columns, so
    /// they never change.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct Flags: u32 {
        /// The file has holes.
        const SPARSE = 1 << 0;
        /// The file is stored compressed.
        const COMPRESSED = 1 << 1;
        /// A cloud placeholder whose data is not on the disk.
        const DATALESS = 1 << 2;
        /// The system may delete the file to free space.
        const PURGEABLE = 1 << 3;
        /// The file's blocks may be shared with a clone.
        const MAY_SHARE = 1 << 4;
        /// Every block of the file is shared with a clone.
        const SHARES_ALL = 1 << 5;
        /// The entry is the root of a mount.
        const MOUNT_ROOT = 1 << 6;
        /// The entry is a btrfs subvolume.
        const SUBVOLUME = 1 << 7;
    }
}

/// The attributes `--attrs full` adds, read by the same syscalls as the basic
/// set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FullAttrs {
    /// Last access time, Unix nanoseconds.
    pub atime_ns: i64,
    /// Creation time, Unix nanoseconds, where the filesystem records one.
    pub btime_ns: Option<i64>,
    /// File type and permission bits, as in `st_mode`.
    pub mode: u32,
    /// Owner user id.
    pub uid: u32,
    /// Owner group id.
    pub gid: u32,
}

/// One directory entry, as a provider reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The entry's name in its parent directory.
    pub name: NameRef,
    /// The directory the entry is in. The walk root is its own parent.
    pub parent: DirId,
    /// For a directory, the id its children name as `parent`; `None` for
    /// every other kind. The walk assigns it.
    pub dir: Option<DirId>,
    /// What the entry is.
    pub kind: Kind,
    /// Apparent size in bytes: a file's length, a symlink's target length.
    pub apparent: u64,
    /// Bytes allocated on disk.
    pub allocated: u64,
    /// Last modification time, Unix nanoseconds.
    pub mtime_ns: i64,
    /// Last inode change time, Unix nanoseconds.
    pub ctime_ns: i64,
    /// Hard-link count.
    pub nlink: u32,
    /// Device id. On macOS it is the volume's own device
    /// (`FSOPT_RETURN_REALDEV`), which inside a volume group differs from
    /// the group's shared `st_dev`: a later `(dev, ino)` check against
    /// `lstat` must read the device the same way.
    pub dev: u64,
    /// Inode number.
    pub ino: u64,
    /// Mount id, for mount-boundary decisions.
    pub mnt_id: u64,
    /// Whether the entry's blocks may be shared.
    pub share: Share,
    /// Whether the entry is on a filesystem boundary.
    pub boundary: Boundary,
    /// Storage facts.
    pub flags: Flags,
    /// The `--attrs full` attributes; `None` in a basic scan.
    pub full: Option<FullAttrs>,
}

/// A volume the walk met: the root's, and one per mount it crossed or
/// stopped at (a `scan_volumes` row).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeInfo {
    /// Device id, as in [`Entry::dev`].
    pub dev: u64,
    /// Mount id, as in [`Entry::mnt_id`]: the mount's unique id on Linux
    /// 6.8 and later, its reusable id before, the device id on macOS.
    pub mnt_id: u64,
    /// Where the volume is mounted.
    pub mount_point: PathBuf,
    /// The filesystem type as the kernel names it, lower-cased (`apfs`,
    /// `ext4`, …).
    pub fs_type: String,
    /// The platform matrix's class of the filesystem type, which decides
    /// whether the walk enters it.
    pub class: VolumeClass,
    /// Mounted read-only.
    pub read_only: bool,
    /// Whether names differ by case alone; `None` where it cannot be told.
    /// For a volume the walk did not enter it comes only from the type and
    /// the mount options (Linux), or is `None` (macOS asks the volume).
    pub case_sensitive: Option<bool>,
    /// Whether names compare normalisation-insensitively (APFS, HFS+).
    pub normalization: Normalization,
    /// The mount options (`noatime`, `relatime`, …), for the
    /// `atime_unreliable` warning.
    pub options: Vec<String>,
    /// The volume's size in bytes. For a volume the walk did not enter,
    /// what the platform has without asking a server or a kernel table:
    /// the kernel's cached sizes on macOS, `statvfs` of a local mount on
    /// Linux, and 0 for network and kernel mounts on both.
    pub total_bytes: u64,
    /// Bytes free for an unprivileged user, read as `total_bytes` is.
    pub free_bytes: u64,
    /// Why the walk did not enter the volume; `None` when it did.
    pub reason: Option<Skip>,
}

impl VolumeInfo {
    /// Whether the walk entered the volume.
    #[must_use]
    pub fn entered(&self) -> bool {
        self.reason.is_none()
    }
}

/// The platform matrix's classes of filesystem types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VolumeClass {
    /// ext4, XFS, btrfs, ZFS, F2FS, bcachefs, APFS: entered.
    Local,
    /// overlayfs: entered; inode numbers are not stable across layers, so
    /// hard links are not deduplicated.
    Overlay,
    /// tmpfs, ramfs, devtmpfs: entered and tagged memory-backed.
    Memory,
    /// NFS, SMB, FUSE, 9p, AFP, autofs: listed, entered only under
    /// `--include-network`.
    Network,
    /// proc, sysfs, cgroup and the other kernel pseudo-filesystems, and
    /// macOS's devfs: never entered.
    Kernel,
    /// exFAT, FAT, NTFS, HFS+: entered, with their per-type limits.
    Foreign,
    /// squashfs, ISO 9660, EROFS: entered, read-only.
    ReadOnlyImage,
}

impl VolumeClass {
    /// The class as the index and the envelope spell it.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            VolumeClass::Local => "local",
            VolumeClass::Overlay => "overlay",
            VolumeClass::Memory => "memory",
            VolumeClass::Network => "network",
            VolumeClass::Kernel => "kernel",
            VolumeClass::Foreign => "foreign",
            VolumeClass::ReadOnlyImage => "read_only_image",
        }
    }
}

/// How a volume compares names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Normalization {
    /// Bytes as given.
    None,
    /// NFC and NFD forms of a name are the same name (APFS, HFS+).
    Insensitive,
}

impl Normalization {
    /// The value as the index spells it.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Normalization::None => "none",
            Normalization::Insensitive => "insensitive",
        }
    }
}

/// What a walk delivers to its sink, on the calling thread.
///
/// The first event is the root's entry. After that a directory's entries
/// arrive after the entry of the directory itself, and its `DirClosed`
/// arrives after its entries, so the sink always knows an entry's parent.
/// A directory the walk does not enter gets `Skipped`, then its `DirClosed`,
/// after its entry. One `Error` may come after its directory's `DirClosed`:
/// a subdirectory that could not be read for `ELOOP`, `ENAMETOOLONG` or
/// `EIO` is an error of its parent, known only once the subdirectory is
/// read; it arrives before the subdirectory's own `DirClosed`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanEvent {
    /// An entry.
    Entry(Entry),
    /// Every entry of `dir` has been delivered. `errno` is set when the
    /// directory could not be read; it then has no entries.
    DirClosed {
        /// The directory.
        dir: DirId,
        /// Why the directory could not be read.
        errno: Option<i32>,
    },
    /// Something disappeared between being listed and being read
    /// (`ENOENT`, `ESTALE`): the entry `name` that `dir` listed, which is not
    /// delivered; or, with no name, the directory `dir` itself, whose
    /// `DirClosed` follows with no entries.
    Vanished {
        /// The directory.
        dir: DirId,
        /// The entry of `dir` that vanished; `None` when `dir` itself did.
        name: Option<NameRef>,
    },
    /// A volume the walk met.
    Volume(VolumeInfo),
    /// An entry that was listed but could not be read.
    Error(ScanError),
    /// `dir` was listed but not entered; its `DirClosed` follows.
    Skipped {
        /// The directory.
        dir: DirId,
        /// Why it was not entered.
        reason: Skip,
    },
}

/// Why the walk listed a directory without entering it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Skip {
    /// macOS: the Data volume's mount point, in a scan that reaches the
    /// volume through its firmlinks.
    Firmlinked,
    /// The mount of a kernel pseudo-filesystem.
    Kernel,
    /// The mount of a network filesystem, without `--include-network`.
    Network,
    /// A mount other than the root's, under `-x`.
    OneFileSystem,
    /// A mount of a directory already on the path (a bind mount of an
    /// ancestor), which would loop.
    BindCycle,
}

impl Skip {
    /// The reason as the index and the envelope spell it.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Skip::Firmlinked => "firmlinked",
            Skip::Kernel => "kernel",
            Skip::Network => "network",
            Skip::OneFileSystem => "one_file_system",
            Skip::BindCycle => "bind_cycle",
        }
    }
}
