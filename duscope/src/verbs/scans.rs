//! `scans`: the saved scans under the index directory, listed newest
//! first; with `--prune`, the ones past keeping removed, and the temporary
//! and spill files left behind; with `--check`, one checked for damage.
//!
//! `--prune` keeps the five newest scans of each root and every scan under
//! seven days old, a temporary file (`<id>.db.tmp-<pid>`) while its writer
//! runs and for 24 hours, and a spill file for seven days. A file it cannot
//! read as an index is left alone. `--check` runs `PRAGMA quick_check` until
//! phase 5 adds the staleness sweep.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use duscope_core::envelope::{
    CheckedEntry, Listing, ListingQuery, PrunedEntry, SCHEMA_VERSION, SavedScanEntry, Tool,
};
use duscope_core::index::{Index, ScanId};
use duscope_core::query::{
    ScanQuery, ScanSelector, TreeView, clock, index_files, resolve_scan, shortest_prefix,
    timegrammar,
};
use duscope_walk::Attrs;

use super::next::Hints;
use super::source::{core_failure, scanned};
use super::{Exit, Failure};
use crate::cli::ScansArgs;
use crate::out::{Output, iso8601, thousands};

/// Scans of one root `--prune` keeps whatever their age.
const KEEP_NEWEST: usize = 5;

/// How long `--prune` keeps every scan, and a spill file.
const KEEP_DAYS: u64 = 7;

/// How long `--prune` keeps a temporary file whose writer still runs.
const TEMP_HOURS: u64 = 24;

const DAY_NS: i64 = 86_400 * 1_000_000_000;

/// `scans`, `scans --prune` or `scans --check`.
pub fn run(args: &ScansArgs, index_dir: Option<&Path>, output: &Output) -> Result<(), Failure> {
    let listed = if args.prune {
        output.fields_first::<PrunedEntry>("scans --prune")?
    } else if args.check.is_some() {
        output.fields_first::<CheckedEntry>("scans --check")?
    } else {
        output.fields_first::<SavedScanEntry>("scans")?
    };
    if listed {
        return Ok(());
    }
    let dir = match index_dir {
        Some(dir) => dir.to_owned(),
        None => {
            duscope_core::index::default_dir(|name| std::env::var_os(name)).ok_or_else(|| {
                Failure::new(
                    Exit::Usage,
                    "usage",
                    "no index directory: neither HOME nor XDG_CACHE_HOME is set",
                )
                .hint("Run duscope scans with --index-dir naming the directory")
            })?
        }
    };
    let dir = std::path::absolute(&dir).unwrap_or(dir);
    let now = match &args.test_now {
        Some(text) => timegrammar::parse_date(text)
            .map(|date| date.ns)
            .map_err(|e| Failure::new(Exit::Usage, "usage", format!("--test-now {e}")))?,
        None => clock::now(),
    };
    if args.prune {
        prune(&dir, now, output)
    } else if let Some(selector) = &args.check {
        check(&dir, selector, &args.trust, now, output)
    } else {
        list(&dir, output)
    }
}

/// The saved scans among `files` that can be read, newest first, and a
/// line on stderr for each file that cannot.
fn saved(files: &[(ScanId, PathBuf)]) -> Vec<Index> {
    let mut scans: Vec<Index> = files
        .iter()
        .filter_map(|(_, file)| match Index::open(file) {
            Ok(index) => {
                // The root's row gives the listing its totals; a file whose
                // rows cannot be read is left to --check.
                let _ = index.node(0);
                match index.failure() {
                    None => Some(index),
                    Some(e) => {
                        log::warn!("cannot read the index {}: {e}", file.display());
                        None
                    }
                }
            }
            Err(e) => {
                log::warn!("{e}");
                None
            }
        })
        .collect();
    scans.sort_by(|a, b| {
        (b.meta().started_at, &b.meta().scan_id).cmp(&(a.meta().started_at, &a.meta().scan_id))
    });
    scans
}

