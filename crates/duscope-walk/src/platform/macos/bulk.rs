//! Reading a directory with `getattrlistbulk(2)`: the attribute request,
//! the decoding of its packed records, and dua-core's CPU-versus-wall probe
//! that hands an I/O-bound directory to the stat queue.
//!
//! A record is the `u32` length, the returned-attributes bitmap, then each
//! requested attribute in bit order, common first, then the directory or the
//! file group (by the object's type), then the extended common group. Under
//! `FSOPT_PACK_INVAL_ATTRS` an attribute the object lacks still takes its
//! place, and the bitmap says which values are real.

use std::io;
use std::os::fd::BorrowedFd;
use std::time::{Duration, Instant};

use libc::attrgroup_t;

use super::ffi::{self, AttrBuf};
use crate::{Boundary, Entry, Flags, Kind, NameRef, Share, WalkOptions};

/// The bulk buffer: Apple's `fts` uses 32 KiB, dumac 128 KB.
pub(super) const BUFFER_BYTES: usize = 64 * 1024;

/// `st_blocks` counts 512-byte units, and `du` sums them.
const BLOCK: u64 = 512;

/// The common attributes of the basic set, in packing order after
/// `ATTR_CMN_RETURNED_ATTRS` (and `ATTR_CMN_ERROR` in a bulk read).
const COMMON: attrgroup_t = libc::ATTR_CMN_RETURNED_ATTRS
    | libc::ATTR_CMN_NAME
    | libc::ATTR_CMN_DEVID
    | libc::ATTR_CMN_OBJTYPE
    | libc::ATTR_CMN_MODTIME
    | libc::ATTR_CMN_CHGTIME
    | libc::ATTR_CMN_FLAGS
    | libc::ATTR_CMN_FILEID;
const DIR: attrgroup_t = libc::ATTR_DIR_LINKCOUNT
    | libc::ATTR_DIR_MOUNTSTATUS
    | libc::ATTR_DIR_ALLOCSIZE
    | libc::ATTR_DIR_DATALENGTH;
const FILE: attrgroup_t = libc::ATTR_FILE_LINKCOUNT
    | libc::ATTR_FILE_ALLOCSIZE
    | libc::ATTR_FILE_DATALENGTH
    | libc::ATTR_FILE_DATAALLOCSIZE;
/// The extended common attributes `--clones` adds, under
/// `FSOPT_ATTR_CMN_EXTENDED`.
const CLONES: attrgroup_t = libc::ATTR_CMNEXT_CLONEID | libc::ATTR_CMNEXT_EXT_FLAGS;

/// The attributes an entry cannot be built without; when the filesystem
/// leaves one out, the entry is read again with `lstat`.
const REQUIRED_COMMON: attrgroup_t = COMMON & !libc::ATTR_CMN_RETURNED_ATTRS;
const REQUIRED_DIR: attrgroup_t = libc::ATTR_DIR_ALLOCSIZE | libc::ATTR_DIR_DATALENGTH;
const REQUIRED_FILE: attrgroup_t =
    libc::ATTR_FILE_LINKCOUNT | libc::ATTR_FILE_ALLOCSIZE | libc::ATTR_FILE_DATALENGTH;

/// `ATTR_DIR_MOUNTSTATUS`: the directory is a mount point.
const DIR_MNTSTATUS_MNTPOINT: u32 = libc::DIR_MNTSTATUS_MNTPOINT;

/// An attribute request: the attribute list and the options for it.
#[derive(Debug, Clone, Copy)]
pub(super) struct Request {
    pub(super) attrs: libc::attrlist,
    pub(super) options: u32,
    /// A bulk read, which does not cross mounts.
    pub(super) bulk: bool,
}

impl Request {
    /// The request of a bulk read under `opts`.
    pub(super) fn bulk(opts: &WalkOptions) -> Request {
        let mut request = Request::single(opts);
        request.attrs.commonattr |= ffi::ATTR_CMN_ERROR;
        request.options &= !libc::FSOPT_NOFOLLOW;
        request.bulk = true;
        request
    }

