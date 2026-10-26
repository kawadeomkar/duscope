//! The manifest file as written: `manifest.toml`, parsed with serde. The
//! [`crate::plan`] module expands it into the exact list of entries.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::{Error, Result};

/// The manifest committed beside the generator, compiled in so the library
/// and the binary always agree on it.
pub const EMBEDDED: &str = include_str!("../manifest.toml");

/// The whole manifest.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// Manifest format; this generator reads format 1.
    pub format: u32,
    /// The expected manifest hash. A test compares it with the hash of the
    /// expanded manifest, so a change to the tree is always deliberate.
    pub hash: String,
    /// The fixed instant every mtime and atime is relative to, RFC 3339 UTC.
    pub instant: String,
    /// Modes and age for entries that do not set their own.
    pub defaults: Defaults,
    /// Names for generated directories and files.
    pub words: Vec<String>,
    /// Extension pools by duscope class, for generated files.
    pub extensions: BTreeMap<String, Vec<String>>,
    /// The named cases, each a set of explicit entries under one root.
    #[serde(rename = "case")]
    pub cases: Vec<CaseSpec>,
    /// Seeded random subtrees.
    #[serde(default, rename = "generate")]
    pub generators: Vec<GenerateSpec>,
}

/// Defaults for entries that do not set their own.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Defaults {
    /// Permission bits of directories.
    pub dir_mode: u32,
    /// Permission bits of files, FIFOs and sockets.
    pub file_mode: u32,
    /// Age (instant minus mtime) of entries and implicit parent directories.
    pub age: String,
}

/// One named case: a root directory and the entries under it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaseSpec {
    /// Stable name tests look the case up by (kebab-case).
    pub name: String,
    /// What the case exercises and which packages rely on it.
    pub about: String,
    /// The case's directory, relative to the tree root.
    pub root: String,
    /// The entries, with paths relative to `root`.
    #[serde(default)]
    pub entries: Vec<EntrySpec>,
}

/// A size: a byte count, or a string with an IEC suffix (`64K`, `2M`).
#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum SizeSpec {
    /// Bytes.
    Bytes(u64),
    /// `<integer>[K|M|G]`, powers of 1,024.
    Text(String),
}

impl SizeSpec {
    /// The size in bytes.
    pub fn bytes(&self) -> Result<u64> {
        match self {
            SizeSpec::Bytes(n) => Ok(*n),
            SizeSpec::Text(text) => parse_size(text),
        }
    }
}

/// One explicit entry. Exactly one of the kind fields (`dir`, `file`,
/// `text`, `sparse`, `compressed`, `hardlink`, `clone`, `symlink`, `fifo`,
/// `socket`, `many`, `chain`) is set.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntrySpec {
    /// Path relative to the case root; `.` is the root itself. `%XX` escapes
    /// a byte, so names that are not valid UTF-8 can be written.
    pub path: String,
    /// A directory.
    pub dir: Option<bool>,
    /// A regular file of this size with generated content.
    pub file: Option<SizeSpec>,
    /// A regular file with exactly this content.
    pub text: Option<String>,
    /// A sparse file of this apparent size.
    pub sparse: Option<SizeSpec>,
    /// With `sparse`: the written extents, `"<offset>+<length>"` each.
    pub data: Option<Vec<String>>,
    /// A file of this size stored with filesystem compression.
    pub compressed: Option<SizeSpec>,
    /// A hard link to this file (relative to the case root, or to the tree
    /// root when it starts with `/`).
    pub hardlink: Option<String>,
    /// A clone (`clonefile`, reflink) of this file, resolved like `hardlink`.
    pub clone: Option<String>,
    /// A symbolic link with this target, stored verbatim (`%XX` escapes).
    pub symlink: Option<String>,
    /// A FIFO.
    pub fifo: Option<bool>,
    /// A Unix-domain socket.
    pub socket: Option<bool>,
    /// A directory holding this many empty files named `<prefix><index>`.
    pub many: Option<u32>,
    /// With `many`: the name prefix.
    pub prefix: Option<String>,
    /// With `many`: the zero-padded width of the index.
    pub width: Option<u32>,
    /// A directory holding this many nested directories named `name`.
    pub chain: Option<u32>,
    /// With `chain`: the name of every nested directory.
    pub name: Option<String>,
    /// With `chain`: the name of the file at the bottom.
    pub leaf: Option<String>,
    /// With `chain`: the size of that file.
    pub leaf_size: Option<SizeSpec>,
    /// Permission bits (defaults from `[defaults]`).
    pub mode: Option<u32>,
    /// Permission bits applied only while a test environment holds the tree
    /// (unreadable directories); `mode` is kept at rest.
    pub test_mode: Option<u32>,
    /// Instant minus mtime (default `[defaults] age`).
    pub age: Option<String>,
    /// Instant minus atime (default: halfway between mtime and the instant,
    /// at most a year before it; half the age after a future mtime).
    pub atime: Option<String>,
    /// Nanoseconds added to the mtime.
    pub nanos: Option<u32>,
}

