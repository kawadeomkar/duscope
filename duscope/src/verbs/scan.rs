//! `scan`, and the scan every verb of phase 1 runs: the walk into the
//! arena, then the roll-up. Until P1-CLI-03 brings the queries, every verb
//! lists the root's largest children.

use std::io::Write as _;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use duscope_core::arena::{Arena, DirRec, EntryIdx};
use duscope_core::envelope::{
    Envelope, QueryBlock, SCHEMA_VERSION, ScanBlock, Tool, TopEntry, VolumeEntry, Warning,
    name_fields,
};
use duscope_core::rollup::roll_up;
use duscope_walk::{Attrs, DirId, Flags, Kind, WalkError, WalkOptions, WalkSummary, walk};
use globset::{Glob, GlobSetBuilder};

use super::{Exit, Failure};
use crate::cli::{AttrsArg, ProviderArg, QueryArgs, ScanArgs, SizeKind, WalkArgs};
use crate::out::{Format, Output, iso8601, thousands};

/// A finished scan.
#[derive(Debug)]
pub struct Scanned {
    /// The tree, rolled up.
    pub arena: Arena,
    /// What the walk counted.
    pub summary: WalkSummary,
    /// How long the walk and the roll-up took.
    pub elapsed: Duration,
    /// The root, symbolic links resolved.
    pub root: PathBuf,
    /// The root as given.
    pub root_as_given: PathBuf,
    /// When the scan started and finished, Unix nanoseconds.
    pub started_ns: i64,
    /// When it finished.
    pub finished_ns: i64,
}

/// `scan`.
pub fn run(args: &ScanArgs, output: &Output) -> Result<(), Failure> {
    if args.save {
        return Err(not_yet("--save", "the index"));
    }
    if args.interactive {
        return Err(not_yet("--interactive", "the interactive viewer"));
    }
    list(&args.walk, &args.exclude, "scan", None, false, output)
}

/// Scans, then prints the root's largest children (its files with
/// `files`) for `verb`. On a terminal, `scan` prints its footer alone
/// until the treemap arrives (P1-MAP-02).
pub fn list(
    walk_args: &WalkArgs,
    excludes: &[String],
    verb: &'static str,
    query: Option<&QueryArgs>,
    files: bool,
    output: &Output,
) -> Result<(), Failure> {
    let scanned = scan(walk_args, excludes)?;
    let size_kind = size_kind(walk_args);
    let envelope = envelope(&scanned, walk_args, verb, query, files, size_kind, output);
    let footer = footer(&envelope.scan, &scanned, output);
    if output.format == Format::Treemap {
        output.line(&footer)
    } else {
        output.list(&envelope, &footer)
    }
}

/// The size the rows are ranked and shown by.
fn size_kind(args: &WalkArgs) -> &'static str {
    if args.apparent_size || args.size == Some(SizeKind::Apparent) {
        "apparent"
    } else {
        "allocated"
    }
}

