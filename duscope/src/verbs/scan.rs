//! `scan`, and the scan every verb of phase 1 runs: the walk into the
//! arena, then the roll-up; and the parts of the envelope that describe
//! the scan (the scan block, its warnings, the footer for people).

use std::collections::HashMap;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use duscope_core::arena::Arena;
use duscope_core::envelope::{ExcludedEntry, ScanBlock, VolumeEntry, Warning};
use duscope_core::query::{ArenaView, SizeKind, TreeView, clock, timegrammar};
use duscope_core::resolve::{ResolvedPath, resolve_path};
use duscope_core::rollup::{RollUp, roll_up};
use duscope_core::rules::Caches;
use duscope_walk::{
    Attrs, DirId, IgnoreRules, NameRef, ScanEvent, Skip, WalkError, WalkOptions, WalkSummary, walk,
};
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
    /// The root, absolute, symbolic links resolved: the path the walk read.
    pub root: PathBuf,
    /// The root as given.
    pub root_as_given: PathBuf,
    /// Whether `--clones` was given. Clones are counted by default, so only
    /// an explicit ask warns `clones_unavailable` on a filesystem without
    /// clone ids.
    pub clones_asked: bool,
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
        fs_type: arena.volumes().first().map(|v| v.fs_type.clone()),
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
    if scanned.clones_asked && summary.clones_unavailable {
        warnings.push(Warning {
            code: "clones_unavailable",
            message: "--clones was asked of a filesystem without clone ids; its clones count \
                      in full, as du counts them"
                .to_owned(),
            count: None,
            hint: Some(
                "Only APFS reports clone ids; rerun duscope without --clones elsewhere".to_owned(),
            ),
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

/// Now, Unix nanoseconds: `--test-now` when given, else [`clock::now`]
/// (which honours `DUSCOPE_TEST_NOW` in a debug build). The scan is timed
/// on it and the queries measure ages and the scan's own age on it, so both
/// read one clock.
pub fn now(walk_args: &WalkArgs) -> Result<i64, Failure> {
    match &walk_args.test_now {
        Some(text) => timegrammar::parse_date(text)
            .map(|date| date.ns)
            .map_err(|e| Failure::new(Exit::Usage, "usage", format!("--test-now {e}"))),
        None => Ok(clock::now()),
    }
}

/// Scans the tree `walk_args` names; a walk flag of a later phase is
/// refused before anything is read.
pub fn scan(walk_args: &WalkArgs, excludes: &[String]) -> Result<Scanned, Failure> {
    if walk_args.skip_containers {
        return Err(not_yet(
            "--skip-containers",
            "the rule that skips ~/Library/Containers",
        ));
    }
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
    // A root given as a symbolic link, or through one, is read as the path
    // it leads to; the envelope keeps the path as given beside it.
    let cwd = std::env::current_dir().unwrap_or_default();
    let ResolvedPath { real: root, .. } = resolve_path(&cwd, &given);
    unresolved_link(&root, &given)?;
    let mut opts = options(walk_args, excludes)?;
    // The walk's setup, before the interrupt watcher starts a thread.
    duscope_walk::prepare();
    // Ctrl-C stops the walk; if it has not stopped soon after, the process
    // ends anyway. Nothing is written before a scan completes, and once the
    // walk is over the result is written whole.
    let interrupts = Interrupts::arm();
    opts.cancel = Arc::clone(&interrupts.flag);
    let started = Instant::now();
    let started_ns = now(walk_args)?;
    let mut arena = Arena::new(opts.attrs);
    let mut push_error = None;
    let mut errors = log::log_enabled!(log::Level::Debug).then(|| DirErrors::new(&root));
    let walked = walk(&root, &opts, |event| {
        if let Some(errors) = &mut errors {
            errors.note(&event);
        }
        if push_error.is_none()
            && let Err(e) = arena.push(event)
        {
            push_error = Some(e);
        }
    });
    let walk_time = started.elapsed();
    drop(interrupts);
    let summary = walked.map_err(|e| walk_failure(&given, e))?;
    if let Some(e) = push_error {
        return Err(Failure::new(Exit::Runtime, "io_error", e.to_string()));
    }
    // A root that is a directory nobody may list is a permission failure
    // (exit 4), one that fails otherwise (EIO, ELOOP) a runtime error; a
    // directory below it that cannot be read is a partial read.
    if let Some(&errno) = arena.unreadable().get(&DirId::ROOT) {
        let error = io::Error::from_raw_os_error(errno);
        return Err(if error.kind() == io::ErrorKind::PermissionDenied {
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
                format!("cannot read {}: {error}", given.display()),
            )
        });
    }
    let rolled = Instant::now();
    let rollup = roll_up(&mut arena);
    let rollup_time = rolled.elapsed();
    let scanned = Scanned {
        arena,
        rollup,
        summary,
        elapsed: started.elapsed(),
        root,
        root_as_given: given,
        clones_asked: walk_args.clones,
        started_ns,
        finished_ns: now(walk_args)?,
    };
    phases(&scanned, walk_time, rollup_time);
    Ok(scanned)
}

/// `-v`: the walk and the roll-up, with counts and rates; `-vv` adds each
/// volume the walk met and its worker threads.
fn phases(scanned: &Scanned, walk_time: Duration, rollup_time: Duration) {
    if !log::log_enabled!(log::Level::Info) {
        return;
    }
    let summary = &scanned.summary;
    let bytes = scanned.view().node(0).allocated;
    let secs = walk_time.as_secs_f64();
    // Whole numbers per second, over at least a microsecond.
    let rate = |n: u64| {
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss
        )]
        let per_second = (n as f64 / secs.max(1e-6)) as u64;
        thousands(per_second)
    };
    log::info!(
        "walk: {} entries, {} directories, {} MB in {secs:.3} s: {} entries/s, {} MB/s",
        thousands(summary.entries),
        thousands(summary.dirs),
        thousands(bytes / 1_000_000),
        rate(summary.entries),
        rate(bytes / 1_000_000),
    );
    log::info!("roll-up: {:.3} s", rollup_time.as_secs_f64());
    for volume in scanned.arena.volumes() {
        let entered = match volume.reason {
            None => "entered".to_owned(),
            Some(reason) => format!("not entered ({})", reason.as_str()),
        };
        log::debug!(
            target: crate::log::DETAIL,
            "volume {}: {}, {entered}, {} MB, {} MB free",
            volume.mount_point.display(),
            volume.fs_type,
            thousands(volume.total_bytes / 1_000_000),
            thousands(volume.free_bytes / 1_000_000),
        );
    }
    log::debug!(target: crate::log::DETAIL, "workers: {} threads", summary.threads);
}

