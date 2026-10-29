//! The arena: the scan in memory, built on the consumer thread from the
//! walk's [`ScanEvent`]s.
//!
//! Each entry is a 48-byte [`EntryRec`] in scan order; each directory also
//! has a 48-byte [`DirRec`], indexed by its [`DirId`], whose cumulative
//! fields the roll-up fills (P1-CORE-02). Names are stored once each in
//! chunked byte storage, without deduplication. What few entries need is
//! kept beside the records: the `(dev, ino)` of hard-linked files, clone
//! ids, mount boundaries, the `--attrs full` side records. Every store grows
//! by fixed-size chunks, so a `Vec` doubling can never double the peak
//! resident size.

use std::collections::HashMap;
use std::mem::size_of;

use duscope_walk::{
    Attrs, Boundary, DirId, Entry, Flags, FullAttrs, Kind, ScanError, ScanEvent, Share, Skip,
    VolumeInfo,
};

use crate::CoreError;

/// An entry's index in scan order; the root is 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EntryIdx(u32);

impl EntryIdx {
    /// The root's index.
    pub const ROOT: EntryIdx = EntryIdx(0);

    /// The index as a number.
    pub const fn get(self) -> u32 {
        self.0
    }

    /// The index for a side table.
    pub const fn index(self) -> usize {
        self.0 as usize
    }

    /// The index with this number.
    pub(crate) const fn from_raw(raw: u32) -> EntryIdx {
        EntryIdx(raw)
    }
}

/// One entry: 48 bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct EntryRec {
    apparent: u64,
    allocated: u64,
    mtime_ns: i64,
    ctime_ns: i64,
    /// Where the name starts in the name store.
    name_at: u32,
    name_len: u16,
    kind: u8,
    /// `Flags` bits 0..=7.
    flags: u8,
    /// The directory the entry is in.
    parent: u32,
    /// A non-directory's hard-link count; a directory's own [`DirId`]
    /// (directories never enter the hard-link map, so their link count is
    /// not kept).
    nlink_or_dir: u32,
}

impl EntryRec {
    /// The directory the entry is in.
    pub(crate) fn parent(&self) -> DirId {
        DirId::new(self.parent)
    }

    /// The entry's own directory, when it is one.
    pub(crate) fn dir(&self) -> Option<DirId> {
        (self.kind == kind_code(Kind::Dir)).then(|| DirId::new(self.nlink_or_dir))
    }

    /// Apparent and allocated bytes.
    pub(crate) fn sizes(&self) -> (u64, u64) {
        (self.apparent, self.allocated)
    }

    /// Modification time.
    pub(crate) fn mtime_ns(&self) -> i64 {
        self.mtime_ns
    }
}

/// The directory records, written while the entries' records are read
/// ([`Arena::records_and_dirs`]).
pub(crate) struct DirsMut<'a>(&'a mut Chunked<DirRec>);

impl DirsMut<'_> {
    /// The directory record of `dir`.
    pub(crate) fn get(&mut self, dir: DirId) -> &mut DirRec {
        self.0.slot(dir.index())
    }
}

/// One directory's record beside its entry: 48 bytes, the cumulative fields
/// zero until the roll-up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct DirRec {
    /// Total apparent bytes of the subtree, the directory included.
    pub cum_apparent: u64,
    /// Total allocated bytes of the subtree.
    pub cum_allocated: u64,
    /// The latest mtime in the subtree.
    pub max_mtime_ns: i64,
    /// The directory's entry; [`DirRec::NO_ENTRY`] until it arrives.
    pub entry: u32,
    /// Non-directories in the subtree.
    pub n_files: u32,
    /// Directories in the subtree, the directory excluded.
    pub n_dirs: u32,
    /// The last pre-order id of the subtree.
    pub last_id: u32,
    /// The last pre-order id of the directory's own files.
    pub files_end: u32,
    /// How far below the root; the root is 0. It saturates at 65,535
    /// (`u16::MAX`): a deeper directory, beyond `PATH_MAX` on both
    /// platforms, records 65,535. The record has no spare bytes to widen it,
    /// and the roll-up and the totals do not read it.
    pub depth: u16,
    /// Roll-up flags ([`DirRec::UPPER_BOUND`]).
    pub flags: u16,
}