/// The envelope of a listing: the scan block, then the root's children,
/// largest first, path bytewise among equals.
fn envelope(
    scanned: &Scanned,
    walk_args: &WalkArgs,
    verb: &'static str,
    query: Option<&QueryArgs>,
    files: bool,
    size_kind: &'static str,
    output: &Output,
) -> Envelope<TopEntry> {
    let arena = &scanned.arena;
    let root = std::path::absolute(&scanned.root).unwrap_or_else(|_| scanned.root.clone());
    let totals = Totals::of(arena, EntryIdx::ROOT);
    let apparent = size_kind == "apparent";
    let root_total = if apparent {
        totals.apparent
    } else {
        totals.allocated
    };
    let mut rows: Vec<TopEntry> = arena
        .entries()
        .skip(1)
        .filter(|view| view.parent == DirId::ROOT && (view.kind == Kind::Dir) != files)
        .map(|view| {
            let path = root.join(std::ffi::OsStr::from_bytes(view.name));
            let path_bytes = path.as_os_str().as_bytes().to_vec();
            let (name, name_raw) = name_fields(view.name);
            let t = Totals::of(arena, view.idx);
            let size = if apparent { t.apparent } else { t.allocated };
            TopEntry {
                path: String::from_utf8_lossy(&path_bytes).into_owned(),
                path_bytes,
                name,
                name_raw,
                kind: kind_name(view.kind),
                allocated_bytes: t.allocated,
                apparent_bytes: t.apparent,
                pct_of_root: pct(size, root_total),
                file_count: t.files,
                dir_count: t.dirs,
                max_mtime: iso8601(t.max_mtime_ns),
                flags: t.flags,
            }
        })
        .collect();
    let key = |row: &TopEntry| {
        if apparent {
            row.apparent_bytes
        } else {
            row.allocated_bytes
        }
    };
    rows.sort_by(|a, b| {
        key(b)
            .cmp(&key(a))
            .then_with(|| a.path_bytes.cmp(&b.path_bytes))
    });
    let total_count = rows.len() as u64;
    // ndjson streams every row; the others return at most 100.
    let limit = (output.format != Format::Ndjson)
        .then(|| query.and_then(|q| q.limit).unwrap_or(25).min(100));
    if let Some(limit) = limit {
        rows.truncate(limit);
    }
    let returned_count = rows.len() as u64;
    Envelope {
        ok: true,
        schema_version: SCHEMA_VERSION,
        tool: Tool::CURRENT,
        scan: scan_block(scanned, walk_args, &root, &totals, size_kind),
        query: QueryBlock {
            command: verb,
            path: root.to_string_lossy().into_owned(),
            depth: query.and_then(|q| q.depth),
            limit: limit.map(|l| l as u64),
            sort: format!("{size_kind}_desc"),
            filters: Vec::new(),
        },
        entries: rows,
        returned_count,
        total_count,
        truncated: returned_count < total_count,
        next_cursor: None,
        warnings: warnings(scanned, totals.upper_bound),
        next: Vec::new(),
    }
}

/// The scan block: the root, the scan's options, times and totals, and
/// the volumes it met.
fn scan_block(
    scanned: &Scanned,
    walk_args: &WalkArgs,
    root: &Path,
    totals: &Totals,
    size_kind: &'static str,
) -> ScanBlock {
    let arena = &scanned.arena;
    let summary = &scanned.summary;
    ScanBlock {
        id: None,
        root: root.to_string_lossy().into_owned(),
        root_as_given: scanned.root_as_given.to_string_lossy().into_owned(),
        started_at: iso8601(scanned.started_ns),
        finished_at: iso8601(scanned.finished_ns),
        size_kind,
        attrs: match walk_args.attrs {
            AttrsArg::Basic => "basic",
            AttrsArg::Full => "full",
        },
        thread_count: summary.threads as u64,
        fs_type: arena
            .volumes()
            .first()
            .map(|v| v.fs_type.clone())
            .unwrap_or_default(),
        device_confined: walk_args.one_file_system,
        total_allocated_bytes: totals.allocated,
        total_apparent_bytes: totals.apparent,
        file_count: totals.files,
        dir_count: totals.dirs,
        error_count: summary.unreadable_dirs + summary.entry_errors,
        vanished_count: arena.vanished(),
        incremental: false,
        volumes: arena
            .volumes()
            .iter()
            .map(|v| VolumeEntry {
                mount_point: v.mount_point.to_string_lossy().into_owned(),
                fs_type: v.fs_type.clone(),
                entered: v.entered(),
                reason: v.reason.map(duscope_walk::Skip::as_str),
                read_only: v.read_only,
                total_bytes: v.total_bytes,
                free_bytes: v.free_bytes,
            })
            .collect(),
    }
}

/// An entry's totals: a directory's subtree, or a file's own.
struct Totals {
    allocated: u64,
    apparent: u64,
    files: u64,
    dirs: u64,
    max_mtime_ns: i64,
    upper_bound: bool,
    flags: Vec<&'static str>,
}

impl Totals {
    fn of(arena: &Arena, idx: EntryIdx) -> Totals {
        let view = arena.entry(idx);
        let mut flags: Vec<&'static str> = FLAG_NAMES
            .iter()
            .filter(|(flag, _)| view.flags.contains(*flag))
            .map(|&(_, name)| name)
            .collect();
        match view.dir {
            Some(dir) => {
                let rec = arena.dir(dir);
                let upper_bound = rec.flags & DirRec::UPPER_BOUND != 0;
                if upper_bound {
                    flags.push("upper_bound");
                }
                if rec.flags & DirRec::UNREADABLE != 0 {
                    flags.push("unreadable");
                }
                Totals {
                    allocated: rec.cum_allocated,
                    apparent: rec.cum_apparent,
                    files: u64::from(rec.n_files),
                    dirs: u64::from(rec.n_dirs) + 1,
                    max_mtime_ns: rec.max_mtime_ns,
                    upper_bound,
                    flags,
                }
            }
            None => Totals {
                allocated: view.allocated,
                apparent: view.apparent,
                files: 1,
                dirs: 0,
                max_mtime_ns: view.mtime_ns,
                upper_bound: false,
                flags,
            },
        }
    }
}

