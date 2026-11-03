//! ncdu's JSON export, and gdu's, which follows it: `[1, minor, {metadata},
//! tree]`, a directory an array whose first element is its info object and
//! a file an info object (dev.yorhel.nl/ncdu/jsonfmt).
//!
//! The reader is a pull parser of its own over the bytes, for two reasons:
//! ncdu writes a name as the bytes the filesystem gave it, which need not
//! be UTF-8, and a tree nests as deep as the filesystem did. It keeps the
//! open directories on a stack, never recursing, and holds one info object
//! at a time. Keys it does not know are skipped, whatever their values.

use std::io::BufRead;

use duscope_walk::{Kind, Skip};

use super::{Builder, ImportFormat, Imported, Item, invalid, kind_of_mode, read_error};
use crate::CoreError;

/// The longest string kept: a root path, or a name (which the builder caps
/// at 64 KiB).
const STRING_MAX: usize = 1 << 20;

/// The deepest a skipped value may nest.
const SKIP_DEPTH_MAX: usize = 1 << 12;

/// Reads the tree; `format` is gdu's or ncdu's, or else the metadata's
/// `progname` tells.
pub(super) fn read(
    input: &mut dyn BufRead,
    format: Option<ImportFormat>,
) -> Result<Imported, CoreError> {
    let mut p = Parser { input, pos: 0 };
    p.expect(b'[')?;
    let major = p.unsigned()?;
    if major != 1 {
        return Err(p.invalid(&format!(
            "the export's major version is {major}, and only 1 is known"
        )));
    }
    p.expect(b',')?;
    p.unsigned()?;
    p.expect(b',')?;
    let meta = p.metadata()?;
    let format = format.unwrap_or(if meta.progname.as_deref() == Some(b"gdu") {
        ImportFormat::Gdu
    } else {
        ImportFormat::Ncdu
    });
    let scanned_at = meta
        .timestamp
        .map(|secs| {
            secs.checked_mul(1_000_000_000)
                .ok_or_else(|| p.invalid("the timestamp is out of range"))
        })
        .transpose()?;
    p.expect(b',')?;
    let mut tree = Builder::new();
    if p.white()? == Some(b'{') {
        return Err(p.invalid("the tree's root is not a directory"));
    }
    p.expect(b'[')?;
    let root = p.info()?;
    tree.open(&root.item(format, true)?)?;
    while tree.depth() > 0 {
        match p.white()? {
            Some(b']') => {
                p.bump();
                tree.close()?;
            }
            Some(b',') => {
                p.bump();
                match p.white()? {
                    Some(b'[') => {
                        p.bump();
                        let info = p.info()?;
                        tree.open(&info.item(format, true)?)?;
                    }
                    Some(b'{') => {
                        let info = p.info()?;
                        tree.leaf(&info.item(format, false)?)?;
                    }
                    _ => return Err(p.invalid("an entry is neither an object nor an array")),
                }
            }
            _ => return Err(p.invalid("a directory's entries go on without a comma")),
        }
    }
    p.expect(b']')?;
    if p.white()?.is_some() {
        return Err(p.invalid("data follows the export"));
    }
    tree.finish(format, scanned_at)
}

/// What the metadata object says.
#[derive(Debug, Default)]
struct Metadata {
    progname: Option<Vec<u8>>,
    timestamp: Option<i64>,
}

/// An info object's fields.
#[derive(Debug, Default)]
struct Info {
    name: Option<Vec<u8>>,
    asize: u64,
    dsize: u64,
    dev: Option<u64>,
    ino: Option<u64>,
    nlink: Option<u64>,
    hlnkc: bool,
    read_error: bool,
    notreg: bool,
    excluded: Option<Vec<u8>>,
    mode: Option<u64>,
    mtime: Option<i64>,
}