impl DirRec {
    /// The subtree's totals include blocks that may be shared with a clone
    /// the scan could not group: an upper bound.
    pub const UPPER_BOUND: u16 = 1;
    /// The directory could not be read.
    pub const UNREADABLE: u16 = 2;
    /// The `entry` of a directory whose id the arena met before its entry
    /// (workers number directories as they find them, so a higher id can
    /// arrive first). A complete walk leaves none; a truncated stream can.
    pub const NO_ENTRY: u32 = u32::MAX;
}

impl Default for DirRec {
    /// A placeholder: no entry yet, every count zero.
    fn default() -> DirRec {
        DirRec {
            cum_apparent: 0,
            cum_allocated: 0,
            max_mtime_ns: 0,
            entry: DirRec::NO_ENTRY,
            n_files: 0,
            n_dirs: 0,
            last_id: 0,
            files_end: 0,
            depth: 0,
            flags: 0,
        }
    }
}

/// The `--attrs full` side record of one entry: 28 bytes, packed to a
/// 4-byte alignment so the times' 8-byte alignment adds no padding. Its
/// fields are only ever read by value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C, packed(4))]
struct FullRec {
    atime_ns: i64,
    /// `i64::MIN` where the filesystem keeps no creation time.
    btime_ns: i64,
    uid: u32,
    gid: u32,
    /// [`FullRec::ABSENT`]'s `u32::MAX` when the entry came without full
    /// attributes; `st_mode` fits 16 bits on both platforms, so no real
    /// mode is that.
    mode: u32,
}

impl FullRec {
    /// The record of an entry delivered without full attributes in a full
    /// scan: read back as `None`, never as owner 0 and a 1970 access time.
    const ABSENT: FullRec = FullRec {
        atime_ns: 0,
        btime_ns: i64::MIN,
        uid: 0,
        gid: 0,
        mode: u32::MAX,
    };

    fn new(full: &FullAttrs) -> FullRec {
        FullRec {
            atime_ns: full.atime_ns,
            btime_ns: full.btime_ns.unwrap_or(i64::MIN),
            uid: full.uid,
            gid: full.gid,
            mode: full.mode,
        }
    }

    fn get(self) -> Option<FullAttrs> {
        (self.mode != FullRec::ABSENT.mode).then_some(FullAttrs {
            atime_ns: self.atime_ns,
            btime_ns: (self.btime_ns != i64::MIN).then_some(self.btime_ns),
            mode: self.mode,
            uid: self.uid,
            gid: self.gid,
        })
    }
}

/// Elements per chunk of a [`Chunked`] store: 64 Ki.
const CHUNK_SHIFT: u32 = 16;
const CHUNK: usize = 1 << CHUNK_SHIFT;

/// A vector that grows by fixed-size chunks instead of doubling.
#[derive(Debug, Clone)]
pub struct Chunked<T> {
    chunks: Vec<Vec<T>>,
    len: usize,
}

impl<T> Default for Chunked<T> {
    fn default() -> Chunked<T> {
        Chunked {
            chunks: Vec::new(),
            len: 0,
        }
    }
}

impl<T> Chunked<T> {
    /// The number of elements.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether there are none.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Appends an element.
    pub fn push(&mut self, value: T) {
        if self.len % CHUNK == 0 {
            self.chunks.push(Vec::with_capacity(CHUNK));
        }
        if let Some(chunk) = self.chunks.last_mut() {
            chunk.push(value);
        }
        self.len += 1;
    }

    /// The element at `index`.
    pub fn get(&self, index: usize) -> Option<&T> {
        self.chunks
            .get(index >> CHUNK_SHIFT)?
            .get(index & (CHUNK - 1))
    }

    /// The element at `index`, mutably.
    pub fn get_mut(&mut self, index: usize) -> Option<&mut T> {
        self.chunks
            .get_mut(index >> CHUNK_SHIFT)?
            .get_mut(index & (CHUNK - 1))
    }

    /// The elements in order.
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.chunks.iter().flatten()
    }

    /// The bytes the store holds on the heap.
    pub fn heap_bytes(&self) -> usize {
        self.chunks.len() * CHUNK * size_of::<T>() + self.chunks.capacity() * size_of::<Vec<T>>()
    }
}

impl<T: Default + Clone> Chunked<T> {
    /// The element at `index`, adding default elements up to it first.
    pub fn slot(&mut self, index: usize) -> &mut T {
        while self.len <= index {
            self.push(T::default());
        }
        // The loop above made the slot.
        let chunk = &mut self.chunks[index >> CHUNK_SHIFT];
        &mut chunk[index & (CHUNK - 1)]
    }
}