/// The storage flags rows name.
const FLAG_NAMES: [(Flags, &str); 8] = [
    (Flags::SPARSE, "sparse"),
    (Flags::COMPRESSED, "compressed"),
    (Flags::DATALESS, "dataless"),
    (Flags::PURGEABLE, "purgeable"),
    (Flags::MAY_SHARE, "may_share"),
    (Flags::SHARES_ALL, "shares_all"),
    (Flags::MOUNT_ROOT, "mount_root"),
    (Flags::SUBVOLUME, "subvolume"),
];

/// A kind as rows name it.
fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Dir => "dir",
        Kind::File => "file",
        Kind::Symlink => "symlink",
        _ => "other",
    }
}

/// `part` as a percentage of `whole`, with one decimal.
fn pct(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        return 0.0;
    }
    let tenths = (u128::from(part) * 1000 + u128::from(whole) / 2) / u128::from(whole);
    f64::from(u32::try_from(tenths).unwrap_or(u32::MAX)) / 10.0
}

/// The scan's warnings.
fn warnings(scanned: &Scanned, upper_bound: bool) -> Vec<Warning> {
    let summary = &scanned.summary;
    let mut warnings = Vec::new();
    if summary.unreadable_dirs > 0 {
        warnings.push(Warning {
            code: "partial_read",
            message: format!(
                "{} directories could not be read; the totals are a lower bound",
                thousands(summary.unreadable_dirs)
            ),
            count: Some(summary.unreadable_dirs),
            hint: Some(
                "Rerun duscope as a user who can read them, or with --strict to fail instead"
                    .to_owned(),
            ),
        });
    }
    if summary.mounts_excluded > 0 {
        warnings.push(Warning {
            code: "mounts_excluded",
            message: format!(
                "{} mounted filesystems were not entered (scan.volumes says why)",
                thousands(summary.mounts_excluded)
            ),
            count: Some(summary.mounts_excluded),
            hint: Some(
                "Rerun duscope scan with --include-network, or without -x, to enter them"
                    .to_owned(),
            ),
        });
    }
    if scanned.arena.vanished() > 0 {
        warnings.push(Warning {
            code: "vanished",
            message: format!(
                "{} entries disappeared while the scan read them",
                thousands(scanned.arena.vanished())
            ),
            count: Some(scanned.arena.vanished()),
            hint: Some("Rerun duscope scan once the tree is quiet".to_owned()),
        });
    }
    if upper_bound {
        warnings.push(Warning {
            code: "upper_bound",
            message: "some files may share blocks with others; the totals are an upper bound"
                .to_owned(),
            count: None,
            hint: None,
        });
    }
    warnings
}

/// The one line for people: the root's total, counts, and time.
fn footer(scan: &ScanBlock, scanned: &Scanned, output: &Output) -> String {
    let total = if scan.size_kind == "apparent" {
        scan.total_apparent_bytes
    } else {
        scan.total_allocated_bytes
    };
    format!(
        "{} {} · {} files · {} dirs · {:.1} s, {} threads",
        output.units.size(total),
        scan.size_kind,
        thousands(scan.file_count),
        thousands(scan.dir_count),
        scanned.elapsed.as_secs_f64(),
        scan.thread_count,
    )
}

/// Now, as Unix nanoseconds.
fn now_ns() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_nanos()).ok())
        .unwrap_or(0)
}

