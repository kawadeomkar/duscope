//! `rescan`: the saved scan that holds the path (or the one `--scan`
//! names) scanned again, at its root, with the options it was scanned
//! with. The new scan is saved as a new index whose `parent_scan_id` names
//! the old one, which stays until `scans --prune`; `latest` then reads the
//! new one. Every rescan is a full one until phase 5 brings `--incremental`.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use duscope_core::index::{Index, index_path};
use duscope_core::query::{ScanQuery, ScanSelector, clock, resolve_scan, short_id};
use duscope_core::rescan;
use duscope_core::resolve::resolve_path;

use super::next;
use super::query::top_envelope;
use super::scan;
use super::source::{Source, core_failure};
use super::{Exit, Failure};
use crate::cli::{AttrsArg, RescanArgs, SizeKind, WalkArgs};
use crate::out::Output;

/// `rescan`.
pub fn run(args: &RescanArgs, index_dir: Option<&Path>, output: &Output) -> Result<(), Failure> {
    if args.incremental {
        return Err(Failure::new(
            Exit::Usage,
            "not_available",
            "--incremental arrives with the FSEvents rescan (P5-CORE-01); every rescan is a full one until then",
        )
        .hint("Run duscope rescan without --incremental"));
    }
    let dir = index_dir
        .map(Path::to_owned)
        .or_else(|| duscope_core::index::default_dir(|name| std::env::var_os(name)))
        .ok_or_else(|| {
            Failure::new(
                Exit::NotFound,
                "scan_not_found",
                "no index directory to look for the saved scan in: neither HOME nor XDG_CACHE_HOME is set",
            )
            .hint("Run duscope rescan with --index-dir naming the directory the scans are in")
        })?;
    let parent = parent(args, &dir)?;
    let saved = rescan::options(&parent).map_err(core_failure)?;
    let root = PathBuf::from(OsStr::from_bytes(&parent.meta().root));
    let walk = WalkArgs {
        path: Some(root),
        threads: (saved.threads > 0).then_some(saved.threads),
        one_file_system: saved.one_file_system,
        include_network: saved.include_network,
        follow_symlinks: saved.follow_symlinks,
        size: Some(if saved.apparent {
            SizeKind::Apparent
        } else {
            SizeKind::Allocated
        }),
        apparent_size: false,
        attrs: match saved.attrs {
            duscope_walk::Attrs::Full => AttrsArg::Full,
            duscope_walk::Attrs::Basic => AttrsArg::Basic,
        },
        // Clones as the scan counted them, by default rather than asked.
        clones: false,
        no_clones: !saved.clones,
        exclude_from: saved.exclude_files,
        ignore_from: saved.ignore_files,
        exclude_caches: saved.exclude_caches,
        skip_containers: false,
        provider: None,
        test_now: None,
        // The saved patterns hold the configuration's of the time; today's
        // are not the saved scan's.
        config_excludes: Vec::new(),
        classes_extra: None,
        caches_extra: None,
    };
    let excludes = saved.excludes;
    let mut scanned = scan::scan(&walk, &excludes)?;
    let parent_id = parent.meta().scan_id.clone();
    scanned.parent = Some(parent_id.clone());
    let id = scan::scan_id(&scanned)?;
    let fresh = Source::Fresh {
        scanned: Box::new(scanned),
        save: None,
    };
    let (mut envelope, footer, fresh) =
        top_envelope(fresh, &walk, None, "rescan", false, true, None, output)?;
    envelope.scan.id = Some(id.to_string());
    let file = index_path(&dir, &id);
    envelope.next = next::list(&envelope, Some(&file), None);
    let footer = format!(
        "{footer} · rescan of saved scan {}",
        short_id(&dir, parent_id.as_str())
    );
    output.list(&envelope, &footer)?;
    match fresh {
        Source::Fresh { scanned, .. } => {
            scan::save(&scanned, &walk, &excludes, &id, Some(&dir), output, true)
        }
        Source::Saved { .. } => Ok(()),
    }
}

/// The saved scan to scan again: the one `--scan` names, or the newest
/// that holds the path (the working directory by default).
fn parent(args: &RescanArgs, dir: &Path) -> Result<Index, Failure> {
    let cwd = std::env::current_dir().map_err(|e| {
        Failure::new(
            Exit::Runtime,
            "io_error",
            format!("cannot read the working directory: {e}"),
        )
    })?;
    let path = args
        .path
        .as_deref()
        .map_or_else(|| cwd.clone(), |p| resolve_path(&cwd, p).real);
    let selector = match &args.scan {
        Some(text) => ScanSelector::parse(text).map_err(core_failure)?,
        None => ScanSelector::Latest,
    };
    // The path picks `latest`; a named scan is rescanned at its root.
    let q = ScanQuery {
        index_dir: dir,
        path: matches!(selector, ScanSelector::Latest).then_some(path.as_path()),
        cwd: &cwd,
        // An old scan is what a rescan renews: none is too old for it.
        max_age_ns: i64::MAX,
        now: clock::now(),
    };
    let resolved = resolve_scan(&selector, &q, |_| true).map_err(core_failure)?;
    resolved.map(|r| r.index).ok_or_else(|| {
        Failure::new(
            Exit::NotFound,
            "scan_not_found",
            format!("no saved scan holds {}", path.display()),
        )
        .hint(format!("Run duscope scan --save {} first", path.display()))
    })
}