    /// The request for one entry by name or path (`getattrlistat`,
    /// `getattrlist`), which takes no `ATTR_CMN_ERROR` and does not follow a
    /// final symlink.
    pub(super) fn single(opts: &WalkOptions) -> Request {
        let mut request = Request {
            attrs: libc::attrlist {
                bitmapcount: libc::ATTR_BIT_MAP_COUNT,
                reserved: 0,
                commonattr: COMMON,
                volattr: 0,
                dirattr: DIR,
                fileattr: FILE,
                forkattr: 0,
            },
            options: libc::FSOPT_PACK_INVAL_ATTRS | libc::FSOPT_NOFOLLOW,
            bulk: false,
        };
        if opts.clones {
            request.attrs.forkattr = CLONES;
            request.options |= libc::FSOPT_ATTR_CMN_EXTENDED;
        }
        request
    }

    /// Whether the request asks for extended common attributes, which some
    /// filesystems refuse.
    pub(super) fn extended(&self) -> bool {
        self.attrs.forkattr != 0
    }

    /// The request without the extended common attributes.
    pub(super) fn without_extended(mut self) -> Request {
        self.attrs.forkattr = 0;
        self.options &= !libc::FSOPT_ATTR_CMN_EXTENDED;
        self
    }
}

/// A time as `getattrlist` packs it: a `struct timespec`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Timespec {
    pub(super) sec: i64,
    pub(super) nsec: i64,
}

impl Timespec {
    /// Unix nanoseconds, saturating at the ends of `i64`.
    pub(super) fn ns(self) -> i64 {
        let ns = i128::from(self.sec) * 1_000_000_000 + i128::from(self.nsec);
        i64::try_from(ns).unwrap_or(if ns < 0 { i64::MIN } else { i64::MAX })
    }
}

/// One decoded record. Values the bitmap does not mark returned are left at
/// their packed defaults; [`Record::entry`] checks the bitmap.
#[derive(Debug, Clone, Default)]
pub(super) struct Record<'a> {
    pub(super) returned: [attrgroup_t; 5],
    pub(super) error: u32,
    pub(super) name: Option<&'a [u8]>,
    pub(super) dev: i32,
    pub(super) objtype: u32,
    pub(super) mtime: Timespec,
    pub(super) ctime: Timespec,
    pub(super) flags: u32,
    pub(super) ino: u64,
    pub(super) links: u32,
    pub(super) mount_status: u32,
    pub(super) alloc: u64,
    pub(super) len: u64,
    pub(super) data_alloc: u64,
    pub(super) clone_id: u64,
    pub(super) ext_flags: u64,
}

/// What a record became.
#[derive(Debug)]
pub(super) enum Decoded {
    /// A complete entry.
    Entry(Entry),
    /// The entry's attributes are incomplete, or describe what lies beneath
    /// a mount point: it is read again by name.
    Restat(NameRef, Option<Boundary>),
    /// The entry was listed but could not be read.
    Error(NameRef, i32),
    /// The record has no name; nothing can be said about it.
    Nameless,
}

const COMMON_GROUP: usize = 0;
const DIR_GROUP: usize = 2;
const FILE_GROUP: usize = 3;