/// Scans the tree `walk_args` names.
pub fn scan(walk_args: &WalkArgs, excludes: &[String]) -> Result<Scanned, Failure> {
    let given = match &walk_args.path {
        Some(path) => path.clone(),
        None => std::env::current_dir().map_err(|e| {
            Failure::new(
                Exit::Runtime,
                "io_error",
                format!("cannot read the current directory: {e}"),
            )
        })?,
    };
    // A root given as a symbolic link, or through one, is scanned as the
    // directory it leads to; the envelope keeps the path as given.
    let root = std::fs::canonicalize(&given).unwrap_or_else(|_| given.clone());
    let mut opts = options(walk_args, excludes)?;
    // Ctrl-C stops the walk; if it has not stopped soon after, the process
    // ends anyway. Nothing is written before a scan completes.
    opts.cancel = interrupt_flag();
    let started = Instant::now();
    let started_ns = now_ns();
    let mut arena = Arena::new(opts.attrs);
    let mut push_error = None;
    let summary = walk(&root, &opts, |event| {
        if push_error.is_none()
            && let Err(e) = arena.push(event)
        {
            push_error = Some(e);
        }
    })
    .map_err(|e| walk_failure(&given, e))?;
    if let Some(e) = push_error {
        return Err(Failure::new(Exit::Runtime, "io_error", e.to_string()));
    }
    // A root that is a directory nobody may list is a permission failure
    // (exit 4), one that fails otherwise (EIO, ELOOP) a runtime error; a
    // directory below it that cannot be read is a partial read.
    if let Some(&errno) = arena.unreadable().get(&DirId::ROOT) {
        return Err(if errno == EACCES || errno == EPERM {
            Failure::new(
                Exit::Permission,
                "permission_denied",
                format!("permission denied reading {}", given.display()),
            )
            .hint("Run duscope scan on a directory you can read")
        } else {
            Failure::new(
                Exit::Runtime,
                "io_error",
                format!(
                    "cannot read {}: {}",
                    given.display(),
                    std::io::Error::from_raw_os_error(errno)
                ),
            )
        });
    }
    roll_up(&mut arena);
    Ok(Scanned {
        arena,
        summary,
        elapsed: started.elapsed(),
        root,
        root_as_given: given,
        started_ns,
        finished_ns: now_ns(),
    })
}

/// The walk options the flags ask for.
fn options(args: &WalkArgs, excludes: &[String]) -> Result<WalkOptions, Failure> {
    let mut globs = GlobSetBuilder::new();
    for pattern in excludes {
        let glob = Glob::new(pattern).map_err(|e| {
            Failure::new(Exit::Usage, "usage", format!("--exclude {pattern:?}: {e}"))
        })?;
        globs.add(glob);
    }
    let excludes = globs
        .build()
        .map_err(|e| Failure::new(Exit::Usage, "usage", format!("--exclude: {e}")))?;
    Ok(WalkOptions {
        threads: args.threads.unwrap_or(0),
        follow_symlinks: args.follow_symlinks,
        one_file_system: args.one_file_system,
        include_network: args.include_network,
        attrs: match args.attrs {
            AttrsArg::Basic => Attrs::Basic,
            AttrsArg::Full => Attrs::Full,
        },
        clones: !args.no_clones,
        excludes,
        exclude_caches: Vec::new(),
        cancel: Arc::new(AtomicBool::new(false)),
        fallback: args.provider == Some(ProviderArg::Fallback),
    })
}

/// The failure a walk error is: a missing root is exit 3, an unreadable
/// one exit 4, a cancelled walk 130.
fn walk_failure(root: &Path, error: WalkError) -> Failure {
    match error {
        WalkError::Root { source, .. } => match source.raw_os_error() {
            Some(libc_enoent) if libc_enoent == ENOENT || libc_enoent == ENOTDIR => Failure::new(
                Exit::NotFound,
                "scan_not_found",
                format!("{} does not exist", root.display()),
            )
            .hint("Check the path, or run duscope scan in the directory to measure"),
            Some(errno) if errno == EACCES || errno == EPERM => Failure::new(
                Exit::Permission,
                "permission_denied",
                format!("permission denied reading {}", root.display()),
            )
            .hint("Run duscope scan on a directory you can read"),
            _ => Failure::new(
                Exit::Runtime,
                "io_error",
                format!("cannot read {}: {source}", root.display()),
            ),
        },
        WalkError::Cancelled => {
            Failure::new(Exit::Interrupted, "interrupted", "the scan was interrupted")
        }
        WalkError::NoProvider => Failure::new(
            Exit::Usage,
            "not_available",
            "this platform has no scanner; duscope runs on Linux and macOS",
        ),
        other => Failure::new(Exit::Runtime, "io_error", other.to_string()),
    }
}

