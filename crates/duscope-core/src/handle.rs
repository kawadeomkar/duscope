//! Handles: what `explain --json` gives an agent, so that a later
//! `clean --from` acts only on the entry `explain` examined, as it was.
//!
//! A handle is base64url(payload ‖ HMAC-SHA256(key, payload)), without
//! padding. The payload names the path, the device and inode `explain`
//! found at it, its bytes on disk, the scan it answered from and an expiry
//! fifteen minutes on ([`LIFETIME_NS`]); laid out, version 1:
//!
//! ```text
//! 1 ‖ expires_at (i64, ns) ‖ dev ‖ ino ‖ allocated (u64) ‖
//!     scan_id's length (u8) ‖ scan_id ‖ path (the rest)
//! ```
//!
//! big-endian. The key is 32 random bytes in `handle.key` in the index
//! directory (`$XDG_CACHE_HOME/duscope`), created on first use with mode
//! 0600; deleting it invalidates every outstanding handle, and one that
//! others can read is refused, since whoever reads it can forge handles.
//! [`Handle::verify`] checks the MAC, in constant time, before it reads
//! the payload, then the expiry: [`CoreError::HandleInvalid`],
//! [`CoreError::HandleExpired`].

use std::fs::{self, DirBuilder, File};
use std::io::{ErrorKind, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

use crate::CoreError;
use crate::query::cursor::{base64url, from_base64url};

/// How long a handle holds: fifteen minutes, in nanoseconds.
pub const LIFETIME_NS: i64 = 15 * 60 * 1_000_000_000;

/// The key file's name, in the index directory.
pub const KEY_FILE: &str = "handle.key";

/// The payload's layout version.
const VERSION: u8 = 1;

/// The MAC's length.
const MAC_LEN: usize = 32;

/// The key handles are signed with.
#[derive(Clone)]
pub struct Key([u8; 32]);

impl std::fmt::Debug for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Key(…)")
    }
}

impl Key {
    /// The key in `dir`'s [`KEY_FILE`], made on first use: 32 random bytes,
    /// mode 0600, the directory made 0700 where it is missing. Two first
    /// uses at once settle on one key, as the file is linked into place
    /// only where none is.
    ///
    /// # Errors
    ///
    /// [`CoreError::HandleKey`] when the key cannot be read or made, is not
    /// 32 bytes, or others may read it.
    pub fn open(dir: &Path) -> Result<Key, CoreError> {
        let file = dir.join(KEY_FILE);
        let fail = |message: String| CoreError::HandleKey {
            file: file.display().to_string(),
            message,
        };
        match File::open(&file) {
            Ok(mut opened) => {
                let mode = opened
                    .metadata()
                    .map_err(|e| fail(e.to_string()))?
                    .permissions()
                    .mode();
                if mode & 0o077 != 0 {
                    return Err(fail(format!(
                        "others may read it (mode {:04o}), and whoever reads it can forge handles",
                        mode & 0o7777
                    )));
                }
                let mut key = [0; 32];
                let mut rest = Vec::new();
                opened
                    .read_exact(&mut key)
                    .and_then(|()| opened.read_to_end(&mut rest))
                    .map_err(|e| fail(format!("not a key of 32 bytes: {e}")))?;
                if !rest.is_empty() {
                    return Err(fail("not a key of 32 bytes".to_owned()));
                }
                Ok(Key(key))
            }
            Err(e) if e.kind() == ErrorKind::NotFound => {
                Key::create(dir, &file).map_err(|e| fail(e.to_string()))?;
                Key::open(dir)
            }
            Err(e) => Err(fail(e.to_string())),
        }
    }

