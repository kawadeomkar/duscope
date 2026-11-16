//! `scans`: the saved scans under the index directory, listed newest
//! first (with PATHs, those of the directories named and of directories
//! under them); with `--prune`, the ones past keeping removed, and the
//! temporary and spill files left behind; with `--check`, one checked for
//! damage and for changes to its tree.
//!
//! `--prune` keeps the five newest scans of each root and every scan under
//! seven days old, a temporary file (`<id>.db.tmp-<pid>`) while its writer
//! runs and for 24 hours, and a spill file for seven days. A file it cannot
//! read as an index is left alone, and so is anything in `scans` that is
//! not a regular file named as the index writer names its temporary files,
//! and anything in `out` that is not a regular file with a spill's name; a
//! `scans` or `out` that is a symbolic link is refused before anything is
//! removed, and each file is removed through the folder opened once, while
//! it is still the file listed. `--check` runs `PRAGMA quick_check`, a
//! damaged file exiting 5 (`index_corrupt`), then sweeps every directory
//! the scan holds (`duscope_core::check`): one that changed since makes the
//! scan `structurally_stale`, exiting 1 (`index_stale`) with the row
//! printed. The index directory, which the save itself wrote to, is left
//! out of the sweep with everything under it, but never the scan's root.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use duscope_core::check;
use duscope_core::envelope::{
    CheckedEntry, Listing, ListingQuery, PrunedEntry, SCHEMA_VERSION, SavedScanEntry, Tool,
};
use duscope_core::index::{Index, ScanId};
use duscope_core::query::{
    ScanQuery, ScanSelector, TreeView, clock, index_files, resolve_scan, shortest_prefix,
    timegrammar,
};
use duscope_core::resolve::resolve_path;
use duscope_walk::{Attrs, CheckedDir, RemoveError};

use super::next::{self, Hints};
use super::source::{core_failure, scanned};
use super::{Exit, Failure};
use crate::cli::ScansArgs;
use crate::out::{Output, is_spill_name, iso8601, thousands};

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
        list(&dir, &args.paths, output)
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