/// Bytes per chunk of the name store: 1 MiB. A name never spans chunks.
const NAME_CHUNK_SHIFT: u32 = 20;
const NAME_CHUNK: usize = 1 << NAME_CHUNK_SHIFT;

/// Every name, stored once each, in 1 MiB chunks.
#[derive(Debug, Clone, Default)]
struct Names {
    chunks: Vec<Vec<u8>>,
}

impl Names {
    /// Stores `name`, returning where it starts.
    fn push(&mut self, name: &[u8]) -> Result<u32, CoreError> {
        let fits = self
            .chunks
            .last()
            .is_some_and(|chunk| chunk.len() + name.len() <= NAME_CHUNK);
        if !fits {
            self.chunks.push(Vec::with_capacity(NAME_CHUNK));
        }
        let index = self.chunks.len() - 1;
        let chunk = &mut self.chunks[index];
        let at = (index << NAME_CHUNK_SHIFT) | chunk.len();
        chunk.extend_from_slice(name);
        u32::try_from(at).map_err(|_| CoreError::ArenaFull("the names fill 4 GiB"))
    }

    fn get(&self, at: u32, len: u16) -> &[u8] {
        let at = at as usize;
        let chunk = &self.chunks[at >> NAME_CHUNK_SHIFT];
        let start = at & (NAME_CHUNK - 1);
        &chunk[start..start + usize::from(len)]
    }

    fn heap_bytes(&self) -> usize {
        self.chunks.iter().map(Vec::capacity).sum::<usize>()
            + self.chunks.capacity() * size_of::<Vec<u8>>()
    }
}

/// An entry as the arena holds it, read through [`Arena::entry`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryView<'a> {
    /// The entry's index.
    pub idx: EntryIdx,
    /// The name's bytes.
    pub name: &'a [u8],
    /// What the entry is.
    pub kind: Kind,
    /// Apparent size.
    pub apparent: u64,
    /// Allocated bytes.
    pub allocated: u64,
    /// Last modification, Unix nanoseconds.
    pub mtime_ns: i64,
    /// Last inode change, Unix nanoseconds.
    pub ctime_ns: i64,
    /// Storage flags.
    pub flags: Flags,
    /// The directory the entry is in (the root is its own parent).
    pub parent: DirId,
    /// A directory's own id.
    pub dir: Option<DirId>,
    /// A non-directory's hard-link count; 0 for a directory.
    pub nlink: u32,
}

/// The identity a hard-linked or cloned file keeps beside its record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Linked {
    /// The entry.
    pub entry: EntryIdx,
    /// Its device.
    pub dev: u64,
    /// Its inode number.
    pub ino: u64,
}

/// A hard-linked or cloned file's identity as the arena stores it: 16
/// bytes, the device an index into the arena's device table.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct SharedRec {
    entry: u32,
    dev: u32,
    /// The inode number, or the clone id.
    key: u64,
}

// The record sizes the memory budget rests on, checked by the compiler:
// a field added to a record fails the build, not only a test.
const _: () = assert!(size_of::<EntryRec>() == 48);
const _: () = assert!(size_of::<DirRec>() == 48);
const _: () = assert!(size_of::<SharedRec>() == 16);

/// A clone group member.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cloned {
    /// The entry.
    pub entry: EntryIdx,
    /// Its device.
    pub dev: u64,
    /// The clone group's id.
    pub clone_id: u64,
}

/// Where an entry crosses onto another filesystem or tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Crossing {
    /// The entry.
    pub entry: EntryIdx,
    /// The kind of boundary.
    pub boundary: Boundary,
    /// The device on the far side.
    pub dev: u64,
    /// The mount id on the far side.
    pub mnt_id: u64,
}

/// The scan in memory.
#[derive(Debug, Clone)]
pub struct Arena {
    attrs: Attrs,
    entries: Chunked<EntryRec>,
    dirs: Chunked<DirRec>,
    names: Names,
    /// `--attrs full` records, parallel to `entries` when collected.
    full: Chunked<FullRec>,
    linked: Chunked<SharedRec>,
    cloned: Chunked<SharedRec>,
    may_share: Chunked<EntryIdx>,
    /// The devices `linked` and `cloned` name, by index.
    devices: Vec<u64>,
    device_index: HashMap<u64, u32>,
    crossings: Vec<Crossing>,
    unreadable: HashMap<DirId, i32>,
    skipped: HashMap<DirId, Skip>,
    errors: Vec<ScanError>,
    volumes: Vec<VolumeInfo>,
    vanished: u64,
    root_dev: u64,
    root_mnt_id: u64,
}