    /// Writes a new key beside `file` and links it into place, unless a
    /// key got there first.
    fn create(dir: &Path, file: &Path) -> std::io::Result<()> {
        DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
        let mut key = [0; 32];
        File::open("/dev/urandom")?.read_exact(&mut key)?;
        let staged: PathBuf = dir.join(format!(".{KEY_FILE}.{}", std::process::id()));
        let written = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&staged)
            .and_then(|mut out| out.write_all(&key).and_then(|()| out.sync_all()))
            .and_then(|()| match fs::hard_link(&staged, file) {
                Err(e) if e.kind() == ErrorKind::AlreadyExists => Ok(()),
                other => other,
            });
        let removed = fs::remove_file(&staged);
        written.and(removed)
    }

    /// A key of these bytes, for tests.
    #[must_use]
    pub fn from_bytes(bytes: [u8; 32]) -> Key {
        Key(bytes)
    }

    fn mac(&self) -> Hmac<Sha256> {
        // HMAC takes a key of any length.
        <Hmac<Sha256> as KeyInit>::new_from_slice(&self.0)
            .unwrap_or_else(|_| unreachable!("HMAC keys have no length limit"))
    }
}

/// What a handle names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandlePayload {
    /// The path, absolute, as bytes.
    pub path: Vec<u8>,
    /// The device `explain` found at it.
    pub dev: u64,
    /// The inode.
    pub ino: u64,
    /// Its bytes on disk then: a directory's subtree.
    pub allocated: u64,
    /// The scan `explain` answered from; empty where it answered from a
    /// fresh `stat`.
    pub scan_id: String,
    /// When the handle stops holding, Unix nanoseconds.
    pub expires_at: i64,
}

/// Issuing and checking handles.
#[derive(Debug, Clone, Copy)]
pub struct Handle;

impl Handle {
    /// A handle for `payload`.
    #[must_use]
    pub fn issue(key: &Key, payload: &HandlePayload) -> String {
        let mut bytes = Vec::with_capacity(34 + payload.scan_id.len() + payload.path.len());
        bytes.push(VERSION);
        bytes.extend_from_slice(&payload.expires_at.to_be_bytes());
        bytes.extend_from_slice(&payload.dev.to_be_bytes());
        bytes.extend_from_slice(&payload.ino.to_be_bytes());
        bytes.extend_from_slice(&payload.allocated.to_be_bytes());
        let scan = payload.scan_id.as_bytes();
        let scan = &scan[..scan.len().min(usize::from(u8::MAX))];
        bytes.push(u8::try_from(scan.len()).unwrap_or(u8::MAX));
        bytes.extend_from_slice(scan);
        bytes.extend_from_slice(&payload.path);
        let mut mac = key.mac();
        mac.update(&bytes);
        bytes.extend_from_slice(&mac.finalize().into_bytes());
        base64url(&bytes)
    }

    /// What `token` names, if `key` signed it and it holds at `now`.
    ///
    /// # Errors
    ///
    /// [`CoreError::HandleInvalid`] for a token `key` did not sign, or that
    /// is not a handle; [`CoreError::HandleExpired`] for one past its
    /// expiry.
    pub fn verify(key: &Key, token: &str, now: i64) -> Result<HandlePayload, CoreError> {
        let invalid =
            || CoreError::HandleInvalid("the handle is not one this duscope issued".to_owned());
        let bytes = from_base64url(token).ok_or_else(invalid)?;
        let split = bytes.len().checked_sub(MAC_LEN).ok_or_else(invalid)?;
        let (payload, tag) = bytes.split_at(split);
        let mut mac = key.mac();
        mac.update(payload);
        mac.verify_slice(tag).map_err(|_| invalid())?;
        let decoded = decode(payload).ok_or_else(invalid)?;
        if now > decoded.expires_at {
            return Err(CoreError::HandleExpired);
        }
        Ok(decoded)
    }
}