impl Info {
    /// The entry the object describes; `array` for a directory's array.
    fn item(self, format: ImportFormat, array: bool) -> Result<Item, CoreError> {
        let name = self.name.ok_or_else(|| invalid("an entry has no name"))?;
        let skipped = self.excluded.map(|why| match why.as_slice() {
            b"otherfs" | b"othfs" => Skip::OneFileSystem,
            b"kernfs" => Skip::Kernel,
            b"frmlink" => Skip::Firmlinked,
            _ => Skip::Excluded,
        });
        let kind = if array {
            Kind::Dir
        } else if let Some(mode) = self.mode {
            kind_of_mode(mode)
        } else if skipped.is_some() {
            // The mounts ncdu leaves out are directories; what a pattern
            // excludes it never stats, and that is left out below.
            Kind::Dir
        } else if self.notreg {
            Kind::Unknown
        } else {
            Kind::File
        };
        // A walk leaves out an excluded file and lists an excluded directory
        // it did not enter: ncdu's pattern-excluded entry is a directory
        // only where its mode says so.
        let left_out = !array
            && skipped == Some(Skip::Excluded)
            && self.mode.is_none_or(|mode| kind_of_mode(mode) != Kind::Dir);
        // gdu's directory sizes hold everything under it: the roll-up adds
        // the totals up again from the entries.
        let (apparent, allocated) = if format == ImportFormat::Gdu && kind == Kind::Dir {
            (0, 0)
        } else {
            (self.asize, self.dsize)
        };
        let mtime_ns = self
            .mtime
            .map_or(Some(0), |secs| secs.checked_mul(1_000_000_000))
            .ok_or_else(|| invalid("an mtime is out of range"))?;
        let nlink = match (self.ino, self.nlink) {
            (Some(_), Some(n)) => u32::try_from(n).unwrap_or(u32::MAX),
            // ncdu 1 marks a hard link without its count.
            (Some(_), None) if self.hlnkc => 2,
            _ => 1,
        };
        Ok(Item {
            name,
            kind,
            apparent,
            allocated,
            mtime_ns,
            dev: self.dev,
            ino: self.ino.filter(|_| nlink > 1),
            nlink,
            unreadable: self.read_error,
            skipped: skipped.filter(|_| kind == Kind::Dir),
            left_out,
        })
    }
}

/// A number, as JSON writes one.
enum Number {
    Unsigned(u64),
    Negative(i64),
    /// With a fraction or an exponent, which no field here takes.
    Other,
}

/// The pull parser.
struct Parser<R> {
    input: R,
    /// Bytes consumed, for error messages.
    pos: u64,
}

impl<R: BufRead> Parser<R> {
    /// An [`CoreError::ImportInvalid`] at the current byte.
    fn invalid(&self, what: &str) -> CoreError {
        CoreError::ImportInvalid(format!("{what}, at byte {}", self.pos))
    }

    fn peek(&mut self) -> Result<Option<u8>, CoreError> {
        Ok(self
            .input
            .fill_buf()
            .map_err(|e| read_error(&e))?
            .first()
            .copied())
    }

    fn bump(&mut self) {
        self.input.consume(1);
        self.pos += 1;
    }

    /// The next byte, consumed.
    fn next(&mut self) -> Result<u8, CoreError> {
        let b = self
            .peek()?
            .ok_or_else(|| self.invalid("the file ends early"))?;
        self.bump();
        Ok(b)
    }

    /// The next byte after white space, not consumed; `None` at the end.
    fn white(&mut self) -> Result<Option<u8>, CoreError> {
        while let Some(b) = self.peek()? {
            if !matches!(b, b' ' | b'\t' | b'\n' | b'\r') {
                return Ok(Some(b));
            }
            self.bump();
        }
        Ok(None)
    }

    /// `b`, after white space.
    fn expect(&mut self, b: u8) -> Result<(), CoreError> {
        match self.white()? {
            Some(got) if got == b => {
                self.bump();
                Ok(())
            }
            Some(got) => Err(self.invalid(&format!(
                "{:?} where {:?} belongs",
                char::from(got),
                char::from(b)
            ))),
            None => Err(self.invalid("the file ends early")),
        }
    }

    /// The metadata object: `progname` and `timestamp` kept.
    fn metadata(&mut self) -> Result<Metadata, CoreError> {
        let mut meta = Metadata::default();
        self.object(|p, key| {
            match key {
                b"progname" => meta.progname = Some(p.string(true)?),
                b"timestamp" => meta.timestamp = Some(p.signed()?),
                _ => p.skip()?,
            }
            Ok(())
        })?;
        Ok(meta)
    }

