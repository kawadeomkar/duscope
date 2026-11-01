//! Import: a tree another tool wrote, read into an [`Arena`] as a walk
//! delivers one, for the roll-up and the index writer: ncdu's JSON export,
//! gdu's (ncdu's format, but with a directory's cumulative sizes), and
//! GrandPerspective's `.gpscan` (format 7).
//!
//! - **Streaming:** the file is read once, front to back. Memory holds the
//!   arena and the names of the open directories' entries, never the file.
//! - **Gzip** is recognised by its magic bytes, whatever the format, and
//!   read through a cap of [`DECOMPRESSED_CAP`] bytes of decompressed data.
//! - **The format** is the one asked for, or else told from the file: XML
//!   is a `.gpscan`; JSON is gdu's when its `progname` is `gdu`, else
//!   ncdu's.
//! - **Directory sizes:** ncdu's are each directory's own; gdu's include
//!   everything under it, so a gdu directory's own sizes are taken as 0 and
//!   the roll-up adds the totals up from the entries; a `.gpscan` gives no
//!   folder sizes.
//! - **What the formats do not hold** takes a value of its own: an import
//!   is a basic scan (no access or creation times); the change time is the
//!   modification time; a `.gpscan`'s one size measure stands for both;
//!   an unreadable directory's error is `EACCES`, as the formats do not say
//!   which; a hard link without an inode number is a file of one link.
//! - **Names** must be names: not empty, not `.` or `..`, without `/` or a
//!   NUL byte, at most 64 KiB, and unique in their directory. The root is
//!   the path the file gives the tree.
//! - **Sizes** may add up to 8 EiB (`i64::MAX` bytes), as the index's
//!   columns hold.

// GrandPerspective is a name, not code.
#![allow(clippy::doc_markdown)]

mod gpscan;
mod ncdu;

use std::collections::HashSet;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{self, BufRead, BufReader, Read};

use duscope_walk::{
    Attrs, Boundary, DirId, Entry, Flags, Kind, NameRef, ScanError, ScanEvent, Share, Skip,
};
use flate2::read::MultiGzDecoder;

use crate::CoreError;
use crate::arena::Arena;

/// The most decompressed data a gzipped file may hold: 1 GiB.
pub const DECOMPRESSED_CAP: u64 = 1 << 30;

/// The error number of a directory the tool could not read.
const EACCES: i32 = 13;

/// The longest name the arena holds.
const NAME_MAX: usize = u16::MAX as usize;

/// The tool that wrote a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportFormat {
    /// ncdu's JSON export.
    Ncdu,
    /// gdu's JSON export.
    Gdu,
    /// GrandPerspective's scan dump.
    Gpscan,
}

impl ImportFormat {
    /// The format's name, as `--format` spells it.
    pub fn as_str(self) -> &'static str {
        match self {
            ImportFormat::Ncdu => "ncdu",
            ImportFormat::Gdu => "gdu",
            ImportFormat::Gpscan => "gpscan",
        }
    }

    /// `scan_meta.source` for an index imported from it.
    pub fn source(self) -> &'static str {
        match self {
            ImportFormat::Ncdu => "import:ncdu",
            ImportFormat::Gdu => "import:gdu",
            ImportFormat::Gpscan => "import:gpscan",
        }
    }
}

/// A tree read from a file.
#[derive(Debug)]
pub struct Imported {
    /// The tree, as a walk would have delivered it; not yet rolled up.
    pub arena: Arena,
    /// The format it was read as.
    pub format: ImportFormat,
    /// The root's path, as the file gives it.
    pub root: Vec<u8>,
    /// When the tool read the tree, Unix nanoseconds, where the file says.
    pub scanned_at: Option<i64>,
    /// Directories the tool could not read.
    pub unreadable_dirs: u64,
    /// Entries the tool could not read.
    pub entry_errors: u64,
    /// Directories the tool listed but did not enter.
    pub skipped_dirs: u64,
    /// Entries an exclude pattern left out, and directories it kept the
    /// tool out of.
    pub excluded: u64,
    /// Directories the tool did not enter because they are other volumes.
    pub mounts_excluded: u64,
}

/// Reads the tree in `input`, as `format` or the format the file shows.
///
/// # Errors
///
/// [`CoreError::ImportInvalid`] for a file that is not what its format
/// says, or that decompresses past [`DECOMPRESSED_CAP`];
/// [`CoreError::ImportRead`] when `input` cannot be read.
pub fn import(input: impl Read, format: Option<ImportFormat>) -> Result<Imported, CoreError> {
    import_capped(input, format, DECOMPRESSED_CAP)
}