/// The payload's fields, once the MAC has vouched for it.
fn decode(payload: &[u8]) -> Option<HandlePayload> {
    let (&version, rest) = payload.split_first()?;
    if version != VERSION {
        return None;
    }
    let word = |at: usize| -> Option<[u8; 8]> { rest.get(at..at + 8)?.try_into().ok() };
    let scan_len = usize::from(*rest.get(32)?);
    let scan = rest.get(33..33 + scan_len)?;
    Some(HandlePayload {
        expires_at: i64::from_be_bytes(word(0)?),
        dev: u64::from_be_bytes(word(8)?),
        ino: u64::from_be_bytes(word(16)?),
        allocated: u64::from_be_bytes(word(24)?),
        scan_id: String::from_utf8(scan.to_vec()).ok()?,
        path: rest.get(33 + scan_len..)?.to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use proptest::prelude::*;

    use super::{Handle, HandlePayload, KEY_FILE, Key, LIFETIME_NS};
    use crate::CoreError;
    use crate::query::cursor::{base64url, from_base64url};

    fn payload() -> HandlePayload {
        HandlePayload {
            path: b"/home/a/caf\xc3\xa9/\xffraw".to_vec(),
            dev: 66,
            ino: 1_234_567,
            allocated: 4096 * 1000,
            scan_id: "3f9a0c1d2e4b".to_owned(),
            expires_at: 1_800_000_000_000_000_000 + LIFETIME_NS,
        }
    }

    #[test]
    fn p2_core_01_a_handle_round_trips_and_expires() {
        let key = Key::from_bytes([7; 32]);
        let token = Handle::issue(&key, &payload());
        assert!(
            token
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        );
        let now = 1_800_000_000_000_000_000;
        assert_eq!(Handle::verify(&key, &token, now).unwrap(), payload());
        // Up to its expiry, then not.
        let last = now + LIFETIME_NS;
        assert!(Handle::verify(&key, &token, last).is_ok());
        assert!(matches!(
            Handle::verify(&key, &token, last + 1),
            Err(CoreError::HandleExpired)
        ));
        // Another key's handle, and what is no handle at all.
        let other = Key::from_bytes([8; 32]);
        for bad in [
            Handle::issue(&other, &payload()),
            String::new(),
            "!!".to_owned(),
            "AAAA".to_owned(),
        ] {
            assert!(matches!(
                Handle::verify(&key, &bad, now),
                Err(CoreError::HandleInvalid(_))
            ));
        }
    }

    proptest! {
        #[test]
        fn p2_core_01_tampering_any_byte_fails_verification(at in any::<prop::sample::Index>(), flip in 1u8..=255) {
            let key = Key::from_bytes([9; 32]);
            let token = Handle::issue(&key, &payload());
            let mut bytes = from_base64url(&token).unwrap();
            let i = at.index(bytes.len());
            bytes[i] ^= flip;
            let tampered = base64url(&bytes);
            prop_assert!(matches!(
                Handle::verify(&key, &tampered, 0),
                Err(CoreError::HandleInvalid(_))
            ));
        }
    }

    #[test]
    fn p2_core_01_the_key_is_made_once_private_and_checked() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("duscope");
        let key = Key::open(&home).unwrap();
        let file = home.join(KEY_FILE);
        let meta = std::fs::metadata(&file).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        assert_eq!(meta.len(), 32);
        assert_eq!(
            std::fs::metadata(&home).unwrap().permissions().mode() & 0o777,
            0o700
        );
        // The same key again: what one signed, the next opening verifies.
        let token = Handle::issue(&key, &payload());
        let again = Key::open(&home).unwrap();
        assert!(Handle::verify(&again, &token, 0).is_ok());
        // No staged file is left beside it.
        assert_eq!(std::fs::read_dir(&home).unwrap().count(), 1);
        // A key others may read is refused, as is one of the wrong length.
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        let err = Key::open(&home).unwrap_err().to_string();
        assert!(err.contains("0644"), "{err}");
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::write(&file, b"short").unwrap();
        assert!(matches!(Key::open(&home), Err(CoreError::HandleKey { .. })));
    }
}