/// The listing of saved scans: every one, or with `paths`, those whose
/// root is one of them or lies under one.
fn list(dir: &Path, paths: &[PathBuf], output: &Output) -> Result<(), Failure> {
    // People see each id's shortest prefix among every index file, so the
    // prefix they type to --scan names this one.
    let files = index_files(dir).map_err(core_failure)?;
    let mut scans = saved(&files);
    if !paths.is_empty() {
        let cwd = std::env::current_dir().map_err(|e| {
            Failure::new(
                Exit::Runtime,
                "io_error",
                format!("cannot read the working directory: {e}"),
            )
        })?;
        let under: Vec<PathBuf> = paths.iter().map(|p| resolve_path(&cwd, p).real).collect();
        scans.retain(|index| under.iter().any(|p| index.root_path().starts_with(p)));
    }
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

/// `--prune`. A `scans` or `out` that is a symbolic link is refused before
/// anything is removed: files are removed from the index directory's own
/// folders, each opened once and removed from through its descriptor,
/// never from one a link leads to.
fn prune(dir: &Path, now: i64, output: &Output) -> Result<(), Failure> {
    let scans = folder(
        dir,
        "scans",
        "saved scans",
        "Put the folder the link leads to in its place, then run duscope scans --prune again",
    )?;
    let out = folder(
        dir,
        "out",
        "spills",
        "Remove the link, then run duscope scans --prune again",
    )?;
    let mut removed = Vec::new();
    let mut kept = 0u64;
    if let Some(scans) = &scans {
        // The scans of each root, newest first: the first five stay, and
        // any younger than a week.
        let mut by_root: BTreeMap<Vec<u8>, Vec<Index>> = BTreeMap::new();
        for index in saved(&index_files(dir).map_err(core_failure)?) {
            by_root
                .entry(index.meta().root.clone())
                .or_default()
                .push(index);
        }
        for indexes in by_root.into_values() {
            for (n, index) in indexes.into_iter().enumerate() {
                let age = now.saturating_sub(index.meta().finished_at);
                let young = age < DAY_NS * i64::try_from(KEEP_DAYS).unwrap_or(i64::MAX);
                if n < KEEP_NEWEST || young {
                    kept += 1;
                    continue;
                }
                let id = index.meta().scan_id.to_string();
                let file = index.file().to_owned();
                drop(index);
                let found = Found::at(&file).map_err(|e| not_removed(&file, &e))?;
                removed.push(remove(scans, &found, "index", Some(id))?);
            }
        }
        for file in stray_temporaries(dir) {
            removed.push(remove(scans, &file, "temporary", None)?);
        }
    }
    if let Some(out) = &out {
        for file in old_spills(dir) {
            removed.push(remove(out, &file, "spill", None)?);
        }
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

/// The index directory's folder `name`, of `what`, opened once for
/// `--prune` to remove files through: `None` where it is no directory.
/// It is opened from `/` one component at a time without following a link
/// (`duscope_walk::CheckedDir`, as `clean --permanent` opens a directory),
/// after the index directory is resolved, so a folder swapped for a link
/// later leads no removal elsewhere. One that is a symbolic link already
/// is refused, with `hint`.
fn folder(dir: &Path, name: &str, what: &str, hint: &str) -> Result<Option<CheckedDir>, Failure> {
    let path = dir.join(name);
    match fs::symlink_metadata(&path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(Failure::new(
                Exit::Runtime,
                "refused_path",
                format!(
                    "{} is a symbolic link, not the index directory's own folder of {what}",
                    path.display()
                ),
            )
            .hint(hint));
        }
        Ok(meta) if meta.is_dir() => {}
        _ => return Ok(None),
    }
    let failed = |why: String| {
        Failure::new(
            Exit::Runtime,
            "io_error",
            format!("cannot open {}: {why}", path.display()),
        )
        .hint("Run duscope scans --prune again")
    };
    let real = fs::canonicalize(dir).map_err(|e| failed(e.to_string()))?;
    CheckedDir::open(&real.join(name))
        .map(Some)
        .map_err(|e| match e {
            RemoveError::Changed { .. } => failed("it changed while it was opened".to_owned()),
            RemoveError::Io { source, .. } => failed(source.to_string()),
            other => failed(other.to_string()),
        })
}

/// A file `--prune` removes: where it was listed, and its device and inode,
/// size and age as the listing read them, without following a link.
struct Found {
    path: PathBuf,
    ident: (u64, u64),
    bytes: u64,
    age: Option<Duration>,
}

impl Found {
    /// `entry` of a folder `--prune` reads, when it is a regular file.
    fn regular(entry: &fs::DirEntry) -> Option<Found> {
        let meta = entry.metadata().ok().filter(fs::Metadata::is_file)?;
        Some(Found::of(entry.path(), &meta))
    }

    /// The file at `path`.
    fn at(path: &Path) -> std::io::Result<Found> {
        fs::symlink_metadata(path).map(|meta| Found::of(path.to_owned(), &meta))
    }

    fn of(path: PathBuf, meta: &fs::Metadata) -> Found {
        Found {
            path,
            ident: (meta.dev(), meta.ino()),
            bytes: meta.len(),
            age: meta
                .modified()
                .ok()
                .and_then(|modified| SystemTime::now().duration_since(modified).ok()),
        }
    }

    /// Whether it was last modified longer than `age` ago.
    fn older_than(&self, age: Duration) -> bool {
        self.age.is_some_and(|elapsed| elapsed > age)
    }
}

/// Removes `file` from `folder`, as a row of `--prune`'s listing: by its
/// name, through the folder's descriptor, and only while that name is the
/// file listed, its device and inode the listing's. What a link swapped in
/// for the folder leads to is never reached, and a file replaced since the
/// listing stays.
fn remove(
    folder: &CheckedDir,
    file: &Found,
    kind: &'static str,
    id: Option<String>,
) -> Result<PrunedEntry, Failure> {
    let name = file.path.file_name().unwrap_or_default();
    duscope_walk::unlink_checked(folder, name, file.ident, &mut 0).map_err(|e| match e {
        RemoveError::Changed { .. } => not_removed(
            &file.path,
            &"it has gone or been replaced since it was listed",
        ),
        RemoveError::Io { source, .. } => not_removed(&file.path, &source),
        other => not_removed(&file.path, &other),
    })?;
    Ok(PrunedEntry {
        path: file.path.to_string_lossy().into_owned(),
        kind,
        id,
        size_bytes: file.bytes,
    })
}

/// `--prune`'s failure to remove the file at `path`, for `why`.
fn not_removed(path: &Path, why: &dyn std::fmt::Display) -> Failure {
    Failure::new(
        Exit::Runtime,
        "io_error",
        format!("cannot remove {}: {why}", path.display()),
    )
    .hint("Remove the file by hand, then run duscope scans --prune again")
}

/// The temporary files of writes that will not finish: regular files with
/// the name the index writer gives one, whose process is gone or that are
/// older than a day. Nothing else in `scans` is taken.
fn stray_temporaries(dir: &Path) -> Vec<Found> {
    let Ok(entries) = fs::read_dir(dir.join("scans")) else {
        return Vec::new();
    };
    let mut files: Vec<Found> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let pid = entry.file_name().to_str().and_then(temporary_pid)?;
            Found::regular(&entry).filter(|file| {
                !duscope_walk::process_alive(pid)
                    || file.older_than(Duration::from_secs(TEMP_HOURS * 3600))
            })
        })
        .collect();
    files.sort_by(|a, b| a.path.cmp(&b.path));
    files
}

/// The process id in `name` when it is a name the index writer gives its
/// temporary file (`duscope_core::index::writer`'s `write_in`):
/// `<id>.db.tmp-<pid>`, `<id>` a scan id and `<pid>` the writer's process
/// id in decimal digits, as `std::process::id()` prints it (no sign, no
/// leading zero).
fn temporary_pid(name: &str) -> Option<u32> {
    let (id, pid) = name.split_once(".db.tmp-")?;
    ScanId::parse(id)?;
    if pid.starts_with('0') || !pid.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    pid.parse().ok()
}