impl Record<'_> {
    fn has(&self, group: usize, attrs: attrgroup_t) -> bool {
        self.returned[group] & attrs == attrs
    }

    /// The entry this record describes.
    pub(super) fn decode(&self, request: &Request) -> Decoded {
        let Some(name) = self
            .name
            .filter(|_| self.has(COMMON_GROUP, libc::ATTR_CMN_NAME))
        else {
            return Decoded::Nameless;
        };
        let name = NameRef::new(name);
        if self.error != 0 {
            return Decoded::Error(name, i32::try_from(self.error).unwrap_or(i32::MAX));
        }
        match self.entry(name, request) {
            Ok(entry) => Decoded::Entry(entry),
            Err((name, boundary)) => Decoded::Restat(name, boundary),
        }
    }

    /// The entry, or its name back when it must be read again by name.
    fn entry(
        &self,
        name: NameRef,
        request: &Request,
    ) -> Result<Entry, (NameRef, Option<Boundary>)> {
        if !self.has(COMMON_GROUP, REQUIRED_COMMON) {
            return Err((name, None));
        }
        let kind = kind(self.objtype);
        let complete = if kind == Kind::Dir {
            self.has(DIR_GROUP, REQUIRED_DIR)
        } else {
            self.has(FILE_GROUP, REQUIRED_FILE)
        };
        if !complete {
            return Err((name, None));
        }
        // A bulk read does not cross mounts: a mount point's record describes
        // the directory beneath it, not the mounted root.
        if request.bulk
            && kind == Kind::Dir
            && self.has(DIR_GROUP, libc::ATTR_DIR_MOUNTSTATUS)
            && self.mount_status & DIR_MNTSTATUS_MNTPOINT != 0
        {
            return Err((name, Some(Boundary::Mount)));
        }
        Ok(build(Fields {
            name,
            kind,
            apparent: self.len,
            allocated: self.alloc.div_ceil(BLOCK) * BLOCK,
            mtime_ns: self.mtime.ns(),
            ctime_ns: self.ctime.ns(),
            nlink: self.links,
            dev: dev(self.dev),
            ino: self.ino,
            bsd_flags: self.flags,
        }))
    }
}

/// The kind of a vnode type.
pub(super) fn kind(objtype: u32) -> Kind {
    match objtype {
        ffi::VREG => Kind::File,
        ffi::VDIR => Kind::Dir,
        ffi::VLNK => Kind::Symlink,
        ffi::VSOCK => Kind::Socket,
        ffi::VFIFO => Kind::Fifo,
        ffi::VCHR => Kind::Char,
        ffi::VBLK => Kind::Block,
        _ => Kind::Unknown,
    }
}

/// A `dev_t` as `std::os::unix::fs::MetadataExt::dev` reports it: macOS's
/// signed 32-bit value, sign-extended.
pub(super) fn dev(dev: i32) -> u64 {
    i64::from(dev).cast_unsigned()
}

/// What both readers, the bulk records and `lstat`, know of an entry.
pub(super) struct Fields {
    pub(super) name: NameRef,
    pub(super) kind: Kind,
    pub(super) apparent: u64,
    pub(super) allocated: u64,
    pub(super) mtime_ns: i64,
    pub(super) ctime_ns: i64,
    pub(super) nlink: u32,
    pub(super) dev: u64,
    pub(super) ino: u64,
    /// `st_flags` / `ATTR_CMN_FLAGS`.
    pub(super) bsd_flags: u32,
}

/// The entry both readers build from what they read, so the two agree
/// field for field.
pub(super) fn build(fields: Fields) -> Entry {
    let Fields {
        name,
        kind,
        apparent,
        allocated,
        mtime_ns,
        ctime_ns,
        nlink,
        dev,
        ino,
        bsd_flags,
    } = fields;
    // Devices, sockets and pipes hold no blocks of their own.
    let allocated = match kind {
        Kind::Socket | Kind::Fifo | Kind::Char | Kind::Block => 0,
        _ => allocated,
    };
    let share = if kind != Kind::Dir && nlink > 1 {
        Share::Hardlink
    } else {
        Share::None
    };
    let mut flags = Flags::empty();
    flags.set(Flags::DATALESS, bsd_flags & ffi::SF_DATALESS != 0);
    Entry {
        name,
        parent: crate::DirId::ROOT,
        dir: None,
        kind,
        apparent,
        allocated,
        mtime_ns,
        ctime_ns,
        nlink,
        dev,
        ino,
        // A macOS volume is identified by its device id.
        mnt_id: dev,
        share,
        boundary: Boundary::None,
        flags,
        full: None,
    }
}

