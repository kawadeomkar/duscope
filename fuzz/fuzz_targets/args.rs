//! P2-TEST-01: the argument parsers of the engine, on arbitrary text: the
//! first byte picks the parser, the rest is its input. Whatever the text,
//! each answers or refuses it with its own error, never panics, and what it
//! accepts holds what its grammar says:
//!
//! - `--scan`: latest, an id prefix (4 to 12 lowercase hex digits), or a
//!   path (holding `/` or `.`);
//! - `--older-than` and `--max-age`: a whole number of hours, not negative;
//! - `--since` and `--test-now`: a date;
//! - `--cursor`: one that encodes back to itself;
//! - a scan id: the text itself;
//! - a handle: never one this key signed;
//! - `config.toml`, a classes table and a caches table.

#![no_main]

use std::path::Path;

use duscope_core::CoreError;
use duscope_core::config::Config;
use duscope_core::handle::{Handle, Key};
use duscope_core::index::ScanId;
use duscope_core::query::timegrammar::{parse_age, parse_date};
use duscope_core::query::{Cursor, ScanSelector};
use duscope_core::rules::{Caches, Classes};
use libfuzzer_sys::fuzz_target;

/// Nanoseconds per hour.
const HOUR_NS: i64 = 3_600_000_000_000;

fuzz_target!(|data: &[u8]| {
    let Some((&which, rest)) = data.split_first() else {
        return;
    };
    let text = String::from_utf8_lossy(rest);
    let text = text.as_ref();
    match which % 9 {
        0 => match ScanSelector::parse(text) {
            Ok(ScanSelector::Latest) => assert_eq!(text, "latest"),
            Ok(ScanSelector::Prefix(prefix)) => {
                assert_eq!(prefix, text);
                assert!((4..=12).contains(&prefix.len()));
                assert!(prefix.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')));
            }
            Ok(ScanSelector::File(path)) => {
                assert!(text.contains('/') || text.contains('.'));
                assert_eq!(path.as_os_str(), text);
            }
            Err(CoreError::Selector(_)) => {}
            Err(other) => panic!("not a selector refusal: {other:?}"),
        },
        1 => {
            if let Ok(ns) = parse_age(text) {
                assert!(ns >= 0 && ns % HOUR_NS == 0, "{text:?} -> {ns}");
            }
        }
        2 => {
            let _ = parse_date(text);
        }
        3 => match Cursor::decode(text) {
            Ok(cursor) => assert_eq!(Cursor::decode(&cursor.encode()).unwrap(), cursor),
            Err(CoreError::InvalidCursor(_)) => {}
            Err(other) => panic!("not a cursor refusal: {other:?}"),
        },
        4 => {
            if let Some(id) = ScanId::parse(text) {
                assert_eq!(id.as_str(), text);
            }
        }
        5 => match Handle::verify(&Key::from_bytes([7; 32]), text, 0) {
            Ok(payload) => panic!("a handle forged: {payload:?}"),
            Err(CoreError::HandleInvalid(_) | CoreError::HandleExpired) => {}
            Err(other) => panic!("not a handle refusal: {other:?}"),
        },
        6 => {
            let _ = Config::parse(text, Path::new("/etc/duscope/config.toml"));
        }
        7 => {
            let _ = Classes::parse(text, "classes.toml");
        }
        _ => {
            let _ = Caches::parse(text, "caches.toml");
        }
    }
});