/// A seeded random subtree under `root`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenerateSpec {
    /// The case the generated entries belong to.
    pub case: String,
    /// Where the subtree is grown, relative to the tree root.
    pub root: String,
    /// Seed of the generator.
    pub seed: u64,
    /// Directories to create below `root`.
    pub dirs: u32,
    /// Files to distribute over `root` and those directories.
    pub files: u32,
    /// Deepest directory level below `root`.
    pub max_depth: u32,
    /// Largest file size.
    pub size_max: SizeSpec,
    /// Size skew: sizes are `size_max · u^size_skew`.
    pub size_skew: u32,
    /// Percentage of files that are empty.
    pub empty_pct: u32,
    /// Youngest age.
    pub age_min: String,
    /// Oldest age.
    pub age_max: String,
    /// Age skew towards `age_min`, like `size_skew`.
    pub age_skew: u32,
    /// Relative weights of the extension classes drawn from.
    pub classes: BTreeMap<String, u32>,
}

impl Manifest {
    /// Parses manifest text.
    pub fn parse(text: &str) -> Result<Manifest> {
        toml::from_str(text).map_err(|e| Error::new(format!("manifest: {e}")))
    }

    /// The manifest compiled into this generator.
    pub fn embedded() -> Result<Manifest> {
        Manifest::parse(EMBEDDED)
    }
}

/// Parses `<integer>[K|M|G]` (powers of 1,024).
pub fn parse_size(text: &str) -> Result<u64> {
    let bad = || Error::new(format!("size {text:?} is not <integer>[K|M|G]"));
    let (digits, scale) = match text.as_bytes().last() {
        Some(b'K') => (&text[..text.len() - 1], 1u64 << 10),
        Some(b'M') => (&text[..text.len() - 1], 1 << 20),
        Some(b'G') => (&text[..text.len() - 1], 1 << 30),
        _ => (text, 1),
    };
    if digits.is_empty() || !digits.bytes().all(|c| c.is_ascii_digit()) {
        return Err(bad());
    }
    let n: u64 = digits.parse().map_err(|_| bad())?;
    n.checked_mul(scale).ok_or_else(bad)
}

/// Decodes `%XX` escapes into raw bytes; every other character stands for
/// its UTF-8 bytes. A lone `%` is an error, so `%25` writes a percent sign.
pub fn decode(text: &str) -> Result<Vec<u8>> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            // Two hex digits exactly: from_str_radix alone would take "+F".
            let hex = bytes
                .get(i + 1..i + 3)
                .filter(|h| h.iter().all(u8::is_ascii_hexdigit))
                .and_then(|h| std::str::from_utf8(h).ok())
                .and_then(|h| u8::from_str_radix(h, 16).ok())
                .ok_or_else(|| Error::new(format!("bad %-escape in {text:?}")))?;
            out.push(hex);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Ok(out)
}

/// The inverse of [`decode`] for display and for the expanded manifest:
/// printable ASCII stays, everything else (and `%`) becomes `%XX`.
pub fn encode(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(bytes.len());
    for &b in bytes {
        if b.is_ascii_graphic() && b != b'%' {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn p0_test_01_sizes_parse() {
        assert_eq!(parse_size("0").unwrap(), 0);
        assert_eq!(parse_size("43").unwrap(), 43);
        assert_eq!(parse_size("64K").unwrap(), 65_536);
        assert_eq!(parse_size("2M").unwrap(), 2_097_152);
        assert_eq!(parse_size("1G").unwrap(), 1_073_741_824);
        for bad in ["", "K", "1.5M", "64k", "-1", "1KB"] {
            assert!(parse_size(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn p0_test_01_escapes_round_trip() {
        assert_eq!(decode("caf%E9").unwrap(), b"caf\xe9");
        assert_eq!(decode("100%25").unwrap(), b"100%");
        assert_eq!(decode("caf\u{e9}").unwrap(), "caf\u{e9}".as_bytes());
        assert!(decode("50%").is_err());
        assert!(decode("%G0").is_err());
        let raw = b"a b\tc\n%\xff\xc3\xa9/x";
        assert_eq!(encode(raw), "a%20b%09c%0A%25%FF%C3%A9/x");
        assert_eq!(decode(&encode(raw)).unwrap(), raw);
    }

    #[test]
    fn p0_test_01_escapes_take_two_hex_digits() {
        for bad in ["%+F", "%-1", "% F", "%F", "%"] {
            assert!(decode(bad).is_err(), "{bad:?} decoded");
        }
    }

    #[test]
    fn p0_test_01_embedded_manifest_parses() {
        let manifest = Manifest::embedded().unwrap();
        assert_eq!(manifest.format, 1);
        assert!(!manifest.cases.is_empty());
    }
}