/// Reads the packed values of a record in order.
struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn take<const N: usize>(&mut self) -> Option<[u8; N]> {
        let end = self.at.checked_add(N)?;
        let bytes = self.bytes.get(self.at..end)?.try_into().ok()?;
        self.at = end;
        Some(bytes)
    }

    fn u32(&mut self) -> Option<u32> {
        self.take().map(u32::from_ne_bytes)
    }

    fn i32(&mut self) -> Option<i32> {
        self.take().map(i32::from_ne_bytes)
    }

    fn u64(&mut self) -> Option<u64> {
        self.take().map(u64::from_ne_bytes)
    }

    fn i64(&mut self) -> Option<i64> {
        self.take().map(i64::from_ne_bytes)
    }

    fn timespec(&mut self) -> Option<Timespec> {
        Some(Timespec {
            sec: self.i64()?,
            nsec: self.i64()?,
        })
    }

    /// An `attrreference_t`'s data: its offset is relative to the reference
    /// itself. A trailing NUL is dropped.
    fn reference(&mut self) -> Option<&'a [u8]> {
        let at = self.at;
        let offset = self.i32()?;
        let len = usize::try_from(self.u32()?).ok()?;
        let start = at.checked_add_signed(isize::try_from(offset).ok()?)?;
        let data = self.bytes.get(start..start.checked_add(len)?)?;
        Some(data.strip_suffix(&[0]).unwrap_or(data))
    }
}

/// The record at the start of `bytes`, and its length.
pub(super) fn record_len(bytes: &[u8]) -> Option<usize> {
    let len = usize::try_from(u32::from_ne_bytes(bytes.get(..4)?.try_into().ok()?)).ok()?;
    (len >= 24 && len <= bytes.len()).then_some(len)
}

/// Decodes one record packed for `request`. `None` when the record is cut
/// short; a well-formed record never is.
pub(super) fn parse<'a>(record: &'a [u8], request: &Request) -> Option<Record<'a>> {
    let attrs = &request.attrs;
    let mut c = Cursor {
        bytes: record,
        at: 4,
    };
    let mut r = Record {
        returned: [c.u32()?, c.u32()?, c.u32()?, c.u32()?, c.u32()?],
        ..Record::default()
    };
    let common = attrs.commonattr;
    if r.returned[COMMON_GROUP] & ffi::ATTR_CMN_ERROR != 0 {
        r.error = c.u32()?;
    }
    if common & libc::ATTR_CMN_NAME != 0 {
        r.name = c.reference();
    }
    if common & libc::ATTR_CMN_DEVID != 0 {
        r.dev = c.i32()?;
    }
    if common & libc::ATTR_CMN_OBJTYPE != 0 {
        r.objtype = c.u32()?;
    }
    if common & libc::ATTR_CMN_MODTIME != 0 {
        r.mtime = c.timespec()?;
    }
    if common & libc::ATTR_CMN_CHGTIME != 0 {
        r.ctime = c.timespec()?;
    }
    if common & libc::ATTR_CMN_FLAGS != 0 {
        r.flags = c.u32()?;
    }
    if common & libc::ATTR_CMN_FILEID != 0 {
        r.ino = c.u64()?;
    }
    if r.error != 0 {
        // Only the name matters for an entry that could not be read.
        return Some(r);
    }
    if r.objtype == ffi::VDIR {
        let dir = attrs.dirattr;
        if dir & libc::ATTR_DIR_LINKCOUNT != 0 {
            r.links = c.u32()?;
        }
        if dir & libc::ATTR_DIR_MOUNTSTATUS != 0 {
            r.mount_status = c.u32()?;
        }
        if dir & libc::ATTR_DIR_ALLOCSIZE != 0 {
            r.alloc = c.u64()?;
        }
        if dir & libc::ATTR_DIR_DATALENGTH != 0 {
            r.len = c.u64()?;
        }
    } else {
        let file = attrs.fileattr;
        if file & libc::ATTR_FILE_LINKCOUNT != 0 {
            r.links = c.u32()?;
        }
        if file & libc::ATTR_FILE_ALLOCSIZE != 0 {
            r.alloc = c.u64()?;
        }
        if file & libc::ATTR_FILE_DATALENGTH != 0 {
            r.len = c.u64()?;
        }
        if file & libc::ATTR_FILE_DATAALLOCSIZE != 0 {
            r.data_alloc = c.u64()?;
        }
    }
    let fork = attrs.forkattr;
    if fork & libc::ATTR_CMNEXT_CLONEID != 0 {
        r.clone_id = c.u64()?;
    }
    if fork & libc::ATTR_CMNEXT_EXT_FLAGS != 0 {
        r.ext_flags = c.u64()?;
    }
    Some(r)
}

