//! `scan`, and the scan every verb of phase 1 runs: the walk into the
//! arena, then the roll-up; and the parts of the envelope that describe
//! the scan (the scan block, its warnings, the footer for people).

use std::collections::HashMap;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use duscope_core::arena::Arena;
use duscope_core::envelope::{ExcludedEntry, ScanBlock, VolumeEntry, Warning};
use duscope_core::query::{ArenaView, SizeKind, TreeView};
use duscope_core::rollup::{RollUp, roll_up};
use duscope_core::rules::Caches;
use duscope_walk::{Attrs, DirId, IgnoreRules, Skip, WalkError, WalkOptions, WalkSummary, walk};
use globset::{Glob, GlobBuilder, GlobSetBuilder};

use super::{Exit, Failure};
use crate::cli::{AttrsArg, ProviderArg, ScanArgs, WalkArgs};
use crate::out::{Output, iso8601, thousands};

/// A finished scan.
#[derive(Debug)]
pub struct Scanned {
    /// The tree, rolled up.
    pub arena: Arena,
    /// Its pre-order numbering and what charging decided.
    pub rollup: RollUp,
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

/// `scan`: the root's largest entries, as `top` lists them, under the
/// scan block; on a terminal the footer alone until the treemap arrives
/// (P1-MAP-02).
pub fn run(args: &ScanArgs, output: &Output) -> Result<(), Failure> {
    if args.save {
        return Err(not_yet("--save", "the index"));
    }
    if args.interactive {
        return Err(not_yet("--interactive", "the interactive viewer"));
    }
    super::query::scan(args, output)
}

impl Scanned {
    /// The scan as the queries read it.
    pub fn view(&self) -> ArenaView<'_> {
        let root = std::path::absolute(&self.root).unwrap_or_else(|_| self.root.clone());
        ArenaView::new(&self.arena, &self.rollup, root)
    }
}

/// The size the rows are ranked and shown by.
pub fn size_kind(args: &WalkArgs) -> SizeKind {
    if args.apparent_size || args.size == Some(crate::cli::SizeKind::Apparent) {
        SizeKind::Apparent
    } else {
        SizeKind::Allocated
    }
}

/// The scan block: the root, the scan's options, times and totals, and
/// the volumes it met.
pub fn scan_block(scanned: &Scanned, walk_args: &WalkArgs, size: SizeKind) -> ScanBlock {
    let arena = &scanned.arena;
    let summary = &scanned.summary;
    let view = scanned.view();
    let root = view.node(0);
    ScanBlock {
        id: None,
        root: view.root_path().to_string_lossy().into_owned(),
        root_as_given: scanned.root_as_given.to_string_lossy().into_owned(),
        started_at: iso8601(scanned.started_ns),
        finished_at: iso8601(scanned.finished_ns),
        size_kind: size.as_str(),
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
        total_allocated_bytes: root.allocated,
        total_apparent_bytes: root.apparent,
        file_count: root.file_count,
        dir_count: root.dir_count,
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
                reason: v.reason.map(Skip::as_str),
                read_only: v.read_only,
                total_bytes: v.total_bytes,
                free_bytes: v.free_bytes,
            })
            .collect(),
        excluded_count: summary.excluded,
        excluded: excluded(arena, view.root_path()),
    }
}