/// How long a walk has to stop after Ctrl-C before the process ends
/// regardless: half the 200 ms the conventions allow, which leaves room for
/// a slow machine to notice the flag and end the process.
const INTERRUPT_GRACE: Duration = Duration::from_millis(100);

/// The process's Ctrl-C flag, set up once. A watcher thread ends the
/// process with exit 130 once the flag has been set for
/// [`INTERRUPT_GRACE`], in case the walk is still finishing a directory;
/// SIGINT sets the flag only once that thread runs. Where SIGINT was
/// inherited ignored (a background job), or the thread cannot be started,
/// Ctrl-C keeps its usual effect and the flag is never set.
fn interrupt_flag() -> Arc<AtomicBool> {
    static FLAG: OnceLock<Arc<AtomicBool>> = OnceLock::new();
    Arc::clone(FLAG.get_or_init(|| {
        let flag = Arc::new(AtomicBool::new(false));
        let disarmed = Arc::new(AtomicBool::new(false));
        let (watched, unwatched) = (Arc::clone(&flag), Arc::clone(&disarmed));
        let watcher = thread::Builder::new()
            .name("interrupt".to_owned())
            .spawn(move || {
                while !watched.load(Ordering::Relaxed) {
                    if unwatched.load(Ordering::Relaxed) {
                        return;
                    }
                    thread::sleep(Duration::from_millis(5));
                }
                thread::sleep(INTERRUPT_GRACE);
                report_interrupt();
                std::process::exit(i32::from(Exit::Interrupted.code()));
            });
        if watcher.is_ok() && !matches!(duscope_walk::cancel_on_interrupt(&flag), Ok(true)) {
            disarmed.store(true, Ordering::Relaxed);
        }
        flag
    }))
}

/// Reports Ctrl-C: one line on stderr, once, whatever the format.
pub fn report_interrupt() {
    static REPORTED: AtomicBool = AtomicBool::new(false);
    if !REPORTED.swap(true, Ordering::SeqCst) {
        // A closed stderr loses the line, not the exit code.
        let _ = writeln!(std::io::stderr().lock(), "duscope: interrupted");
    }
}

/// The errno values the exit codes distinguish; equal on Linux and macOS.
const ENOENT: i32 = 2;
const EPERM: i32 = 1;
const EACCES: i32 = 13;
const ENOTDIR: i32 = 20;

/// A flag of a later phase.
fn not_yet(flag: &str, what: &str) -> Failure {
    Failure::new(
        Exit::Usage,
        "not_available",
        format!("{flag} needs {what}, which this version does not have yet"),
    )
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    use clap::Parser;
    use duscope_core::arena::Arena;
    use duscope_core::rollup::roll_up;
    use duscope_walk::{Attrs, MemProvider, WalkOptions, walk_with};

    use super::{ENOENT, Scanned, envelope};
    use crate::cli::{Cli, Verb};
    use crate::out::{Format, Output, Units};

    #[test]
    fn p1_walk_08_a_vanished_file_increments_vanished_count() {
        let mut tree = MemProvider::new("/t");
        tree.file("kept", 10)
            .file("gone", 10)
            .unstatable("gone", ENOENT);
        let mut arena = Arena::new(Attrs::Basic);
        let summary = walk_with(&tree, Path::new("/t"), &WalkOptions::default(), |event| {
            arena.push(event).unwrap();
        })
        .unwrap();
        roll_up(&mut arena);
        let scanned = Scanned {
            arena,
            summary,
            elapsed: Duration::ZERO,
            root: "/t".into(),
            root_as_given: "/t".into(),
            started_ns: 0,
            finished_ns: 0,
        };
        let Some(Verb::Scan(args)) = Cli::try_parse_from(["duscope", "scan", "/t"]).unwrap().verb
        else {
            panic!("not scan");
        };
        let output = Output {
            format: Format::Json,
            color: false,
            units: Units::default(),
            null: false,
            header: false,
        };
        let envelope = envelope(
            &scanned,
            &args.walk,
            "scan",
            None,
            true,
            "allocated",
            &output,
        );
        assert_eq!(envelope.scan.vanished_count, 1);
        assert_eq!(envelope.scan.error_count, 0);
        let warning = envelope
            .warnings
            .iter()
            .find(|w| w.code == "vanished")
            .unwrap();
        assert_eq!(warning.count, Some(1));
        // The file that vanished is not a row.
        assert_eq!(envelope.entries.len(), 1);
    }
}
