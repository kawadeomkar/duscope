//! `--max-chars`: a JSON envelope longer than the limit (24,000 characters
//! by default, what an agent's tool reads inline) goes to a file under the
//! index directory's `out/`, 0600, and stdout gets a stub naming the file
//! with the envelope's first five rows. An `out` that is a symbolic link is
//! refused, and `out` is given 0700 only when it is this user's own
//! directory. Each file is made through `out` opened once, from `/` without
//! following a link, so an `out` swapped for a link meanwhile never gets
//! one. `scans --prune` removes such files after a week.

use std::ffi::OsStr;
use std::fs::{self, DirBuilder, File};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use duscope_core::envelope::{SCHEMA_VERSION, Spill, SpilledEnvelope, Tool, Warning};
use duscope_core::index::ScanId;
use duscope_core::query::clock;
use duscope_walk::{CheckedDir, RemoveError};
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
    let out = open_out(dir)?;
    write_in(&out, dir, scan, verb, json)
}

/// The failure to write a spill into `dir`, for `e`.
fn failed(dir: &Path) -> impl Fn(std::io::Error) -> Failure + '_ {
    move |e| {
        Failure::new(
            Exit::Runtime,
            "io_error",
            format!("cannot write the envelope to {}: {e}", dir.display()),
        )
        .hint("Rerun the command with -o FILE, or a larger --max-chars")
    }
}

/// The index directory's `out` at `dir`, made where it is missing and
/// opened once, from `/` a component at a time without following a link
/// (`duscope_walk::CheckedDir`), once the index directory above it is
/// resolved: every spill is made through that descriptor, so an `out`
/// swapped for a link after this check is never reached.
fn open_out(dir: &Path) -> Result<CheckedDir, Failure> {
    let failed = failed(dir);
    // The directory holds what scans print: the owner's only, and so is each
    // directory made on the way to it (the index directory itself, when a
    // spill is the first thing written there).
    let made = DirBuilder::new().recursive(true).mode(0o700).create(dir);
    // A link in its place, even one to nothing, leads elsewhere: refused,
    // as `scans --prune` refuses it, before anything is written or changed.
    let found = match fs::symlink_metadata(dir) {
        Ok(found) => found,
        Err(e) => return Err(failed(made.err().unwrap_or(e))),
    };
    if found.file_type().is_symlink() {
        return Err(Failure::new(
            Exit::Runtime,
            "io_error",
            format!(
                "{} is a symbolic link, not the index directory's own folder of spills",
                dir.display()
            ),
        )
        .hint("Remove the link, or rerun the command with -o FILE or a larger --max-chars"));
    }
    made.map_err(&failed)?;
    owners_alone(dir, &found).map_err(&failed)?;
    let (above, name) = match (dir.parent(), dir.file_name()) {
        (Some(above), Some(name)) if !above.as_os_str().is_empty() => (above, name),
        (_, Some(name)) => (Path::new("."), name),
        (_, None) => {
            return Err(failed(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "it names no folder",
            )));
        }
    };
    let above = fs::canonicalize(above).map_err(&failed)?;
    CheckedDir::open(&above.join(name)).map_err(|e| {
        failed(match e {
            RemoveError::Changed { .. } => {
                std::io::Error::other("it was replaced while it was opened")
            }
            RemoveError::Io { source, .. } => source,
            other => std::io::Error::other(other.to_string()),
        })
    })
}

/// Writes `json` to a new file in `out`, the folder [`open_out`] opened at
/// `dir`, through its descriptor and only where nothing is at the name;
/// returns the file's path under `dir`.
fn write_in(
    out: &CheckedDir,
    dir: &Path,
    scan: Option<&str>,
    verb: &str,
    json: &[u8],
) -> Result<PathBuf, Failure> {
    let failed = failed(dir);
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
        match duscope_walk::create_checked(out, OsStr::new(&name)) {
            Ok(mut file) => {
                file.write_all(json).map_err(&failed)?;
                file.write_all(b"\n").map_err(&failed)?;
                return Ok(dir.join(name));
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && n < 100 => n += 1,
            Err(e) => return Err(failed(e)),
        }
    }
}

/// Gives `dir`, read as `found` without following a link, mode 0700
/// whatever the umask when it is a directory of this user's; one of
/// another user's is left as it is. The mode is set through a descriptor of
/// the directory opened, whose device and inode must be `found`'s, so a
/// link put in its place since is never followed to change what it leads
/// to.
fn owners_alone(dir: &Path, found: &fs::Metadata) -> std::io::Result<()> {
    if !found.is_dir() || found.uid() != duscope_walk::effective_uid() {
        return Ok(());
    }
    let opened = File::open(dir)?;
    let now = opened.metadata()?;
    if (now.dev(), now.ino()) != (found.dev(), found.ino()) {
        return Err(std::io::Error::other(
            "it was replaced while its mode was being set",
        ));
    }
    opened.set_permissions(fs::Permissions::from_mode(0o700))
}