impl Arena {
    /// An empty arena for a scan collecting `attrs`.
    pub fn new(attrs: Attrs) -> Arena {
        Arena {
            attrs,
            entries: Chunked::default(),
            dirs: Chunked::default(),
            names: Names::default(),
            full: Chunked::default(),
            linked: Chunked::default(),
            cloned: Chunked::default(),
            may_share: Chunked::default(),
            devices: Vec::new(),
            device_index: HashMap::new(),
            crossings: Vec::new(),
            unreadable: HashMap::new(),
            skipped: HashMap::new(),
            errors: Vec::new(),
            volumes: Vec::new(),
            vanished: 0,
            root_dev: 0,
            root_mnt_id: 0,
        }
    }

    /// Takes one event of the walk, in the order the walk delivers them.
    pub fn push(&mut self, event: ScanEvent) -> Result<(), CoreError> {
        match event {
            ScanEvent::Entry(entry) => self.push_entry(&entry)?,
            ScanEvent::DirClosed { dir, errno } => {
                if let Some(errno) = errno {
                    self.unreadable.insert(dir, errno);
                    self.dirs.slot(dir.index()).flags |= DirRec::UNREADABLE;
                }
            }
            ScanEvent::Vanished { .. } => self.vanished += 1,
            ScanEvent::Volume(volume) => self.volumes.push(volume),
            ScanEvent::Error(error) => self.errors.push(error),
            ScanEvent::Skipped { dir, reason } => {
                self.skipped.insert(dir, reason);
            }
        }
        Ok(())
    }

    fn push_entry(&mut self, entry: &Entry) -> Result<(), CoreError> {
        let full = |what| CoreError::ArenaFull(what);
        let idx = u32::try_from(self.entries.len()).map_err(|_| full("more than 4G entries"))?;
        let name_len = u16::try_from(entry.name.as_bytes().len())
            .map_err(|_| full("a name longer than 64 KiB"))?;
        let name_at = self.names.push(entry.name.as_bytes())?;
        let flags = u8::try_from(entry.flags.bits()).map_err(|_| full("a flag beyond bit 7"))?;
        let is_dir = entry.kind == Kind::Dir;
        let nlink_or_dir = match entry.dir {
            Some(dir) => dir.get(),
            None => entry.nlink,
        };
        self.entries.push(EntryRec {
            apparent: entry.apparent,
            allocated: entry.allocated,
            mtime_ns: entry.mtime_ns,
            ctime_ns: entry.ctime_ns,
            name_at,
            name_len,
            kind: kind_code(entry.kind),
            flags,
            parent: entry.parent.get(),
            nlink_or_dir,
        });
        let index = EntryIdx(idx);
        if let Some(dir) = entry.dir {
            let depth = if idx == 0 {
                0
            } else {
                self.dirs
                    .get(entry.parent.index())
                    .map_or(0, |p| p.depth.saturating_add(1))
            };
            let rec = self.dirs.slot(dir.index());
            rec.entry = idx;
            rec.depth = depth;
        }
        if self.attrs == Attrs::Full {
            let rec = entry.full.as_ref().map_or(FullRec::ABSENT, FullRec::new);
            self.full.push(rec);
        }
        if idx == 0 {
            self.root_dev = entry.dev;
            self.root_mnt_id = entry.mnt_id;
        }
        if !is_dir && entry.nlink > 1 {
            let dev = self.device(entry.dev)?;
            self.linked.push(SharedRec {
                entry: index.get(),
                dev,
                key: entry.ino,
            });
        }
        match entry.share {
            Share::Clone(clone_id) => {
                let dev = self.device(entry.dev)?;
                self.cloned.push(SharedRec {
                    entry: index.get(),
                    dev,
                    key: clone_id,
                });
            }
            Share::MayShare => self.may_share.push(index),
            Share::None | Share::Hardlink => {}
        }
        if is_dir && (entry.boundary != Boundary::None || idx == 0) {
            self.crossings.push(Crossing {
                entry: index,
                boundary: entry.boundary,
                dev: entry.dev,
                mnt_id: entry.mnt_id,
            });
        }
        Ok(())
    }