/// dua-core's probe: a bulk refill that spends less than half its wall time
/// on the CPU was waiting on I/O, and two such refills in a row at the start
/// of a directory hand it to the stat queue, where every worker can wait on
/// its entries at once (dua-cli measured 2.5x on a >1M-entry directory).
/// The ratio needs no latency threshold, so a slow CPU never looks like slow
/// I/O.
#[derive(Debug, Default)]
pub(super) struct Probe {
    samples: u8,
    waiting: u8,
}

impl Probe {
    /// Records one refill; true once the directory should go to the stat
    /// queue. A refill that was not waiting, or a CPU clock that could not
    /// be read or went backwards, ends the probe.
    pub(super) fn sample(&mut self, cpu: Option<Duration>, wall: Duration) -> bool {
        if self.samples >= 2 {
            return false;
        }
        self.samples += 1;
        let waiting = cpu.is_some_and(|cpu| !cpu.is_zero() && cpu < wall / 2);
        if waiting {
            self.waiting += 1;
        } else {
            self.samples = 2;
        }
        self.waiting >= 2
    }

    /// Whether the probe is still taking samples.
    pub(super) fn active(&self) -> bool {
        self.samples < 2
    }
}

/// How a bulk read of a directory ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Outcome {
    /// Every entry was read.
    Done,
    /// The filesystem does not do bulk reads (`ENOTSUP`, `EINVAL`).
    Unsupported,
    /// The probe found the directory I/O-bound; nothing was kept.
    IoBound,
    /// The directory can be listed but not searched (`EACCES` on the first
    /// read): its names are known, but none of its entries can be read.
    Unsearchable,
}

/// Reads every record of the directory open on `dir` into `out`, calling
/// `restat` for each entry that must be read again by name. With `probe`,
/// the first two refills are timed against the thread's CPU clock.
pub(super) fn read(
    dir: BorrowedFd<'_>,
    opts: &WalkOptions,
    buf: &mut AttrBuf,
    probe: bool,
    out: &mut Vec<Decoded>,
) -> io::Result<Outcome> {
    let mut request = Request::bulk(opts);
    let mut timer = Probe::default();
    let mut fills = 0usize;
    loop {
        let timing = (probe && timer.active()).then(|| (ffi::thread_cpu_time(), Instant::now()));
        let count = match ffi::getattrlistbulk(dir, &request.attrs, buf, u64::from(request.options))
        {
            Ok(0) => return Ok(Outcome::Done),
            Ok(count) => count,
            Err(e) if request.extended() && unsupported(&e) => {
                // Some filesystems refuse the extended attributes alone;
                // nothing was read, so asking again without them is safe.
                request = request.without_extended();
                continue;
            }
            Err(e) if fills == 0 && unsupported(&e) => return Ok(Outcome::Unsupported),
            Err(e) if fills == 0 && e.raw_os_error() == Some(libc::EACCES) => {
                return Ok(Outcome::Unsearchable);
            }
            Err(e) => return Err(e),
        };
        fills += 1;
        if let Some((cpu_start, start)) = timing {
            let cpu = cpu_start.and_then(|s| ffi::thread_cpu_time()?.checked_sub(s));
            if timer.sample(cpu, start.elapsed()) {
                out.clear();
                return Ok(Outcome::IoBound);
            }
        }
        let mut bytes = buf.bytes();
        for _ in 0..count {
            let Some(len) = record_len(bytes) else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "getattrlistbulk returned a record longer than its buffer",
                ));
            };
            let decoded =
                parse(&bytes[..len], &request).map_or(Decoded::Nameless, |r| r.decode(&request));
            out.push(decoded);
            bytes = &bytes[len..];
        }
    }
}

/// Whether an error means the filesystem cannot serve the request.
pub(super) fn unsupported(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(libc::ENOTSUP | libc::EOPNOTSUPP | libc::EINVAL | libc::ENOSYS)
    ) || error.kind() == io::ErrorKind::Unsupported
}