    /// An info object.
    fn info(&mut self) -> Result<Info, CoreError> {
        let mut info = Info::default();
        self.object(|p, key| {
            match key {
                b"name" => info.name = Some(p.string(true)?),
                b"asize" => info.asize = p.unsigned()?,
                b"dsize" => info.dsize = p.unsigned()?,
                b"dev" => info.dev = Some(p.unsigned()?),
                b"ino" => info.ino = Some(p.unsigned()?),
                b"nlink" => info.nlink = Some(p.unsigned()?),
                b"mode" => info.mode = Some(p.unsigned()?),
                b"mtime" => info.mtime = Some(p.signed()?),
                b"hlnkc" => info.hlnkc = p.boolean()?,
                b"read_error" => info.read_error = p.boolean()?,
                b"notreg" => info.notreg = p.boolean()?,
                b"excluded" => info.excluded = Some(p.string(true)?),
                _ => p.skip()?,
            }
            Ok(())
        })?;
        Ok(info)
    }

    /// An object, each member's value read by `member` given its key.
    fn object(
        &mut self,
        mut member: impl FnMut(&mut Self, &[u8]) -> Result<(), CoreError>,
    ) -> Result<(), CoreError> {
        self.expect(b'{')?;
        if self.white()? == Some(b'}') {
            self.bump();
            return Ok(());
        }
        loop {
            if self.white()? != Some(b'"') {
                return Err(self.invalid("an object's key is not a string"));
            }
            let key = self.string(true)?;
            self.expect(b':')?;
            self.white()?;
            member(self, &key)?;
            match self.white()? {
                Some(b',') => self.bump(),
                Some(b'}') => {
                    self.bump();
                    return Ok(());
                }
                _ => return Err(self.invalid("an object's members go on without a comma")),
            }
        }
    }

    /// A string's bytes, decoded; `keep` false reads past one. A name is
    /// the bytes between the quotes: escapes decoded, other bytes as they
    /// are, UTF-8 or not.
    fn string(&mut self, keep: bool) -> Result<Vec<u8>, CoreError> {
        if self.white()? != Some(b'"') {
            return Err(self.invalid("a string belongs here"));
        }
        self.bump();
        let mut out = Vec::new();
        loop {
            match self.next()? {
                b'"' => return Ok(out),
                b'\\' => match self.next()? {
                    b'u' => self.unicode(&mut out, keep)?,
                    other => {
                        let decoded = self.escape(other)?;
                        if keep {
                            out.extend_from_slice(decoded);
                        }
                    }
                },
                b => {
                    if keep {
                        out.push(b);
                    }
                }
            }
            if out.len() > STRING_MAX {
                return Err(self.invalid("a string is longer than 1 MiB"));
            }
        }
    }