/// The listing of saved scans.
fn list(dir: &Path, output: &Output) -> Result<(), Failure> {
    // People see each id's shortest prefix among every index file, so the
    // prefix they type to --scan names this one.
    let files = index_files(dir);
    let scans = saved(&files);
    let ids: Vec<&str> = files.iter().map(|(id, _)| id.as_str()).collect();
    let entries: Vec<SavedScanEntry> = scans
        .iter()
        .map(|index| {
            let meta = index.meta();
            let root = index.node(0);
            let id = meta.scan_id.to_string();
            SavedScanEntry {
                short_id: shortest_prefix(&id, &ids).to_owned(),
                id,
                root: index.root_path().to_string_lossy().into_owned(),
                started_at: iso8601(meta.started_at),
                finished_at: iso8601(meta.finished_at),
                attrs: match index.attrs() {
                    Attrs::Basic => "basic",
                    Attrs::Full => "full",
                },
                entry_count: u64::from(index.len()),
                allocated_bytes: root.allocated,
                apparent_bytes: root.apparent,
                file_count: root.file_count,
                dir_count: root.dir_count,
                parent_scan_id: meta.parent_scan_id.clone(),
                source: meta.source.clone(),
                index_version: index.version().to_owned(),
                index_bytes: fs::metadata(index.file()).map_or(0, |m| m.len()),
                index_path: index.file().to_string_lossy().into_owned(),
            }
        })
        .collect();
    let bytes: u64 = entries.iter().map(|e| e.index_bytes).sum();
    let footer = format!(
        "{} saved scans · {} in {}",
        thousands(entries.len() as u64),
        output.units.size(bytes),
        dir.display()
    );
    let mut listed = listing("scans", dir, entries);
    // The newest scan, at the top.
    if let Some(newest) = listed.entries.first()
        && let Some(mut hints) = Hints::new(Some(&newest.id), Some(Path::new(&newest.index_path)))
    {
        let root = Some(newest.root.as_str());
        listed.next = hints
            .run("summary", root, &[])
            .run("top", root, &[])
            .run("caches", root, &[])
            .done();
    }
    output.listing(&listed, &footer)
}

/// `--prune`.
fn prune(dir: &Path, now: i64, output: &Output) -> Result<(), Failure> {
    let mut removed = Vec::new();
    // The scans of each root, newest first: the first five stay, and any
    // younger than a week.
    let mut by_root: BTreeMap<Vec<u8>, Vec<Index>> = BTreeMap::new();
    for index in saved(&index_files(dir)) {
        by_root
            .entry(index.meta().root.clone())
            .or_default()
            .push(index);
    }
    let mut kept = 0u64;
    for scans in by_root.into_values() {
        for (n, index) in scans.into_iter().enumerate() {
            let age = now.saturating_sub(index.meta().finished_at);
            let young = age < DAY_NS * i64::try_from(KEEP_DAYS).unwrap_or(i64::MAX);
            if n < KEEP_NEWEST || young {
                kept += 1;
                continue;
            }
            let id = index.meta().scan_id.to_string();
            let file = index.file().to_owned();
            drop(index);
            removed.push(remove(&file, "index", Some(id))?);
        }
    }
    for file in stray_temporaries(dir) {
        removed.push(remove(&file, "temporary", None)?);
    }
    for file in old_spills(dir) {
        removed.push(remove(&file, "spill", None)?);
    }
    let bytes: u64 = removed.iter().map(|r| r.size_bytes).sum();
    let footer = format!(
        "removed {} files, {} · kept {} saved scans",
        thousands(removed.len() as u64),
        output.units.size(bytes),
        thousands(kept)
    );
    output.listing(&listing("scans --prune", dir, removed), &footer)
}

