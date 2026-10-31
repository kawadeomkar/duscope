//! Scan ids: the first 12 lowercase hex digits of SHA-256(root ‖
//! `started_at` ‖ 8 random bytes), which name the index file and which
//! `--scan` takes a prefix of.

use std::fmt::{self, Write as _};
use std::io::{self, Read};

use sha2::{Digest, Sha256};

/// The hex digits an id keeps.
const LEN: usize = 12;

/// A scan's id.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ScanId(String);

impl ScanId {
    /// The id of a scan of `root` (its bytes, symbolic links resolved) that
    /// started at `started_ns` (Unix nanoseconds), salted with `random`, so
    /// two scans of one root in the same nanosecond still differ.
    pub fn new(root: &[u8], started_ns: i64, random: [u8; 8]) -> ScanId {
        let mut hash = Sha256::new();
        hash.update(root);
        hash.update(started_ns.to_be_bytes());
        hash.update(random);
        let digest = hash.finalize();
        let mut id = String::with_capacity(LEN);
        for byte in digest.iter().take(LEN / 2) {
            let _ = write!(id, "{byte:02x}");
        }
        ScanId(id)
    }

    /// The id `text` spells: 12 lowercase hex digits; `None` for anything
    /// else.
    pub fn parse(text: &str) -> Option<ScanId> {
        let hex = |b: u8| b.is_ascii_digit() || (b'a'..=b'f').contains(&b);
        (text.len() == LEN && text.bytes().all(hex)).then(|| ScanId(text.to_owned()))
    }

    /// The id's hex digits.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ScanId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Eight bytes from the system's random source, `/dev/urandom` on Linux and
/// macOS alike.
pub fn random_bytes() -> io::Result<[u8; 8]> {
    let mut bytes = [0; 8];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes)
}