/// The directories the excludes kept the scan out of, path bytewise.
fn excluded(arena: &Arena, root: &Path) -> Vec<ExcludedEntry> {
    let skipped: Vec<(DirId, &'static str)> = arena
        .skipped()
        .iter()
        .filter(|(_, reason)| matches!(reason, Skip::Excluded | Skip::ExcludedCache))
        .map(|(&dir, reason)| (dir, reason.as_str()))
        .collect();
    if skipped.is_empty() {
        return Vec::new();
    }
    // Each directory's name and parent, to spell the paths.
    let dirs: HashMap<DirId, (&[u8], DirId)> = arena
        .entries()
        .filter_map(|view| view.dir.map(|dir| (dir, (view.name, view.parent))))
        .collect();
    let path_of = |dir: DirId| {
        let mut names = Vec::new();
        let mut at = dir;
        while at != DirId::ROOT {
            let (name, parent) = dirs[&at];
            names.push(name);
            at = parent;
        }
        let mut path = root.as_os_str().as_bytes().to_vec();
        for name in names.iter().rev() {
            path.push(b'/');
            path.extend_from_slice(name);
        }
        path
    };
    let mut rows: Vec<(Vec<u8>, &'static str)> = skipped
        .into_iter()
        .map(|(dir, reason)| (path_of(dir), reason))
        .collect();
    rows.sort();
    rows.into_iter()
        .map(|(path, reason)| ExcludedEntry {
            path: String::from_utf8_lossy(&path).into_owned(),
            reason,
        })
        .collect()
}

/// The scan's warnings.
pub fn warnings(scanned: &Scanned) -> Vec<Warning> {
    let summary = &scanned.summary;
    let upper_bound = scanned.view().node(0).upper_bound;
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
pub fn footer(scan: &ScanBlock, scanned: &Scanned, output: &Output) -> String {
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

/// Now on the system clock, as Unix nanoseconds: the clock scans are timed
/// by.
pub fn now_ns() -> i64 {
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
    let rollup = roll_up(&mut arena);
    Ok(Scanned {
        arena,
        rollup,
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
        globs.add(exclude_glob(pattern, "--exclude")?);
    }
    for file in &args.exclude_from {
        let text = read_flag_file(file, "--exclude-from")?;
        for line in String::from_utf8_lossy(&text).lines() {
            let line = line.trim();
            if !line.is_empty() && !line.starts_with('#') {
                globs.add(exclude_glob(line, "--exclude-from")?);
            }
        }
    }
    let excludes = globs
        .build()
        .map_err(|e| Failure::new(Exit::Usage, "usage", format!("--exclude: {e}")))?;
    let mut ignore = IgnoreRules::new();
    for file in &args.ignore_from {
        let text = read_flag_file(file, "--ignore-from")?;
        ignore.add(&text, file).map_err(|e| {
            Failure::new(
                Exit::Usage,
                "usage",
                format!("--ignore-from {}: {e}", file.display()),
            )
        })?;
    }
    let exclude_caches = if args.exclude_caches {
        Caches::vendored().compile()
    } else {
        Vec::new()
    };
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
        ignore,
        exclude_caches,
        cancel: Arc::new(AtomicBool::new(false)),
        fallback: args.provider == Some(ProviderArg::Fallback),
    })
}

/// An `--exclude` pattern as the walk matches it, against paths relative to
/// the root: a pattern starting with `/` or `./` is anchored at the root;
/// any other matches at every depth (`*.tmp` matches `a/b/c.tmp`). `*` stays
/// within a path component and `**` crosses them; a trailing `/` is
/// dropped. A pattern naming nothing below the root (empty, `/` or `.`) is
/// a usage error.
fn exclude_glob(pattern: &str, flag: &str) -> Result<Glob, Failure> {
    let trimmed = pattern.trim_end_matches('/');
    let (anchored, rest) = match trimmed.strip_prefix("./") {
        Some(rest) => (true, rest),
        None => (trimmed.starts_with('/'), trimmed),
    };
    let rest = rest.trim_start_matches('/');
    if rest.is_empty() || rest == "." {
        return Err(Failure::new(
            Exit::Usage,
            "usage",
            format!("{flag} {pattern:?}: the pattern names no path below the root"),
        ));
    }
    let glob = if anchored {
        rest.to_owned()
    } else {
        format!("**/{rest}")
    };
    GlobBuilder::new(&glob)
        .literal_separator(true)
        .build()
        .map_err(|e| Failure::new(Exit::Usage, "usage", format!("{flag} {pattern:?}: {e}")))
}

/// The contents of a file a flag names; a file that cannot be read is a
/// usage error.
fn read_flag_file(file: &Path, flag: &str) -> Result<Vec<u8>, Failure> {
    std::fs::read(file).map_err(|e| {
        Failure::new(
            Exit::Usage,
            "usage",
            format!("{flag} {}: {e}", file.display()),
        )
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
        let (interrupted, stand_down) = (Arc::clone(&flag), Arc::clone(&disarmed));
        let watcher = thread::Builder::new()
            .name("interrupt".to_owned())
            .spawn(move || {
                while !interrupted.load(Ordering::Relaxed) {
                    if stand_down.load(Ordering::Relaxed) {
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
        crate::out::stderr_line("duscope: interrupted");
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

    use super::{ENOENT, Scanned};
    use crate::cli::{Cli, Verb};
    use crate::out::{Format, Output, Units};
    use crate::verbs::query::top_envelope;

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
        let rollup = roll_up(&mut arena);
        let scanned = Scanned {
            arena,
            rollup,
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
        let (envelope, _) =
            top_envelope(scanned, &args.walk, None, "scan", true, true, &output).unwrap();
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