/// [`import`], with the decompressed data capped at `cap` bytes.
fn import_capped(
    input: impl Read,
    format: Option<ImportFormat>,
    cap: u64,
) -> Result<Imported, CoreError> {
    let (head, rest) = peel(input, 2)?;
    let gzip = head.starts_with(&[0x1f, 0x8b]);
    let input = io::Cursor::new(head).chain(rest);
    if gzip {
        let capped = Capped {
            inner: MultiGzDecoder::new(input),
            left: cap,
        };
        read(Box::new(capped), format)
    } else {
        read(Box::new(input), format)
    }
}

/// Reads the tree in the (decompressed) `input`, through one reader type,
/// so that each format's reader is compiled once.
fn read(input: Box<dyn Read + '_>, format: Option<ImportFormat>) -> Result<Imported, CoreError> {
    // A byte-order mark before the first character, whatever the format.
    let (mut head, rest) = peel(input, 3)?;
    if head.starts_with(b"\xef\xbb\xbf") {
        head.clear();
    }
    let mut input = BufReader::new(io::Cursor::new(head).chain(rest));
    let format = match format {
        Some(format) => Some(format),
        None => sniff(&mut input)?,
    };
    match format {
        Some(ImportFormat::Gpscan) => gpscan::read(&mut input),
        format => ncdu::read(&mut input, format),
    }
}

/// The first `n` bytes of `input` (fewer only at its end), however short
/// its reads, and the rest of it.
fn peel<R: Read>(mut input: R, n: u64) -> Result<(Vec<u8>, R), CoreError> {
    let mut head = Vec::new();
    (&mut input)
        .take(n)
        .read_to_end(&mut head)
        .map_err(|e| read_error(&e))?;
    Ok((head, input))
}

/// The format the file's first character shows: `<` for XML, a
/// `.gpscan`; `None` for JSON, whose metadata tells ncdu's from gdu's.
fn sniff(input: &mut dyn BufRead) -> Result<Option<ImportFormat>, CoreError> {
    loop {
        let buf = input.fill_buf().map_err(|e| read_error(&e))?;
        let Some(&first) = buf.first() else {
            return Err(CoreError::ImportInvalid("the file is empty".to_owned()));
        };
        if first.is_ascii_whitespace() {
            input.consume(1);
            continue;
        }
        return match first {
            b'<' => Ok(Some(ImportFormat::Gpscan)),
            b'[' => Ok(None),
            _ => Err(CoreError::ImportInvalid(
                "the file is neither JSON (ncdu, gdu) nor XML (a .gpscan)".to_owned(),
            )),
        };
    }
}

/// An error reading the file: the cap, a damaged or cut-off gzip stream,
/// or the file itself.
fn read_error(error: &io::Error) -> CoreError {
    let cap = error
        .get_ref()
        .is_some_and(<dyn std::error::Error + Send + Sync>::is::<CapPassed>);
    if cap {
        return CoreError::ImportInvalid(format!(
            "the file decompresses to more than {DECOMPRESSED_CAP} bytes, the import's cap"
        ));
    }
    match error.kind() {
        io::ErrorKind::InvalidData | io::ErrorKind::InvalidInput => {
            CoreError::ImportInvalid(format!("the gzip stream is damaged: {error}"))
        }
        io::ErrorKind::UnexpectedEof => {
            CoreError::ImportInvalid("the gzip stream ends early".to_owned())
        }
        _ => CoreError::ImportRead(io::Error::new(error.kind(), error.to_string())),
    }
}

/// Past the decompression cap.
#[derive(Debug)]
struct CapPassed;

impl std::fmt::Display for CapPassed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("past the decompression cap")
    }
}

impl std::error::Error for CapPassed {}

/// A reader that fails once more than its cap has been read.
struct Capped<R> {
    inner: R,
    left: u64,
}

impl<R: Read> Read for Capped<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.left = self
            .left
            .checked_sub(n as u64)
            .ok_or_else(|| io::Error::other(CapPassed))?;
        Ok(n)
    }
}

/// One entry, as a format's reader hands it over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Item {
    pub name: Vec<u8>,
    pub kind: Kind,
    pub apparent: u64,
    pub allocated: u64,
    pub mtime_ns: i64,
    /// Its device, where the file gives one; else its directory's.
    pub dev: Option<u64>,
    /// A hard-linked file's inode number.
    pub ino: Option<u64>,
    pub nlink: u32,
    /// A directory the tool could not read, or a file it could not stat.
    pub unreadable: bool,
    /// A directory the tool listed but did not enter.
    pub skipped: Option<Skip>,
    /// An entry an exclude pattern left out, of no kind the file says: it is
    /// counted, not listed, as a walk leaves out an excluded file.
    pub left_out: bool,
}

impl Item {
    /// An entry of `kind` named `name`, with nothing else known.
    pub fn new(name: Vec<u8>, kind: Kind) -> Item {
        Item {
            name,
            kind,
            apparent: 0,
            allocated: 0,
            mtime_ns: 0,
            dev: None,
            ino: None,
            nlink: 1,
            unreadable: false,
            skipped: None,
            left_out: false,
        }
    }
}

