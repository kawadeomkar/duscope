//! Which saved scan a verb reads: `--scan`'s selector, resolved against the
//! index directory, and the entry of that scan the verb is about.
//!
//! `latest`, which a verb uses when no `--scan` is given, is the newest
//! usable scan whose root contains the path the verb is about; a scan id
//! prefix of at least four hex digits names one scan; a path (one holding a
//! `/` or a `.`) names an index file. A scan older than `--max-age` is still
//! read, and the verb warns `stale_index`.
//!
//! An index file outside the index directory is read only when `--trust`
//! names it (SECURITY.md): anyone may have made it. The index directory and
//! its files should be 0700 and 0600; a looser one is read, with a warning.

use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Once;

use crate::CoreError;
use crate::index::{Index, ScanId};
use crate::query::TreeView;
use crate::resolve::resolve_in_index;

/// The fewest hex digits an id prefix may have.
pub const MIN_PREFIX: usize = 4;

/// What `--scan` names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanSelector {
    /// The newest usable scan whose root contains the path.
    Latest,
    /// The scan whose id starts with these hex digits.
    Prefix(String),
    /// The index file at this path.
    File(PathBuf),
}

impl ScanSelector {
    /// Reads `--scan`'s value.
    pub fn parse(text: &str) -> Result<ScanSelector, CoreError> {
        if text == "latest" {
            return Ok(ScanSelector::Latest);
        }
        if text.contains('/') || text.contains('.') {
            return Ok(ScanSelector::File(PathBuf::from(text)));
        }
        let hex = |b: u8| b.is_ascii_digit() || (b'a'..=b'f').contains(&b);
        if (MIN_PREFIX..=12).contains(&text.len()) && text.bytes().all(hex) {
            return Ok(ScanSelector::Prefix(text.to_owned()));
        }
        Err(CoreError::Selector(format!(
            "--scan {text:?} is not latest, a scan id or at least {MIN_PREFIX} of its leading \
             hex digits, or a path containing / or ."
        )))
    }
}

/// The index files under `index_dir` (`scans/<id>.db`), by id. Temporary
/// files and other names are left out.
pub fn index_files(index_dir: &Path) -> Vec<(ScanId, PathBuf)> {
    warn_if_open(index_dir);
    let Ok(dir) = std::fs::read_dir(index_dir.join("scans")) else {
        return Vec::new();
    };
    let mut files: Vec<(ScanId, PathBuf)> = dir
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name();
            let id = name.to_str()?.strip_suffix(".db")?;
            Some((ScanId::parse(id)?, entry.path()))
        })
        .collect();
    files.sort();
    files
}

/// The shortest prefix of `id`, at least [`MIN_PREFIX`] digits, that no id
/// in `others` other than `id` itself starts with: what people are shown.
pub fn shortest_prefix<'a>(id: &'a str, others: &[&str]) -> &'a str {
    (MIN_PREFIX..id.len())
        .map(|n| &id[..n])
        .find(|prefix| {
            !others
                .iter()
                .any(|other| *other != id && other.starts_with(prefix))
        })
        .unwrap_or(id)
}

/// [`shortest_prefix`] of `id` among the saved scans under `index_dir`.
pub fn short_id(index_dir: &Path, id: &str) -> String {
    let files = index_files(index_dir);
    let ids: Vec<&str> = files.iter().map(|(id, _)| id.as_str()).collect();
    shortest_prefix(id, &ids).to_owned()
}

/// `--under PATH` in a saved scan: the subtree's first and last ids, which
/// a query binds as `:root_id` and `:root_last`. A path the scan does not
/// hold is [`CoreError::ScanNotFound`].
pub fn under(index: &Index, path: &Path) -> Result<(u32, u32), CoreError> {
    let id = entry_at(index, path, "--under ")?;
    Ok((id, index.node(id).last_id))
}

/// The entry of `index` at `path`, or why there is none: a read that failed
/// is a damaged file, not a missing path. `what` names the flag in the
/// message.
fn entry_at(index: &Index, path: &Path, what: &str) -> Result<u32, CoreError> {
    match resolve_in_index(index, path) {
        Some(id) => Ok(id),
        None if index.failure().is_some() => Err(CoreError::IndexCorrupt {
            file: index.file().display().to_string(),
            message: index.failure().unwrap_or_default(),
        }),
        None => Err(CoreError::ScanNotFound(format!(
            "{what}{} is not in scan {}, whose root is {}",
            path.display(),
            index.meta().scan_id,
            index.root_path().display()
        ))),
    }
}

/// What a selector resolves against.
#[derive(Debug, Clone, Copy)]
pub struct ScanQuery<'a> {
    /// The index directory.
    pub index_dir: &'a Path,
    /// The path the verb is about, absolute and resolved; `None` when the
    /// command line gave none.
    pub path: Option<&'a Path>,
    /// The working directory, absolute: the path `latest` looks for when
    /// none was given, and what a relative index path is read from.
    pub cwd: &'a Path,
    /// `--max-age`, nanoseconds.
    pub max_age_ns: i64,
    /// Now, Unix nanoseconds.
    pub now: i64,
    /// `--trust`: the index files outside the index directory a selector
    /// may name, relative to `cwd` or absolute.
    pub trust: &'a [PathBuf],
}

/// A saved scan resolved for a verb.
#[derive(Debug)]
pub struct Resolved {
    /// The scan.
    pub index: Index,
    /// The entry the verb is about: the path's, or the root.
    pub base: u32,
    /// Whether the scan finished longer ago than `--max-age`.
    pub stale: bool,
}

