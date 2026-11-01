//! `--max-chars`: a JSON envelope longer than the limit (24,000 characters
//! by default, what an agent's tool reads inline) goes to a file under the
//! index directory's `out/`, 0600, and stdout gets a stub naming the file
//! with the envelope's first five rows. `scans --prune` removes such files
//! after a week.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use duscope_core::envelope::{SCHEMA_VERSION, Spill, SpilledEnvelope, Tool, Warning};
use duscope_core::query::clock;
use serde::Serialize;

use crate::verbs::{Exit, Failure};

/// `--max-chars` when the flag does not set it.
pub const DEFAULT_MAX_CHARS: usize = 24_000;

/// Rows a stub previews.
const PREVIEW: usize = 5;

/// Writes `json`, the whole envelope, to a new file in `dir` named after
/// the scan (`scan`, or `fresh` for one not saved), the verb and now, and
/// returns it.
pub fn write(dir: &Path, scan: Option<&str>, verb: &str, json: &[u8]) -> Result<PathBuf, Failure> {
    let failed = |e: std::io::Error| {
        Failure::new(
            Exit::Runtime,
            "io_error",
            format!("cannot write the envelope to {}: {e}", dir.display()),
        )
        .hint("Rerun the command with -o FILE, or a larger --max-chars")
    };
    fs::create_dir_all(dir).map_err(failed)?;
    // The directory holds what scans print: the owner's only.
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).map_err(failed)?;
    let stamp = compact(&duscope_core::query::timegrammar::iso8601(clock::now()));
    let verb = verb.replace(' ', "-");
    let scan = scan.unwrap_or("fresh");
    let mut n = 1;
    loop {
        let name = if n == 1 {
            format!("{scan}-{verb}-{stamp}.json")
        } else {
            format!("{scan}-{verb}-{stamp}-{n}.json")
        };
        let path = dir.join(name);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(mut file) => {
                file.write_all(json).map_err(failed)?;
                file.write_all(b"\n").map_err(failed)?;
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && n < 100 => n += 1,
            Err(e) => return Err(failed(e)),
        }
    }
}

/// An ISO-8601 time as a file name takes it: `20261031T235959Z`.
fn compact(iso: &str) -> String {
    iso.chars().filter(char::is_ascii_alphanumeric).collect()
}

/// The stub for an envelope of `rows` rows, `chars` characters long,
/// written to `path` (`bytes` long): the first rows of `entries`, the
/// envelope's `warnings` and `spilled`.
pub fn stub<T: Serialize>(
    entries: Vec<T>,
    warnings: &[Warning],
    path: &Path,
    rows: u64,
    bytes: u64,
    chars: usize,
    max: usize,
) -> SpilledEnvelope<T> {
    let mut warnings = warnings.to_vec();
    warnings.push(Warning {
        code: "spilled",
        message: format!(
            "the envelope is {chars} characters, more than --max-chars {max}; it is in {}",
            path.display()
        ),
        count: Some(rows),
        hint: Some(
            "Read the file, or rerun with --fields, a smaller --limit or -o FILE".to_owned(),
        ),
    });
    SpilledEnvelope {
        ok: true,
        schema_version: SCHEMA_VERSION,
        tool: Tool::CURRENT,
        spilled: Spill {
            path: path.display().to_string(),
            rows,
            bytes,
        },
        preview: entries.into_iter().take(PREVIEW).collect(),
        warnings,
    }
}