/// A directory whose entries are still coming.
struct Open {
    dir: DirId,
    dev: u64,
    unreadable: bool,
    /// Hashes of the entries' names so far, which must not repeat.
    names: HashSet<u64>,
}

/// Turns a format's entries into the events of a walk, into an arena.
pub(crate) struct Builder {
    arena: Arena,
    open: Vec<Open>,
    next_dir: u32,
    root: Option<Vec<u8>>,
    unreadable_dirs: u64,
    entry_errors: u64,
    skipped_dirs: u64,
    excluded: u64,
    mounts_excluded: u64,
    /// Every entry's sizes so far, added up: at most `i64::MAX`, so that no
    /// total of the roll-up or column of the index can overflow.
    sum_apparent: u64,
    sum_allocated: u64,
}

impl Builder {
    pub fn new() -> Builder {
        Builder {
            arena: Arena::new(Attrs::Basic),
            open: Vec::new(),
            next_dir: 0,
            root: None,
            unreadable_dirs: 0,
            entry_errors: 0,
            skipped_dirs: 0,
            excluded: 0,
            mounts_excluded: 0,
            sum_apparent: 0,
            sum_allocated: 0,
        }
    }

    /// The directories open, the root's included.
    pub fn depth(&self) -> usize {
        self.open.len()
    }

    /// Opens a directory, the root first (named by its path): its entries
    /// follow, until [`Builder::close`].
    pub fn open(&mut self, item: &Item) -> Result<(), CoreError> {
        let dir = self.dir_entry(item)?;
        let dev = self.dev(item);
        self.open.push(Open {
            dir,
            dev,
            unreadable: item.unreadable,
            names: HashSet::new(),
        });
        Ok(())
    }

    /// Closes the directory opened last.
    pub fn close(&mut self) -> Result<(), CoreError> {
        let open = self
            .open
            .pop()
            .ok_or_else(|| invalid("a directory closes that was not opened"))?;
        if open.unreadable {
            self.unreadable_dirs += 1;
        }
        self.push(ScanEvent::DirClosed {
            dir: open.dir,
            errno: open.unreadable.then_some(EACCES),
        })
    }

    /// An entry with nothing under it: a file, or a directory with no
    /// entries, one the tool did not enter, or one it could not read.
    pub fn leaf(&mut self, item: &Item) -> Result<(), CoreError> {
        if self.open.is_empty() {
            return Err(invalid("the tree's root is not a directory"));
        }
        if item.left_out {
            self.excluded += 1;
            return Ok(());
        }
        if item.kind == Kind::Dir {
            let skipped = item.skipped;
            let unreadable = item.unreadable;
            let dir = self.dir_entry(item)?;
            if let Some(reason) = skipped {
                self.skipped_dirs += 1;
                match reason {
                    Skip::Excluded | Skip::ExcludedCache => self.excluded += 1,
                    _ => self.mounts_excluded += 1,
                }
                self.push(ScanEvent::Skipped { dir, reason })?;
            }
            if unreadable {
                self.unreadable_dirs += 1;
            }
            return self.push(ScanEvent::DirClosed {
                dir,
                errno: unreadable.then_some(EACCES),
            });
        }
        let parent = self.check_name(&item.name)?;
        if item.unreadable {
            // As a walk reports an entry it could not stat: an error of its
            // directory, not an entry.
            self.entry_errors += 1;
            return self.push(ScanEvent::Error(ScanError {
                dir: parent,
                name: NameRef::new(&item.name),
                errno: EACCES,
            }));
        }
        let dev = self.dev(item);
        let nlink = if item.ino.is_some() {
            item.nlink.max(1)
        } else {
            1
        };
        self.push(ScanEvent::Entry(Entry {
            name: NameRef::new(&item.name),
            parent,
            dir: None,
            kind: item.kind,
            apparent: item.apparent,
            allocated: item.allocated,
            mtime_ns: item.mtime_ns,
            ctime_ns: item.mtime_ns,
            nlink,
            dev,
            ino: item.ino.unwrap_or(0),
            mnt_id: dev,
            share: if nlink > 1 {
                Share::Hardlink
            } else {
                Share::None
            },
            boundary: Boundary::None,
            flags: Flags::empty(),
            full: None,
        }))
    }

    /// The tree read, once every directory is closed.
    pub fn finish(
        self,
        format: ImportFormat,
        scanned_at: Option<i64>,
    ) -> Result<Imported, CoreError> {
        if !self.open.is_empty() {
            return Err(invalid("the file ends inside a directory"));
        }
        let root = self.root.ok_or_else(|| invalid("the file holds no tree"))?;
        Ok(Imported {
            arena: self.arena,
            format,
            root,
            scanned_at,
            unreadable_dirs: self.unreadable_dirs,
            entry_errors: self.entry_errors,
            skipped_dirs: self.skipped_dirs,
            excluded: self.excluded,
            mounts_excluded: self.mounts_excluded,
        })
    }