/// `--debug`: each directory's errors, logged as the walk delivers them,
/// with the path spelled from the directories met so far.
struct DirErrors {
    root: Vec<u8>,
    dirs: HashMap<DirId, (Vec<u8>, DirId)>,
}

impl DirErrors {
    fn new(root: &Path) -> DirErrors {
        DirErrors {
            root: root.as_os_str().as_bytes().to_vec(),
            dirs: HashMap::new(),
        }
    }

    /// Records a directory, or logs an error, a vanished entry or a skip.
    fn note(&mut self, event: &ScanEvent) {
        match event {
            ScanEvent::Entry(entry) => {
                if let Some(dir) = entry.dir
                    && dir != DirId::ROOT
                {
                    self.dirs
                        .insert(dir, (entry.name.as_bytes().to_vec(), entry.parent));
                }
            }
            ScanEvent::DirClosed {
                dir,
                errno: Some(errno),
            } => log::debug!(
                "{}: {}",
                self.path(*dir, None),
                io::Error::from_raw_os_error(*errno)
            ),
            ScanEvent::Error(error) => log::debug!(
                "{}: {}",
                self.path(error.dir, Some(error.name.as_bytes())),
                io::Error::from_raw_os_error(error.errno)
            ),
            ScanEvent::Vanished { dir, name } => log::debug!(
                "{}: vanished during the scan",
                self.path(*dir, name.as_ref().map(NameRef::as_bytes))
            ),
            _ => {}
        }
    }

    /// The path of `dir`, and of `name` in it.
    fn path(&self, dir: DirId, name: Option<&[u8]>) -> String {
        let mut names: Vec<&[u8]> = name.into_iter().collect();
        let mut at = dir;
        while at != DirId::ROOT {
            let Some((name, parent)) = self.dirs.get(&at) else {
                break;
            };
            names.push(name);
            at = *parent;
        }
        let mut path = self.root.clone();
        for name in names.iter().rev() {
            path.push(b'/');
            path.extend_from_slice(name);
        }
        String::from_utf8_lossy(&path).into_owned()
    }
}