/// Removes `file`, as a row of `--prune`'s listing.
fn remove(file: &Path, kind: &'static str, id: Option<String>) -> Result<PrunedEntry, Failure> {
    let bytes = fs::metadata(file).map_or(0, |m| m.len());
    fs::remove_file(file).map_err(|e| {
        Failure::new(
            Exit::Runtime,
            "io_error",
            format!("cannot remove {}: {e}", file.display()),
        )
        .hint("Remove the file by hand, then run duscope scans --prune again")
    })?;
    Ok(PrunedEntry {
        path: file.to_string_lossy().into_owned(),
        kind,
        id,
        size_bytes: bytes,
    })
}

/// The temporary files of writes that will not finish: their process is
/// gone, or they are older than a day.
fn stray_temporaries(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir.join("scans")) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|entry| {
            let name = entry.file_name();
            let Some(pid) = name
                .to_str()
                .and_then(|n| n.rsplit_once(".db.tmp-"))
                .and_then(|(_, pid)| pid.parse::<u32>().ok())
            else {
                return false;
            };
            !duscope_walk::process_alive(pid)
                || older_than(entry, Duration::from_secs(TEMP_HOURS * 3600))
        })
        .map(|entry| entry.path())
        .collect();
    files.sort();
    files
}

/// The spill files (`out/` under the index directory) older than a week.
fn old_spills(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir.join("out")) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_file()))
        .filter(|entry| older_than(entry, Duration::from_secs(KEEP_DAYS * 86_400)))
        .map(|entry| entry.path())
        .collect();
    files.sort();
    files
}

/// Whether `entry` was last modified longer than `age` ago.
fn older_than(entry: &fs::DirEntry, age: Duration) -> bool {
    entry
        .metadata()
        .and_then(|m| m.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_some_and(|elapsed| elapsed > age)
}

/// `--check`.
fn check(
    dir: &Path,
    selector: &str,
    trust: &[PathBuf],
    now: i64,
    output: &Output,
) -> Result<(), Failure> {
    let selector = ScanSelector::parse(selector).map_err(core_failure)?;
    let cwd = std::env::current_dir().map_err(|e| {
        Failure::new(
            Exit::Runtime,
            "io_error",
            format!("cannot read the working directory: {e}"),
        )
    })?;
    let q = ScanQuery {
        index_dir: dir,
        path: None,
        cwd: &cwd,
        max_age_ns: i64::MAX,
        now,
        trust,
    };
    let Some(resolved) = resolve_scan(&selector, &q, scanned).map_err(core_failure)? else {
        return Err(Failure::new(
            Exit::NotFound,
            "scan_not_found",
            format!("no saved scan holds {}", cwd.display()),
        )
        .hint("Run duscope scans to list the saved scans"));
    };
    let index = resolved.index;
    let problems = index.quick_check().map_err(core_failure)?;
    if let Some(first) = problems.first() {
        return Err(Failure::new(
            Exit::Index,
            "index_corrupt",
            format!(
                "the saved scan {} is damaged: {first} ({} problems in {})",
                index.meta().scan_id,
                problems.len(),
                index.file().display()
            ),
        )
        .hint(format!(
            "Run duscope scan --save {} to measure the tree again",
            index.root_path().display()
        )));
    }
    let entry = CheckedEntry {
        id: index.meta().scan_id.to_string(),
        index_path: index.file().to_string_lossy().into_owned(),
        quick_check: "ok",
    };
    let footer = format!("the saved scan {} is sound", entry.id);
    output.listing(&listing("scans --check", dir, vec![entry]), &footer)
}

/// The listing envelope of `rows`.
fn listing<T>(command: &'static str, dir: &Path, rows: Vec<T>) -> Listing<T> {
    let count = rows.len() as u64;
    Listing {
        ok: true,
        schema_version: SCHEMA_VERSION,
        tool: Tool::CURRENT,
        query: ListingQuery {
            command,
            index_dir: dir.to_string_lossy().into_owned(),
        },
        entries: rows,
        returned_count: count,
        total_count: count,
        truncated: false,
        next_cursor: None,
        warnings: Vec::new(),
        next: Vec::new(),
    }
}