    /// The bytes a one-letter escape stands for.
    fn escape(&self, letter: u8) -> Result<&'static [u8], CoreError> {
        Ok(match letter {
            b'"' => b"\"",
            b'\\' => b"\\",
            b'/' => b"/",
            b'b' => b"\x08",
            b'f' => b"\x0c",
            b'n' => b"\n",
            b'r' => b"\r",
            b't' => b"\t",
            other => {
                return Err(self.invalid(&format!(
                    "\\{} is not an escape",
                    char::from(other).escape_default()
                )));
            }
        })
    }

    /// Appends the character of the `\u` escape just begun to `out`, with
    /// the low surrogate's escape that follows a high one. A surrogate
    /// without its other half is U+FFFD, and an escape after a lone high
    /// surrogate is read on its own.
    fn unicode(&mut self, out: &mut Vec<u8>, keep: bool) -> Result<(), CoreError> {
        let mut push = |c: char| {
            if keep {
                let mut utf8 = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut utf8).as_bytes());
            }
        };
        let mut unit = self.hex4()?;
        loop {
            if !(0xd800..0xdc00).contains(&unit) {
                push(char::from_u32(unit).unwrap_or(char::REPLACEMENT_CHARACTER));
                return Ok(());
            }
            // A high surrogate: its low half follows as another escape.
            if self.peek()? != Some(b'\\') {
                push(char::REPLACEMENT_CHARACTER);
                return Ok(());
            }
            self.bump();
            let letter = self.next()?;
            if letter != b'u' {
                push(char::REPLACEMENT_CHARACTER);
                let decoded = self.escape(letter)?;
                if keep {
                    out.extend_from_slice(decoded);
                }
                return Ok(());
            }
            let low = self.hex4()?;
            if (0xdc00..0xe000).contains(&low) {
                let c = 0x10000 + ((unit - 0xd800) << 10) + (low - 0xdc00);
                push(char::from_u32(c).unwrap_or(char::REPLACEMENT_CHARACTER));
                return Ok(());
            }
            push(char::REPLACEMENT_CHARACTER);
            unit = low;
        }
    }

    fn hex4(&mut self) -> Result<u32, CoreError> {
        let mut value = 0;
        for _ in 0..4 {
            let b = self.next()?;
            let digit = char::from(b)
                .to_digit(16)
                .ok_or_else(|| self.invalid("a \\u escape needs four hex digits"))?;
            value = value * 16 + digit;
        }
        Ok(value)
    }

    /// A number.
    fn number(&mut self) -> Result<Number, CoreError> {
        let negative = self.white()? == Some(b'-');
        if negative {
            self.bump();
        }
        let mut value: u64 = 0;
        let mut digits = 0;
        let mut overflow = false;
        while let Some(b @ b'0'..=b'9') = self.peek()? {
            self.bump();
            digits += 1;
            match value
                .checked_mul(10)
                .and_then(|v| v.checked_add(u64::from(b - b'0')))
            {
                Some(v) => value = v,
                None => overflow = true,
            }
        }
        if digits == 0 {
            return Err(self.invalid("a number belongs here"));
        }
        let mut other = overflow;
        while let Some(b'.' | b'e' | b'E' | b'+' | b'-' | b'0'..=b'9') = self.peek()? {
            self.bump();
            other = true;
        }
        Ok(match (other, negative) {
            (true, _) => Number::Other,
            (false, false) => Number::Unsigned(value),
            (false, true) => match i64::try_from(value) {
                Ok(v) => Number::Negative(-v),
                Err(_) => Number::Other,
            },
        })
    }

    /// A whole number of 0 or more.
    fn unsigned(&mut self) -> Result<u64, CoreError> {
        match self.number()? {
            Number::Unsigned(v) => Ok(v),
            _ => Err(self.invalid("a whole number of 0 or more belongs here")),
        }
    }

    /// A whole number.
    fn signed(&mut self) -> Result<i64, CoreError> {
        match self.number()? {
            Number::Unsigned(v) => {
                i64::try_from(v).map_err(|_| self.invalid("a number is out of range"))
            }
            Number::Negative(v) => Ok(v),
            Number::Other => Err(self.invalid("a whole number belongs here")),
        }
    }

    fn boolean(&mut self) -> Result<bool, CoreError> {
        match self.white()? {
            Some(b't') => self.word(b"true").map(|()| true),
            Some(b'f') => self.word(b"false").map(|()| false),
            _ => Err(self.invalid("true or false belongs here")),
        }
    }

    fn word(&mut self, word: &[u8]) -> Result<(), CoreError> {
        for &want in word {
            if self.next()? != want {
                return Err(self.invalid("an unknown word"));
            }
        }
        Ok(())
    }

    /// Reads past one value of any kind, nested as deep as
    /// [`SKIP_DEPTH_MAX`].
    fn skip(&mut self) -> Result<(), CoreError> {
        // The closing brackets the open containers wait for.
        let mut closers: Vec<u8> = Vec::new();
        loop {
            match self.white()? {
                Some(b'"') => {
                    self.string(false)?;
                }
                Some(b'[' | b'{') => {
                    let open = self.next()?;
                    if closers.len() == SKIP_DEPTH_MAX {
                        return Err(self.invalid("a value nests too deep"));
                    }
                    closers.push(if open == b'[' { b']' } else { b'}' });
                }
                Some(close @ (b']' | b'}')) => {
                    if closers.pop() != Some(close) {
                        return Err(self.invalid("a bracket closes what it did not open"));
                    }
                    self.bump();
                }
                Some(b',' | b':') if !closers.is_empty() => self.bump(),
                Some(b't') => self.word(b"true")?,
                Some(b'f') => self.word(b"false")?,
                Some(b'n') => self.word(b"null")?,
                Some(b'-' | b'0'..=b'9') => {
                    self.number()?;
                }
                Some(_) => return Err(self.invalid("a value belongs here")),
                None => return Err(self.invalid("the file ends early")),
            }
            if closers.is_empty() {
                return Ok(());
            }
        }
    }
}
