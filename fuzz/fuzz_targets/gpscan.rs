//! P2-CLI-03: arbitrary bytes imported as GrandPerspective's `.gpscan`
//! (XML), gzipped or not. Whatever the input, the import reads a tree or
//! refuses the file (`ImportInvalid`), within the 1 GiB decompression cap;
//! never panics; and a tree it reads rolls up without overflowing a total.

#![no_main]

use duscope_core::CoreError;
use duscope_core::interchange::import::{ImportFormat, import};
use duscope_core::rollup::roll_up;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    match import(data, Some(ImportFormat::Gpscan)) {
        Ok(mut imported) => {
            roll_up(&mut imported.arena);
        }
        Err(CoreError::ImportInvalid(_)) => {}
        Err(other) => panic!("not a refusal: {other:?}"),
    }
});
