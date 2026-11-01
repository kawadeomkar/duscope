//! Keyset cursors: where the next page of a list starts.
//!
//! A cursor is base64url (no padding) of the JSON
//! `{"scan": <id>, "after": [<value>, <id>], "filters": <hash>, "seen": <n>}`:
//! the scan it pages, the sort value and id of the last row returned, a hash
//! of the query (verb, path, depth, sort, filters), and how many rows came
//! before the next page. A saved scan never changes, so the rows after that
//! key are the same on every read of it. A cursor whose scan or query
//! differs from the command's is refused: its key means nothing there.

use serde::{Deserialize, Serialize};

use super::{Node, SizeKind, SortKey};
use crate::CoreError;

/// Where a page starts: after the row with sort value `value` and id `id`,
/// `seen` rows into the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct After {
    /// The last row's sort value: its size, file count or newest time
    /// (Unix nanoseconds), or 0 under `--sort path`.
    pub value: i64,
    /// The last row's id in the view the query read.
    pub id: u32,
    /// The rows returned before the next page.
    pub seen: u64,
}

impl After {
    /// The key of the row `node`, `seen` rows in, under `key` in `size`.
    pub fn of(node: &Node, key: SortKey, size: SizeKind, seen: u64) -> After {
        let value = match key {
            SortKey::Size => i64::try_from(node.size(size)).unwrap_or(i64::MAX),
            SortKey::Count => i64::try_from(node.file_count).unwrap_or(i64::MAX),
            SortKey::Mtime => node.max_mtime_ns,
            SortKey::Path => 0,
        };
        After {
            value,
            id: node.id,
            seen,
        }
    }
}

/// A page's cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cursor {
    /// The id of the saved scan it pages.
    pub scan: String,
    /// The key the next page starts after.
    pub after: After,
    /// [`query_hash`] of the query it pages.
    pub filters: String,
}

/// The cursor's JSON.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    scan: String,
    after: (i64, u32),
    filters: String,
    seen: u64,
}

impl Cursor {
    /// The cursor as the envelope's `next_cursor` carries it.
    pub fn encode(&self) -> String {
        let wire = Wire {
            scan: self.scan.clone(),
            after: (self.after.value, self.after.id),
            filters: self.filters.clone(),
            seen: self.after.seen,
        };
        // A struct of strings and integers always serialises.
        let json = serde_json::to_vec(&wire).unwrap_or_default();
        base64url(&json)
    }

    /// The cursor `text` encodes; [`CoreError::InvalidCursor`] when it is
    /// not one.
    pub fn decode(text: &str) -> Result<Cursor, CoreError> {
        let invalid = |why: &str| CoreError::InvalidCursor(format!("--cursor {why}"));
        let json = from_base64url(text.trim()).ok_or_else(|| invalid("is not base64url"))?;
        let wire: Wire =
            serde_json::from_slice(&json).map_err(|_| invalid("is not a duscope cursor"))?;
        Ok(Cursor {
            scan: wire.scan,
            after: After {
                value: wire.after.0,
                id: wire.after.1,
                seen: wire.seen,
            },
            filters: wire.filters,
        })
    }
}

/// A short hash of a query's parts (FNV-1a, 64 bits, in hex), which a
/// cursor carries and the next command compares: the parts are separated,
/// so `["ab", "c"]` and `["a", "bc"]` differ.
pub fn query_hash<'a>(parts: impl IntoIterator<Item = &'a str>) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |byte: u8| {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    };
    for part in parts {
        for &byte in part.as_bytes() {
            eat(byte);
        }
        eat(0);
    }
    format!("{hash:016x}")
}

/// The base64url alphabet (RFC 4648 §5).
const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// `bytes` in base64url, without padding.
fn base64url(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..=chunk.len() {
            out.push(char::from(ALPHABET[(n >> (18 - 6 * i)) as usize & 63]));
        }
    }
    out
}

/// The bytes of base64url `text`, padding allowed; `None` when it is not
/// base64url.
fn from_base64url(text: &str) -> Option<Vec<u8>> {
    let text = text.trim_end_matches('=');
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let mut acc: u32 = 0;
    let mut bits = 0;
    for byte in text.bytes() {
        let value = ALPHABET.iter().position(|&a| a == byte)?;
        acc = (acc << 6) | u32::try_from(value).ok()?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(u8::try_from((acc >> bits) & 0xff).ok()?);
        }
    }
    // A last character that carries only padding bits is not base64.
    (bits < 6).then_some(out)
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::{After, Cursor, base64url, from_base64url, query_hash};

    proptest! {
        #[test]
        fn p2_qry_02_base64url_round_trips(bytes in proptest::collection::vec(any::<u8>(), 0..64)) {
            let text = base64url(&bytes);
            prop_assert!(text.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'));
            prop_assert_eq!(from_base64url(&text), Some(bytes));
        }

        #[test]
        fn p2_qry_02_a_cursor_round_trips(
            scan in "[0-9a-f]{12}",
            value in any::<i64>(),
            id in any::<u32>(),
            seen in any::<u64>(),
            filters in "[0-9a-f]{16}",
        ) {
            let cursor = Cursor { scan, after: After { value, id, seen }, filters };
            prop_assert_eq!(Cursor::decode(&cursor.encode()).unwrap(), cursor);
        }
    }

    #[test]
    fn p2_qry_02_what_is_not_a_cursor_is_refused() {
        for text in ["", "!!!", "e30", "bm90IGpzb24", "eyJzY2FuIjoxfQ"] {
            assert!(Cursor::decode(text).is_err(), "{text:?}");
        }
        assert_ne!(query_hash(["ab", "c"]), query_hash(["a", "bc"]));
        assert_eq!(query_hash(["top"]).len(), 16);
    }
}
