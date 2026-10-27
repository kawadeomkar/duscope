//! `scan`, and the scan every verb of phase 1 runs: the walk into the
//! arena, then the roll-up.

use std::io::{self, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use duscope_core::arena::Arena;
use duscope_core::rollup::roll_up;
use duscope_walk::{Attrs, DirId, WalkError, WalkOptions, WalkSummary, walk};
use globset::{Glob, GlobSetBuilder};

use super::{Exit, Failure};
use crate::cli::{AttrsArg, ProviderArg, ScanArgs, WalkArgs};

/// A finished scan.
#[derive(Debug)]
pub struct Scanned {
    /// The tree, rolled up.
    pub arena: Arena,
    /// What the walk counted.
    pub summary: WalkSummary,
    /// How long the walk and the roll-up took.
    pub elapsed: Duration,
}

/// `scan`.
pub fn run(args: &ScanArgs) -> Result<(), Failure> {
    if args.save {
        return Err(not_yet("--save", "the index"));
    }
    if args.interactive {
        return Err(not_yet("--interactive", "the interactive viewer"));
    }
    summarize(&args.walk, &args.exclude)
}

/// Scans and prints the one-line summary.
pub fn summarize(walk_args: &WalkArgs, excludes: &[String]) -> Result<(), Failure> {
    let scanned = scan(walk_args, excludes)?;
    let root = scanned.arena.dir(DirId::ROOT);
    let line = format!(
        "{} allocated · {} files · {} dirs · {:.1} s, {} threads",
        iec(root.cum_allocated),
        thousands(u64::from(root.n_files)),
        thousands(u64::from(root.n_dirs)),
        scanned.elapsed.as_secs_f64(),
        scanned.summary.threads,
    );
    write_stdout(&line)
}

/// Scans the tree `walk_args` names.
pub fn scan(walk_args: &WalkArgs, excludes: &[String]) -> Result<Scanned, Failure> {
    let root = match &walk_args.path {
        Some(path) => path.clone(),
        None => std::env::current_dir().map_err(|e| {
            Failure::new(
                Exit::Runtime,
                "io_error",
                format!("cannot read the current directory: {e}"),
            )
        })?,
    };
    let opts = options(walk_args, excludes)?;
    let started = Instant::now();
    let mut arena = Arena::new(opts.attrs);
    let mut push_error = None;
    let summary = walk(&root, &opts, |event| {
        if push_error.is_none()
            && let Err(e) = arena.push(event)
        {
            push_error = Some(e);
        }
    })
    .map_err(|e| walk_failure(&root, e))?;
    if let Some(e) = push_error {
        return Err(Failure::new(Exit::Runtime, "io_error", e.to_string()));
    }
    // A root that is a directory nobody may list is a permission failure
    // (exit 4); a directory below it that cannot be read is a partial read.
    if arena.unreadable().contains_key(&DirId::ROOT) {
        return Err(Failure::new(
            Exit::Permission,
            "permission_denied",
            format!("permission denied reading {}", root.display()),
        )
        .hint("Run duscope scan on a directory you can read"));
    }
    roll_up(&mut arena);
    Ok(Scanned {
        arena,
        summary,
        elapsed: started.elapsed(),
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

/// Writes one line to stdout. A closed pipe (`duscope … | head`) ends the
/// command quietly with success.
pub fn write_stdout(line: &str) -> Result<(), Failure> {
    let mut stdout = io::stdout().lock();
    let written = writeln!(stdout, "{line}").and_then(|()| stdout.flush());
    quiet_on_broken_pipe(written)
}

/// A write's result: success, also when the reader went away.
pub fn quiet_on_broken_pipe(result: io::Result<()>) -> Result<(), Failure> {
    match result {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        Err(e) => Err(Failure::new(
            Exit::Runtime,
            "io_error",
            format!("cannot write the output: {e}"),
        )),
    }
}

/// Bytes in IEC units with one decimal, as tables and footers show them.
pub fn iec(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut unit = 0;
    let mut whole = bytes;
    while whole >= 1024 && unit + 1 < UNITS.len() {
        whole /= 1024;
        unit += 1;
    }
    if unit == 0 {
        return format!("{bytes} B");
    }
    // One decimal from integer arithmetic: tenths of the unit, rounded.
    let divisor = 1u128 << (10 * unit);
    let tenths = (u128::from(bytes) * 10 + divisor / 2) / divisor;
    format!("{}.{} {}", tenths / 10, tenths % 10, UNITS[unit])
}

/// A count with thousands separators.
pub fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn p1_cli_01_a_closed_pipe_is_success() {
        let broken = Err(io::Error::from(io::ErrorKind::BrokenPipe));
        assert_eq!(quiet_on_broken_pipe(broken), Ok(()));
        let other = Err(io::Error::from(io::ErrorKind::PermissionDenied));
        assert_eq!(quiet_on_broken_pipe(other).unwrap_err().exit, Exit::Runtime);
    }

    #[test]
    fn p1_cli_01_sizes_and_counts_read_as_humans_expect() {
        assert_eq!(iec(0), "0 B");
        assert_eq!(iec(1023), "1023 B");
        assert_eq!(iec(1024), "1.0 KiB");
        assert_eq!(iec(1536), "1.5 KiB");
        assert_eq!(iec(412 * (1 << 30) + 300 * (1 << 20)), "412.3 GiB");
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(1_284_310), "1,284,310");
        assert_eq!(thousands(999), "999");
    }
}