/// Whether `name` is one [`write()`] gives a file, so that `scans --prune`
/// removes nothing else: `<scan>-<verb>-<stamp>.json`, or
/// `<scan>-<verb>-<stamp>-<n>.json` (`<n>` from 2 to 100) where that name
/// was taken; `<scan>` a scan id or `fresh`, `<verb>` lowercase letters,
/// digits and dashes, `<stamp>` the time as [`compact`] writes it,
/// `20261031T235959Z`.
pub fn is_spill_name(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".json") else {
        return false;
    };
    let stem = match stem.rsplit_once('-') {
        Some((before, n))
            if !n.starts_with('0') && n.parse::<u8>().is_ok_and(|n| (2..=100).contains(&n)) =>
        {
            before
        }
        _ => stem,
    };
    let Some((rest, stamp)) = stem.rsplit_once('-') else {
        return false;
    };
    let Some((scan, verb)) = rest.split_once('-') else {
        return false;
    };
    let stamped = match stamp.as_bytes() {
        [date @ .., b'T', h0, h1, m0, m1, s0, s1, b'Z'] if date.len() == 8 => date
            .iter()
            .chain([h0, h1, m0, m1, s0, s1])
            .all(u8::is_ascii_digit),
        _ => false,
    };
    stamped
        && (scan == "fresh" || ScanId::parse(scan).is_some())
        && !verb.is_empty()
        && verb
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::{MetadataExt, symlink};

    use duscope_testkit::TestEnv;

    use super::{is_spill_name, open_out, write, write_in};

    #[test]
    fn p5_sec_01_a_spill_is_made_in_the_out_it_opened_never_a_link_swapped_in() {
        let env = TestEnv::new();
        let dir = env.index_dir().join("out");
        let opened = open_out(&dir).unwrap();
        // `out` moved aside once opened, and in its place a link to a
        // folder elsewhere.
        let elsewhere = env.tmp_dir().join("elsewhere");
        fs::create_dir_all(&elsewhere).unwrap();
        let aside = env.index_dir().join("out.aside");
        fs::rename(&dir, &aside).unwrap();
        symlink(&elsewhere, &dir).unwrap();
        let path = write_in(&opened, &dir, Some("0123456789ab"), "top", b"{}").unwrap();
        // The file is in the folder opened, where it now is, 0600; nothing
        // is where the link leads.
        let name = path.file_name().unwrap();
        assert_eq!(path, dir.join(name));
        assert_eq!(fs::read(aside.join(name)).unwrap(), b"{}\n");
        assert_eq!(
            fs::metadata(aside.join(name)).unwrap().mode() & 0o777,
            0o600
        );
        assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0);
        // With the link in place already, the spill is refused.
        let failure = write(&dir, None, "top", b"{}").err().unwrap();
        assert_eq!(failure.code, "io_error");
        assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0);
    }

    #[test]
    fn p5_sec_01_a_spill_name_is_one_write_gives() {
        let env = TestEnv::new();
        let dir = env.tmp_dir().join("out");
        // Three in the same second: the second and third take a number.
        let mut written = Vec::new();
        for (scan, verb) in [
            (Some("0123456789ab"), "top"),
            (Some("0123456789ab"), "top"),
            (None, "scans --prune"),
            (Some("0123456789ab"), "by-type"),
            (Some("0123456789ab"), "by-type"),
            (Some("0123456789ab"), "by-type"),
        ] {
            written.push(write(&dir, scan, verb, b"{}").unwrap());
        }
        for path in &written {
            let name = path.file_name().unwrap().to_str().unwrap();
            assert!(is_spill_name(name), "{name}");
        }
        assert!(written[5].to_str().unwrap().ends_with("-3.json"));
        for name in [
            "0123456789ab-top-20261031T235959Z.json",
            "fresh-scans---prune-20261031T235959Z-2.json",
            "0123456789ab-by-type-20261031T235959Z-100.json",
        ] {
            assert!(is_spill_name(name), "{name}");
        }
        for name in [
            "report-final.xlsx",
            "old.json",
            "notes-top-20261031T235959Z.json",
            "0123456789AB-top-20261031T235959Z.json",
            "0123456789a-top-20261031T235959Z.json",
            "0123456789ab-Top-20261031T235959Z.json",
            "0123456789ab--20261031T235959Z.json",
            "0123456789ab-top-2026-10-31.json",
            "0123456789ab-top-20261031T2359Z.json",
            "0123456789ab-top-20261031T235959Z.json.bak",
            "0123456789ab-top-20261031T235959Z-1.json",
            "0123456789ab-top-20261031T235959Z-02.json",
            "0123456789ab-top-20261031T235959Z-101.json",
            "0123456789ab-top-20261031T235959Z.JSON",
            "fresh-top-20261031T235959Z",
            "fresh-top-1234567\u{e9}T00000Z.json",
        ] {
            assert!(!is_spill_name(name), "{name}");
        }
    }
}