/// The walk options the flags ask for.
fn options(args: &WalkArgs, excludes: &[String]) -> Result<WalkOptions, Failure> {
    let mut globs = GlobSetBuilder::new();
    // Patterns ending in `/`, which leave out directories only, go to the
    // gitignore rules, which tell a directory from a file.
    let mut dirs_only = String::new();
    let mut add = |pattern: &str, flag: &str| -> Result<(), Failure> {
        match exclude_pattern(pattern, flag)? {
            Exclude::Glob(glob) => {
                globs.add(glob);
            }
            Exclude::DirsOnly(line) => {
                dirs_only.push_str(&line);
                dirs_only.push('\n');
            }
        }
        Ok(())
    };
    for pattern in excludes {
        add(pattern, "--exclude")?;
    }
    for pattern in &args.config_excludes {
        add(pattern, "the configuration's [excludes]")?;
    }
    for file in &args.exclude_from {
        let text = read_flag_file(file, "--exclude-from")?;
        // One pattern a line, CRLF or LF; blank lines, `#` comments and the
        // blanks around a pattern are not part of any pattern.
        for line in String::from_utf8_lossy(&text).lines() {
            let line = line.trim();
            if !line.is_empty() && !line.starts_with('#') {
                add(line, "--exclude-from")?;
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
    // Last, so no negation in an ignore file takes them back.
    if !dirs_only.is_empty() {
        ignore
            .add(dirs_only.as_bytes(), "--exclude")
            .map_err(|e| Failure::new(Exit::Usage, "usage", format!("--exclude: {e}")))?;
    }
    let exclude_caches = if args.exclude_caches {
        caches(args)?.compile()
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

/// The cache rules: the vendored ones, with the configuration's
/// `[caches] extra` table merged over them.
fn caches(args: &WalkArgs) -> Result<Caches, Failure> {
    let Some(file) = &args.caches_extra else {
        return Ok(Caches::vendored());
    };
    let refused = |message: String| {
        Failure::new(Exit::Usage, "usage", format!("[caches] extra {message}"))
            .hint("Fix the configuration file, or run duscope with --config none")
    };
    let text =
        std::fs::read_to_string(file).map_err(|e| refused(format!("{}: {e}", file.display())))?;
    let user =
        Caches::parse(&text, &file.display().to_string()).map_err(|e| refused(e.to_string()))?;
    Ok(Caches::vendored().merged(user))
}

/// An `--exclude` pattern as the walk matches it.
enum Exclude {
    /// A glob of paths, directories and files alike.
    Glob(Glob),
    /// A pattern that ended in `/`, as a gitignore line that leaves out
    /// directories only.
    DirsOnly(String),
}

/// An `--exclude` pattern as the walk matches it, against paths relative to
/// the root: a pattern starting with `/` or `./` is anchored at the root;
/// any other matches at every depth (`*.tmp` matches `a/b/c.tmp`). `*` stays
/// within a path component and `**` crosses them. A trailing `/` leaves out
/// directories only (`build/` leaves out a directory named build, not a
/// file of that name), as in gitignore. A pattern naming nothing below the
/// root (empty, `/` or `.`) is a usage error.
fn exclude_pattern(pattern: &str, flag: &str) -> Result<Exclude, Failure> {
    let dirs_only = pattern.ends_with('/');
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
    if dirs_only {
        // The same anchoring in gitignore's terms; the prefix also keeps a
        // leading `!` or `#` from reading as a negation or a comment.
        let prefix = if anchored { "/" } else { "**/" };
        return Ok(Exclude::DirsOnly(format!("{prefix}{rest}/")));
    }
    let glob = if anchored {
        rest.to_owned()
    } else {
        format!("**/{rest}")
    };
    GlobBuilder::new(&glob)
        .literal_separator(true)
        .build()
        .map(Exclude::Glob)
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
        WalkError::Root { source, .. } => match source.kind() {
            io::ErrorKind::NotFound | io::ErrorKind::NotADirectory => Failure::new(
                Exit::NotFound,
                "scan_not_found",
                format!("{} does not exist", root.display()),
            )
            .hint("Check the path, or run duscope scan in the directory to measure"),
            io::ErrorKind::PermissionDenied => Failure::new(
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

/// Ctrl-C during a walk. While armed, SIGINT sets `flag`, the walk's
/// cancellation flag, and a watcher thread reports it and ends the process
/// with exit 130 at once. A scan writes nothing until its walk is over, so
/// there is nothing to finish first, and waiting for the walk to wind down
/// took over 200 ms on machines whose timers overshoot. The watcher blocks
/// until SIGINT or the stand-down, without polling. Dropping the guard once
/// the walk is over ends the watcher; the two share a lock, so a Ctrl-C
/// while the result is written lets it finish rather than cut it off from
/// another thread. Where SIGINT was inherited ignored (a background job),
/// or the watcher cannot be started, Ctrl-C keeps its usual effect and the
/// flag is never set.
struct Interrupts {
    flag: Arc<AtomicBool>,
    /// Set once the walk is over, under the lock the watcher exits under.
    over: Arc<Mutex<bool>>,
    armed: bool,
}

impl Interrupts {
    fn arm() -> Interrupts {
        let flag = Arc::new(AtomicBool::new(false));
        let over = Arc::new(Mutex::new(false));
        let armed = matches!(duscope_walk::cancel_on_interrupt(&flag), Ok(true));
        if armed {
            let walk_over = Arc::clone(&over);
            let watcher = thread::Builder::new()
                .name("interrupt".to_owned())
                .spawn(move || {
                    if duscope_walk::wait_for_interrupt() {
                        // Held until the exit, so the walk cannot end and its
                        // result start between this check and the exit.
                        let over = walk_over.lock().unwrap_or_else(PoisonError::into_inner);
                        if !*over {
                            report_interrupt();
                            std::process::exit(i32::from(Exit::Interrupted.code()));
                        }
                    }
                });
            if watcher.is_err() {
                duscope_walk::stand_down_interrupt();
                return Interrupts {
                    flag,
                    over,
                    armed: false,
                };
            }
        }
        Interrupts { flag, over, armed }
    }
}

impl Drop for Interrupts {
    fn drop(&mut self) {
        if self.armed {
            *self.over.lock().unwrap_or_else(PoisonError::into_inner) = true;
            duscope_walk::stand_down_interrupt();
        }
    }
}

/// Reports Ctrl-C: one line on stderr, once, whatever the format.
pub fn report_interrupt() {
    static REPORTED: AtomicBool = AtomicBool::new(false);
    if !REPORTED.swap(true, Ordering::SeqCst) {
        crate::out::stderr_line("duscope: interrupted");
    }
}

/// The failure of a root that is still a symbolic link once resolved: one
/// that leads nowhere (exit 3) or into a loop (a runtime error), where
/// scanning the link itself would report a few bytes as a success.
fn unresolved_link(root: &Path, given: &Path) -> Result<(), Failure> {
    let is_link = std::fs::symlink_metadata(root).is_ok_and(|m| m.file_type().is_symlink());
    match std::fs::metadata(root) {
        Err(e) if is_link && e.kind() == io::ErrorKind::NotFound => Err(Failure::new(
            Exit::NotFound,
            "scan_not_found",
            format!("{} is a symbolic link to nothing", given.display()),
        )
        .hint("Check where the link points, or scan the directory it should lead to")),
        Err(e) if is_link => Err(Failure::new(
            Exit::Runtime,
            "io_error",
            format!("cannot follow {}: {e}", given.display()),
        )),
        _ => Ok(()),
    }
}

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
    use duscope_core::query::SizeKind;
    use duscope_core::rollup::roll_up;
    use duscope_walk::{Attrs, MemProvider, ScanEvent, WalkOptions, walk_with};

    use super::{Scanned, scan_block, warnings};
    use crate::cli::{Cli, Verb};
    use crate::out::{Format, Output, Units};
    use crate::verbs::query::top_envelope;

    /// A scan of a small in-memory tree; without its volume when `volume`
    /// is false.
    fn scanned(volume: bool) -> Scanned {
        let mut tree = MemProvider::new("/t");
        tree.file("a", 10);
        let mut arena = Arena::new(Attrs::Basic);
        let summary = walk_with(&tree, Path::new("/t"), &WalkOptions::default(), |event| {
            if volume || !matches!(event, ScanEvent::Volume(_)) {
                arena.push(event).unwrap();
            }
        })
        .unwrap();
        let rollup = roll_up(&mut arena);
        Scanned {
            arena,
            rollup,
            summary,
            elapsed: Duration::ZERO,
            root: "/t".into(),
            root_as_given: "/t".into(),
            clones_asked: false,
            started_ns: 0,
            finished_ns: 0,
        }
    }

    #[test]
    fn p1_cli_02_fs_type_is_null_without_a_volume() {
        let Some(Verb::Scan(args)) = Cli::try_parse_from(["duscope", "scan", "/t"]).unwrap().verb
        else {
            panic!("not scan");
        };
        let block = |scanned: &Scanned| {
            serde_json::to_value(scan_block(scanned, &args.walk, SizeKind::Allocated)).unwrap()
        };
        assert_eq!(block(&scanned(true))["fs_type"], "mem");
        assert_eq!(block(&scanned(false))["fs_type"], serde_json::Value::Null);
    }

    #[test]
    fn p1_walk_07_clones_unavailable_warns_only_when_clones_were_asked() {
        let codes =
            |scanned: &Scanned| warnings(scanned).iter().map(|w| w.code).collect::<Vec<_>>();
        let mut scanned = scanned(true);
        scanned.summary.clones_unavailable = true;
        // Clones on by default, on a filesystem without clone ids: quiet.
        assert!(codes(&scanned).is_empty());
        scanned.clones_asked = true;
        assert_eq!(codes(&scanned), ["clones_unavailable"]);
        let warning = &warnings(&scanned)[0];
        assert!(warning.message.contains("--clones"), "{}", warning.message);
        assert!(warning.hint.is_some());
        scanned.summary.clones_unavailable = false;
        assert!(codes(&scanned).is_empty());
    }

    #[test]
    fn p1_walk_08_a_vanished_file_increments_vanished_count() {
        /// `ENOENT`, 2 on Linux and macOS alike.
        const ENOENT: i32 = 2;
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
            clones_asked: false,
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
            quiet: false,
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