    /// Entries, the root included.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the arena holds nothing yet.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The directory ids the arena has a record for, the root included when
    /// it is a directory: one more than the highest id met, which sizes a
    /// table indexed by [`DirId`]. After a complete walk every one holds its
    /// directory's entry; after a truncated stream an id whose entry never
    /// arrived is a placeholder, which [`Arena::has_dir`] reports as absent.
    pub fn dir_count(&self) -> usize {
        self.dirs.len()
    }

    /// Whether `dir`'s entry is in the arena.
    pub fn has_dir(&self, dir: DirId) -> bool {
        self.dirs
            .get(dir.index())
            .is_some_and(|rec| rec.entry != DirRec::NO_ENTRY)
    }

    /// The attribute set the scan collected.
    pub fn attrs(&self) -> Attrs {
        self.attrs
    }

    /// The entry at `idx`.
    ///
    /// # Panics
    ///
    /// When `idx` is not an entry of this arena.
    pub fn entry(&self, idx: EntryIdx) -> EntryView<'_> {
        let rec = self.rec(idx);
        let kind = kind_of(rec.kind);
        let dir = (kind == Kind::Dir).then(|| DirId::new(rec.nlink_or_dir));
        EntryView {
            idx,
            name: self.names.get(rec.name_at, rec.name_len),
            kind,
            apparent: rec.apparent,
            allocated: rec.allocated,
            mtime_ns: rec.mtime_ns,
            ctime_ns: rec.ctime_ns,
            flags: Flags::from_bits_truncate(u32::from(rec.flags)),
            parent: DirId::new(rec.parent),
            dir,
            nlink: if dir.is_some() { 0 } else { rec.nlink_or_dir },
        }
    }

    fn rec(&self, idx: EntryIdx) -> &EntryRec {
        self.entries
            .get(idx.index())
            .unwrap_or_else(|| panic!("no entry {idx:?}"))
    }

    /// The record of `idx`, for a pass over many entries that reads a field
    /// or two of each: [`Arena::entry`] reads them all.
    ///
    /// # Panics
    ///
    /// When `idx` is not an entry of this arena.
    pub(crate) fn record(&self, idx: EntryIdx) -> &EntryRec {
        self.rec(idx)
    }

    /// Every entry's record, in index order.
    pub(crate) fn records(&self) -> impl Iterator<Item = &EntryRec> {
        self.entries.iter()
    }

    /// The name of `idx`.
    ///
    /// # Panics
    ///
    /// When `idx` is not an entry of this arena.
    pub(crate) fn name(&self, idx: EntryIdx) -> &[u8] {
        let rec = self.rec(idx);
        self.names.get(rec.name_at, rec.name_len)
    }

    /// The `--attrs full` attributes of `idx`; `None` in a basic scan, and
    /// for an entry the walk delivered without them in a full scan.
    pub fn full(&self, idx: EntryIdx) -> Option<FullAttrs> {
        self.full.get(idx.index()).and_then(|rec| rec.get())
    }

    /// The directory record of `dir`.
    ///
    /// # Panics
    ///
    /// When `dir` is not a directory of this arena, or its entry never
    /// arrived ([`Arena::has_dir`] is false): a placeholder must not pass
    /// for a directory.
    pub fn dir(&self, dir: DirId) -> &DirRec {
        self.dirs
            .get(dir.index())
            .filter(|rec| rec.entry != DirRec::NO_ENTRY)
            .unwrap_or_else(|| panic!("no directory {dir:?}"))
    }

    /// The directory record of `dir`, for the roll-up.
    pub(crate) fn dir_mut(&mut self, dir: DirId) -> &mut DirRec {
        self.dirs.slot(dir.index())
    }

    /// Every entry's record in index order, beside the directory records to
    /// write: for a pass that reads the one and adds into the other.
    pub(crate) fn records_and_dirs(&mut self) -> (impl Iterator<Item = &EntryRec>, DirsMut<'_>) {
        (self.entries.iter(), DirsMut(&mut self.dirs))
    }

    /// Every entry, in scan order.
    pub fn entries(&self) -> impl Iterator<Item = EntryView<'_>> {
        (0..self.entries.len()).map(|i| self.entry(EntryIdx(u32::try_from(i).unwrap_or(u32::MAX))))
    }

    /// The index of `dev` in the device table, added when new.
    fn device(&mut self, dev: u64) -> Result<u32, CoreError> {
        if let Some(&index) = self.device_index.get(&dev) {
            return Ok(index);
        }
        let index = u32::try_from(self.devices.len())
            .map_err(|_| CoreError::ArenaFull("more than 4 Gi devices"))?;
        self.devices.push(dev);
        self.device_index.insert(dev, index);
        Ok(index)
    }

    /// Hard-linked non-directories with their `(dev, ino)`, in scan order.
    pub fn linked(&self) -> impl Iterator<Item = Linked> + '_ {
        self.linked.iter().map(|rec| self.as_linked(rec))
    }

    /// How many hard-linked non-directories there are.
    pub fn linked_len(&self) -> usize {
        self.linked.len()
    }

    /// The `index`th hard-linked non-directory, in scan order.
    ///
    /// # Panics
    ///
    /// When there are not that many.
    pub fn linked_at(&self, index: usize) -> Linked {
        self.as_linked(
            self.linked
                .get(index)
                .unwrap_or_else(|| panic!("no link {index}")),
        )
    }

    fn as_linked(&self, rec: &SharedRec) -> Linked {
        Linked {
            entry: EntryIdx(rec.entry),
            dev: self.devices[rec.dev as usize],
            ino: rec.key,
        }
    }

    /// Clone group members, in scan order.
    pub fn cloned(&self) -> impl Iterator<Item = Cloned> + '_ {
        self.cloned.iter().map(|rec| self.as_cloned(rec))
    }

    /// How many clone group members there are.
    pub fn cloned_len(&self) -> usize {
        self.cloned.len()
    }

    /// The `index`th clone group member, in scan order.
    ///
    /// # Panics
    ///
    /// When there are not that many.
    pub fn cloned_at(&self, index: usize) -> Cloned {
        self.as_cloned(
            self.cloned
                .get(index)
                .unwrap_or_else(|| panic!("no clone {index}")),
        )
    }

    fn as_cloned(&self, rec: &SharedRec) -> Cloned {
        Cloned {
            entry: EntryIdx(rec.entry),
            dev: self.devices[rec.dev as usize],
            clone_id: rec.key,
        }
    }

    /// Files whose blocks may be shared without a clone id.
    pub fn may_share(&self) -> impl Iterator<Item = EntryIdx> + '_ {
        self.may_share.iter().copied()
    }

    /// The root and every directory on a boundary, with its device and
    /// mount id.
    pub fn crossings(&self) -> &[Crossing] {
        &self.crossings
    }

    /// Directories that could not be read, with the error number.
    pub fn unreadable(&self) -> &HashMap<DirId, i32> {
        &self.unreadable
    }

    /// Directories listed but not entered, and why.
    pub fn skipped(&self) -> &HashMap<DirId, Skip> {
        &self.skipped
    }

    /// Entries listed but not read.
    pub fn errors(&self) -> &[ScanError] {
        &self.errors
    }

    /// The volumes the walk met.
    pub fn volumes(&self) -> &[VolumeInfo] {
        &self.volumes
    }

    /// Entries that vanished between listing and reading.
    pub fn vanished(&self) -> u64 {
        self.vanished
    }

    /// The root's device and mount id.
    pub fn root_volume(&self) -> (u64, u64) {
        (self.root_dev, self.root_mnt_id)
    }

    /// The bytes the arena holds on the heap: what it adds to the peak
    /// resident size.
    pub fn heap_bytes(&self) -> usize {
        self.entries.heap_bytes()
            + self.dirs.heap_bytes()
            + self.names.heap_bytes()
            + self.full.heap_bytes()
            + self.linked.heap_bytes()
            + self.cloned.heap_bytes()
            + self.may_share.heap_bytes()
            + self.devices.capacity() * size_of::<u64>()
            + self.crossings.capacity() * size_of::<Crossing>()
            + self.errors.capacity() * size_of::<ScanError>()
            + self.volumes.capacity() * size_of::<VolumeInfo>()
    }
}

/// A kind as the record stores it.
fn kind_code(kind: Kind) -> u8 {
    match kind {
        Kind::Dir => 0,
        Kind::File => 1,
        Kind::Symlink => 2,
        Kind::Socket => 3,
        Kind::Fifo => 4,
        Kind::Char => 5,
        Kind::Block => 6,
        Kind::Unknown => 7,
    }
}

/// The kind a record stores.
fn kind_of(code: u8) -> Kind {
    match code {
        0 => Kind::Dir,
        1 => Kind::File,
        2 => Kind::Symlink,
        3 => Kind::Socket,
        4 => Kind::Fifo,
        5 => Kind::Char,
        6 => Kind::Block,
        _ => Kind::Unknown,
    }
}

#[cfg(test)]
mod tests;