/// Resolves `selector`. `latest` reads only scans `usable` accepts (taken
/// with the same options as the verb would scan with), and is `Ok(None)`
/// when none contains the path, which a verb then scans. A prefix or an
/// index path that names nothing, or a path outside the scan it names, is
/// [`CoreError::ScanNotFound`]; a prefix more than one scan has is
/// [`CoreError::Selector`].
pub fn resolve_scan(
    selector: &ScanSelector,
    q: &ScanQuery<'_>,
    usable: impl Fn(&Index) -> bool,
) -> Result<Option<Resolved>, CoreError> {
    let named = |index: Index| -> Result<Option<Resolved>, CoreError> {
        let path = q.path.unwrap_or(index.root_path());
        let base = entry_at(&index, path, "")?;
        Ok(Some(resolved(index, base, q)))
    };
    match selector {
        ScanSelector::Latest => Ok(latest(q, usable)),
        ScanSelector::Prefix(prefix) => {
            let matches: Vec<(ScanId, PathBuf)> = index_files(q.index_dir)
                .into_iter()
                .filter(|(id, _)| id.as_str().starts_with(prefix.as_str()))
                .collect();
            match matches.as_slice() {
                [] => Err(CoreError::ScanNotFound(format!(
                    "no saved scan has an id starting {prefix}"
                ))),
                [(_, file)] => named(Index::open(file)?),
                more => Err(CoreError::Selector(format!(
                    "{} saved scans have ids starting {prefix}: {}",
                    more.len(),
                    more.iter()
                        .map(|(id, _)| id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ))),
            }
        }
        ScanSelector::File(file) => {
            let file = q.cwd.join(file);
            if !file.is_file() {
                return Err(CoreError::ScanNotFound(format!(
                    "no index file at {}",
                    file.display()
                )));
            }
            if !trusted(&file, q) {
                return Err(CoreError::UntrustedIndex(file.display().to_string()));
            }
            named(Index::open(&file)?)
        }
    }
}

/// Whether `file` may be read: it is inside the index directory, or
/// `--trust` names it. Both sides are resolved, so a link inside the
/// directory to a file outside it is outside, and `--trust` may name the
/// file by any path to it.
fn trusted(file: &Path, q: &ScanQuery<'_>) -> bool {
    let Ok(file) = std::fs::canonicalize(file) else {
        return false;
    };
    if let Ok(dir) = std::fs::canonicalize(q.index_dir)
        && file.starts_with(&dir)
    {
        warn_if_open(q.index_dir);
        return true;
    }
    q.trust
        .iter()
        .any(|named| std::fs::canonicalize(q.cwd.join(named)).is_ok_and(|named| named == file))
}

/// Warns, once a process, when the index directory, its `scans`, or an
/// index file in it is open to other users or another user's: the scans
/// name every path measured. The writer makes them 0700 and 0600.
fn warn_if_open(index_dir: &Path) {
    static CHECKED: Once = Once::new();
    CHECKED.call_once(|| {
        let uid = duscope_walk::effective_uid();
        let scans = index_dir.join("scans");
        for dir in [index_dir, scans.as_path()] {
            let Ok(meta) = std::fs::metadata(dir) else {
                continue;
            };
            if meta.uid() != uid {
                log::warn!(
                    "the saved scans are not private: {} belongs to uid {}",
                    dir.display(),
                    meta.uid()
                );
            } else if meta.permissions().mode() & 0o077 != 0 {
                log::warn!(
                    "the saved scans are not private: {} is mode {:o}; run chmod 700 {}",
                    dir.display(),
                    meta.permissions().mode() & 0o7777,
                    dir.display()
                );
            }
        }
        let open = std::fs::read_dir(&scans)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().as_encoded_bytes().ends_with(b".db"))
            .filter(|entry| {
                entry
                    .metadata()
                    .is_ok_and(|meta| meta.uid() != uid || meta.mode() & 0o077 != 0)
            })
            .count();
        if open > 0 {
            log::warn!(
                "the saved scans are not private: others may read, or another user owns, \
                 {open} of the index files in {}; run chmod 600 {}/*.db",
                scans.display(),
                scans.display()
            );
        }
    });
}

/// The newest scan `usable` accepts whose root contains the path (or the
/// working directory) and holds an entry at it.
fn latest(q: &ScanQuery<'_>, usable: impl Fn(&Index) -> bool) -> Option<Resolved> {
    let path = q.path.unwrap_or(q.cwd);
    let mut found: Option<(Index, u32)> = None;
    for (_, file) in index_files(q.index_dir) {
        // A file that cannot be read is no candidate; `scans` lists it.
        let Ok(index) = Index::open(&file) else {
            continue;
        };
        let newer = found.as_ref().is_none_or(|(best, _)| {
            (index.meta().started_at, &index.meta().scan_id)
                > (best.meta().started_at, &best.meta().scan_id)
        });
        if !newer || !usable(&index) {
            continue;
        }
        // A directory the scan listed but did not enter holds nothing of
        // what is under it now: a scan of it measures it.
        if let Some(base) = resolve_in_index(&index, path).filter(|&base| index.entered(base)) {
            found = Some((index, base));
        }
    }
    found.map(|(index, base)| resolved(index, base, q))
}

fn resolved(index: Index, base: u32, q: &ScanQuery<'_>) -> Resolved {
    let age = q.now.saturating_sub(index.meta().finished_at);
    Resolved {
        stale: age > q.max_age_ns,
        index,
        base,
    }
}