    /// Pushes a directory's entry; its id.
    fn dir_entry(&mut self, item: &Item) -> Result<DirId, CoreError> {
        let dir = DirId::new(self.next_dir);
        self.next_dir = self
            .next_dir
            .checked_add(1)
            .ok_or_else(|| invalid("more than 4G directories"))?;
        let (name, parent) = if self.open.is_empty() {
            if self.root.is_some() {
                return Err(invalid("the file holds a second tree"));
            }
            let name = root_name(&item.name)?;
            self.root = Some(item.name.clone());
            (name, DirId::ROOT)
        } else {
            let parent = self.check_name(&item.name)?;
            (item.name.clone(), parent)
        };
        let dev = self.dev(item);
        self.push(ScanEvent::Entry(Entry {
            name: NameRef::new(&name),
            parent,
            dir: Some(dir),
            kind: Kind::Dir,
            apparent: item.apparent,
            allocated: item.allocated,
            mtime_ns: item.mtime_ns,
            ctime_ns: item.mtime_ns,
            nlink: 1,
            dev,
            ino: 0,
            mnt_id: dev,
            share: Share::None,
            boundary: Boundary::None,
            flags: Flags::empty(),
            full: None,
        }))?;
        Ok(dir)
    }

    /// `name` checked as an entry's name in the open directory, which it
    /// is now one of; the directory.
    fn check_name(&mut self, name: &[u8]) -> Result<DirId, CoreError> {
        let shown = || String::from_utf8_lossy(&name[..name.len().min(64)]).into_owned();
        if name.is_empty() || name == b"." || name == b".." {
            return Err(invalid(&format!("{:?} is not a name", shown())));
        }
        if name.contains(&b'/') || name.contains(&0) {
            return Err(invalid(&format!(
                "the name {:?} holds a slash or a NUL byte",
                shown()
            )));
        }
        if name.len() > NAME_MAX {
            return Err(invalid(&format!(
                "the name {:?}… is longer than 64 KiB",
                shown()
            )));
        }
        let open = self
            .open
            .last_mut()
            .ok_or_else(|| invalid("an entry outside the tree"))?;
        let mut hasher = DefaultHasher::new();
        name.hash(&mut hasher);
        if !open.names.insert(hasher.finish()) {
            return Err(invalid(&format!(
                "the name {:?} comes twice in one directory",
                shown()
            )));
        }
        Ok(open.dir)
    }

    /// `item`'s device: its own, or its directory's.
    fn dev(&self, item: &Item) -> u64 {
        item.dev
            .unwrap_or_else(|| self.open.last().map_or(0, |open| open.dev))
    }

    fn push(&mut self, event: ScanEvent) -> Result<(), CoreError> {
        if let ScanEvent::Entry(entry) = &event {
            let limit = |sum: u64, size: u64| {
                sum.checked_add(size)
                    .filter(|&total| i64::try_from(total).is_ok())
                    .ok_or_else(|| invalid("the sizes add up to more than 8 EiB"))
            };
            self.sum_apparent = limit(self.sum_apparent, entry.apparent)?;
            self.sum_allocated = limit(self.sum_allocated, entry.allocated)?;
        }
        self.arena.push(event)
    }
}

/// The root entry's name: the path's last component, or the path itself
/// for `/`.
fn root_name(path: &[u8]) -> Result<Vec<u8>, CoreError> {
    if path.is_empty() || path.contains(&0) {
        return Err(invalid("the tree's root has no path"));
    }
    let last = path
        .rsplit(|&b| b == b'/')
        .find(|part| !part.is_empty())
        .unwrap_or(path);
    if last.len() > NAME_MAX {
        return Err(invalid("the root's name is longer than 64 KiB"));
    }
    Ok(last.to_vec())
}

/// An [`CoreError::ImportInvalid`] saying `what`.
pub(crate) fn invalid(what: &str) -> CoreError {
    CoreError::ImportInvalid(what.to_owned())
}

/// The kind `st_mode`'s type bits name.
pub(crate) fn kind_of_mode(mode: u64) -> Kind {
    match mode & 0o170_000 {
        0o040_000 => Kind::Dir,
        0o100_000 => Kind::File,
        0o120_000 => Kind::Symlink,
        0o140_000 => Kind::Socket,
        0o010_000 => Kind::Fifo,
        0o020_000 => Kind::Char,
        0o060_000 => Kind::Block,
        _ => Kind::Unknown,
    }
}

#[cfg(test)]
mod tests;
