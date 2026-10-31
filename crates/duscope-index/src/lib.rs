//! The on-disk index: the bulk writer and scan ids, the code that needs
//! SQLite, apart from `duscope-core` so the bundled SQLite compiles beside it.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod scanid;
pub mod writer;

pub use scanid::{ScanId, random_bytes};
pub use writer::{FIRMLINK, IndexWriter, ScanMeta, index_path};