/// The spill files (`out/` under the index directory) older than a week:
/// regular files with the names a spill is given, and nothing else.
fn old_spills(dir: &Path) -> Vec<Found> {
    let Ok(entries) = fs::read_dir(dir.join("out")) else {
        return Vec::new();
    };
    let mut files: Vec<Found> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_str().is_some_and(is_spill_name))
        .filter_map(|entry| Found::regular(&entry))
        .filter(|file| file.older_than(Duration::from_secs(KEEP_DAYS * 86_400)))
        .collect();
    files.sort_by(|a, b| a.path.cmp(&b.path));
    files
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
    // The index directory changes with every save into it: a scan of a
    // tree that holds it would otherwise never be fresh.
    let leave_out = [fs::canonicalize(dir).unwrap_or_else(|_| dir.to_owned())];
    let swept = check::sweep(&index, &leave_out).map_err(core_failure)?;
    let stale = swept.stale();
    let id = index.meta().scan_id.to_string();
    let root = index.root_path().to_string_lossy().into_owned();
    let entry = CheckedEntry {
        id: id.clone(),
        index_path: index.file().to_string_lossy().into_owned(),
        quick_check: "ok",
        structurally_stale: stale,
    };
    let dirs = thousands(swept.dir_count);
    let footer = match &swept.first_changed {
        Some(first) if stale => format!(
            "{} of {dirs} directories changed since the scan, {} first",
            thousands(swept.changed_count),
            first.display()
        ),
        _ if stale => format!("the saved scan {id} holds no directory to compare with its tree"),
        _ => format!("the saved scan {id} is sound, and its {dirs} directories are unchanged"),
    };
    let mut checked = listing("scans --check", dir, vec![entry]);
    checked.ok = !stale;
    // An import has no scan options to scan with again: a new scan of its
    // root renews it.
    let renewable = scanned(&index);
    if let Some(mut hints) = Hints::new(Some(&id), Some(index.file())) {
        let root = Some(root.as_str());
        checked.next = match (stale, renewable) {
            (true, true) => hints.run("rescan", root, &[]).done(),
            (true, false) => Vec::new(),
            (false, _) => hints.run("summary", root, &[]).run("top", root, &[]).done(),
        };
    }
    output.listing(&checked, &footer)?;
    if !stale {
        return Ok(());
    }
    // The row says so already; the failure goes to stderr with the exit.
    Err(Failure::new(
        Exit::Runtime,
        "index_stale",
        format!("the saved scan {id} is stale: its tree changed since it was taken"),
    )
    .hint(if renewable {
        format!(
            "Run {} to measure the tree again",
            next::command("rescan", None, &[], &id, index.file())
        )
    } else {
        format!("Run duscope scan --save {root} to measure the tree again")
    })
    .reported())
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

#[cfg(test)]
mod tests {
    use std::fs::{self, File};
    use std::path::Path;
    use std::time::{Duration, SystemTime};

    use duscope_testkit::TestEnv;

    use super::{folder, old_spills, remove};

    /// Writes a spill at `path`, last modified eight days ago.
    fn old_spill(path: &Path) {
        fs::write(path, b"{}").unwrap();
        File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(8 * 86_400))
            .unwrap();
    }

    #[test]
    fn p5_sec_01_prune_removes_through_the_folder_it_opened_never_a_link_swapped_in() {
        let env = TestEnv::new();
        let dir = env.index_dir();
        let out = dir.join("out");
        fs::create_dir_all(&out).unwrap();
        let name = "fresh-top-20300101T000000Z.json";
        old_spill(&out.join(name));
        let opened = folder(&dir, "out", "spills", "").unwrap().unwrap();
        let ours = old_spills(&dir);
        // `out` moved aside once opened, and in its place a link to a
        // folder holding a spill of the same name, as old.
        let elsewhere = env.tmp_dir().join("elsewhere");
        fs::create_dir_all(&elsewhere).unwrap();
        old_spill(&elsewhere.join(name));
        let aside = dir.join("out.aside");
        fs::rename(&out, &aside).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &out).unwrap();
        // The listing goes by path, through the link, and finds their file:
        // not the one at that name in the folder opened, so nothing goes.
        let theirs = old_spills(&dir);
        assert_eq!(theirs.len(), 1);
        let failure = remove(&opened, &theirs[0], "spill", None).err().unwrap();
        assert_eq!(failure.code, "io_error");
        assert_eq!(
            failure.message,
            format!(
                "cannot remove {}: it has gone or been replaced since it was listed",
                out.join(name).display()
            )
        );
        assert!(elsewhere.join(name).exists() && aside.join(name).exists());
        // Listed before the swap: removed from the folder opened, where it
        // is, and theirs stays.
        let row = remove(&opened, &ours[0], "spill", None).unwrap();
        assert_eq!(row.path, out.join(name).to_str().unwrap());
        assert!(!aside.join(name).exists());
        assert!(elsewhere.join(name).exists());
    }
}
