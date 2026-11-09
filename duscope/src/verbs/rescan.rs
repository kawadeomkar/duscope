//! `rescan`: the saved scan that holds the path (or the one `--scan`
//! names) scanned again, at its root, with the options it was scanned
//! with. The new scan is saved as a new index whose `parent_scan_id` names
//! the old one, which stays until `scans --prune`; `latest` then reads the
//! new one.
//!
//! By default a rescan reads again only what changed, where the platform
//! keeps a change history (macOS's `FSEvents`) and every check passes
//! (`duscope_core::rescan::plan`); a failed check reads the whole tree and
//! says why in `fallback_reason`. `--full` always reads the whole tree, and
//! `--incremental` fails where a full read would be needed. Linux keeps no
//! such history: every rescan there reads the whole tree, and `diff`
//! compares it with its parent.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use duscope_core::envelope::TopEntry;
use duscope_core::index::{Index, ScanId, index_path};
use duscope_core::query::{
    ScanQuery, ScanSelector, TreeView, clock, resolve_scan, short_id, timegrammar,
};
use duscope_core::rescan::{self, Checks, Fallback, FsEvents, Stop};
use duscope_core::resolve::resolve_path;
use duscope_walk::{ScanEvent, WalkOptions};

use super::next;
use super::query::top_envelope;
use super::scan::{self, Scanned};
use super::source::{Source, core_failure, scanned};
use super::{Exit, Failure};
use crate::cli::{AttrsArg, RescanArgs, SizeKind, WalkArgs};
use crate::out::{Output, thousands};

/// The saved scan a rescan renews: its id, root and options, and its
/// index, which an incremental rescan reads.
struct Parent {
    id: ScanId,
    root: PathBuf,
    options: rescan::ScanOptions,
    attrs: duscope_walk::Attrs,
    index: Index,
}

/// What the rescan was asked to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Only what changed where it can, else the whole tree.
    Auto,
    /// The whole tree (`--full`).
    Full,
    /// Only what changed, or nothing (`--incremental`).
    Incremental,
}

/// `rescan`.
pub fn run(args: &RescanArgs, index_dir: Option<&Path>, output: &Output) -> Result<(), Failure> {
    let mode = if args.full {
        Mode::Full
    } else if args.incremental {
        Mode::Incremental
    } else {
        Mode::Auto
    };
    // A wrong --fields or --max-age costs no scan.
    if output.fields_first::<TopEntry>("rescan")? {
        return Ok(());
    }
    let max_age_ns = match &args.max_age {
        Some(text) => timegrammar::parse_age(text)
            .map_err(|e| Failure::new(Exit::Usage, "usage", format!("--max-age {e}")))?,
        None => rescan::MAX_AGE_NS,
    };
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
    let walk = walk_args(&parent);
    let excludes = parent.options.excludes.clone();
    // From the checks and the replays to the roll-up: what the footer's
    // incremental rescan took.
    let began = Instant::now();
    let (mut scanned, fallback) = match mode {
        Mode::Full => (scan::scan(&walk, &excludes)?, None),
        Mode::Auto | Mode::Incremental => match incremental(&parent, &walk, &excludes, max_age_ns)?
        {
            Ok(scanned) => (scanned, None),
            Err(stop) => {
                let fallback = refused(mode, stop, &parent)?;
                (scan::scan(&walk, &excludes)?, fallback)
            }
        },
    };
    let took = began.elapsed();
    scanned.parent = Some(parent.id.clone());
    scanned.fallback = fallback.map(Fallback::as_str);
    let id = scan::scan_id(&scanned)?;
    let read_again = scanned.incremental;
    let fresh = Source::Fresh {
        scanned: Box::new(scanned),
        save: None,
    };
    let (mut envelope, footer, fresh) =
        top_envelope(fresh, &walk, None, "rescan", false, true, None, output)?;
    envelope.scan.id = Some(id.to_string());
    let file = index_path(&dir, &id);
    envelope.next = next::list(&envelope, Some(&file), None);
    let how = how(read_again, envelope.scan.dir_count, took, fallback);
    let footer = format!(
        "{footer}{how} · rescan of saved scan {}",
        short(&dir, &parent.id, &id)
    );
    output.list(&envelope, &footer)?;
    let Source::Fresh { scanned, .. } = fresh else {
        return Ok(());
    };
    scan::save(&scanned, &walk, &excludes, &id, Some(&dir), output, true)
}

/// What the footer says of how the rescan read the tree: `rescanned 1,204
/// of 143,882 directories in 0.9 s (FSEvents)` for an incremental one that
/// read `read_again` of `dirs` directories in `took`, `full rescan: options
/// changed` for one that fell back, nothing for a full one asked for or the
/// only kind the platform has.
fn how(read_again: Option<u64>, dirs: u64, took: Duration, fallback: Option<Fallback>) -> String {
    match (read_again, fallback) {
        (Some(read), _) => format!(
            " · rescanned {} of {} directories in {:.1} s (FSEvents)",
            thousands(read),
            thousands(dirs),
            took.as_secs_f64()
        ),
        (None, Some(reason)) => format!(" · full rescan: {}", reason.words()),
        (None, None) => String::new(),
    }
}

/// The walk's flags for a rescan of `parent`: its saved options, no
/// configuration's.
fn walk_args(parent: &Parent) -> WalkArgs {
    let saved = &parent.options;
    WalkArgs {
        path: Some(parent.root.clone()),
        threads: (saved.threads > 0).then_some(saved.threads),
        one_file_system: saved.one_file_system,
        include_network: saved.include_network,
        follow_symlinks: saved.follow_symlinks,
        size: Some(if saved.size_kind == "apparent" {
            SizeKind::Apparent
        } else {
            SizeKind::Allocated
        }),
        apparent_size: false,
        attrs: match parent.attrs {
            duscope_walk::Attrs::Full => AttrsArg::Full,
            duscope_walk::Attrs::Basic => AttrsArg::Basic,
        },
        // Clones as the scan counted them, by default rather than asked.
        clones: false,
        no_clones: !saved.clones,
        exclude_from: saved.exclude_files.iter().map(PathBuf::from).collect(),
        ignore_from: saved.ignore_files.iter().map(PathBuf::from).collect(),
        exclude_caches: saved.exclude_caches,
        skip_containers: saved.skip_containers,
        provider: None,
        test_now: None,
        // The saved patterns hold the configuration's of the time; today's
        // are not the saved scan's.
        config_excludes: Vec::new(),
        classes_extra: None,
        caches_extra: saved.caches_extra.as_ref().map(PathBuf::from),
    }
}

/// The incremental rescan of `parent`: the scan, with the directories it
/// read again, or why it could not be one.
fn incremental(
    parent: &Parent,
    walk: &WalkArgs,
    excludes: &[String],
    max_age_ns: i64,
) -> Result<Result<Scanned, Stop>, Failure> {
    let checks = Checks {
        now_ns: scan::now(walk)?,
        max_age_ns,
        tool_version: env!("CARGO_PKG_VERSION"),
    };
    let history = FsEvents;
    let plan = match rescan::plan(&parent.index, &checks, &history) {
        Ok(plan) => plan,
        Err(stop) => return Ok(Err(stop)),
    };
    log::info!(
        "incremental rescan: {} of the saved scan's {} directories changed",
        thousands(plan.reported() as u64),
        thousands(plan.saved_dirs() as u64)
    );
    let mut read_again = 0;
    let read = |root: &Path, opts: &WalkOptions, sink: &mut dyn FnMut(ScanEvent)| {
        let provider = match duscope_walk::provider(opts) {
            Ok(provider) => provider,
            Err(e) => return Ok(Err(e)),
        };
        match rescan::merge(&plan, &parent.index, root, opts, &*provider, &history, sink) {
            Ok(merged) => {
                read_again = merged.rescanned_dirs;
                Ok(Ok(merged.summary))
            }
            Err(Stop::Walk(e)) => Ok(Err(e)),
            Err(stop) => Err(stop),
        }
    };
    let scanned = scan::scan_by(walk, excludes, read)?;
    Ok(scanned.map(|mut scanned| {
        scanned.incremental = Some(read_again);
        scanned
    }))
}

/// What a rescan that could not read only what changed does instead:
/// under `--incremental`, fail; else read the whole tree, with the reason
/// it gives (none where the platform keeps no history to read).
fn refused(mode: Mode, stop: Stop, parent: &Parent) -> Result<Option<Fallback>, Failure> {
    match (stop, mode) {
        (Stop::Index(e), _) => Err(core_failure(e)),
        (Stop::Walk(e), _) => Err(Failure::new(Exit::Runtime, "io_error", e.to_string())),
        (Stop::NoHistory, Mode::Incremental) => Err(Failure::new(
            Exit::Usage,
            "not_available",
            "this platform keeps no change history to read only what changed: every rescan \
             reads the whole tree",
        )
        .hint(format!(
            "Run duscope rescan without --incremental to scan the whole tree, then duscope diff {} \
             latest --under {} to see what changed",
            parent.id,
            parent.root.display()
        ))),
        (Stop::NoHistory, _) => Ok(None),
        (Stop::Fallback(reason, detail), Mode::Incremental) => Err(Failure::new(
            Exit::Usage,
            "not_available",
            format!(
                "an incremental rescan of saved scan {} is not possible ({}): {detail}",
                parent.id,
                reason.as_str()
            ),
        )
        .hint("Run duscope rescan without --incremental to scan the whole tree again")),
        (Stop::Fallback(reason, detail), _) => {
            log::info!("full rescan ({}): {detail}", reason.as_str());
            Ok(Some(reason))
        }
    }
}

/// The shortest prefix of `parent` no other saved scan shares, the new
/// one (`child`, not yet written) included.
fn short(dir: &Path, parent: &ScanId, child: &ScanId) -> String {
    let short = short_id(dir, parent.as_str());
    let shared = parent
        .as_str()
        .bytes()
        .zip(child.as_str().bytes())
        .take_while(|(a, b)| a == b)
        .count();
    let len = short.len().max(shared + 1).min(parent.as_str().len());
    parent.as_str()[..len].to_owned()
}

/// The saved scan to scan again: the one `--scan` names (which must hold
/// the path, when one is given), or the newest that holds the path (the
/// working directory by default).
fn parent(args: &RescanArgs, dir: &Path) -> Result<Parent, Failure> {
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
    let q = ScanQuery {
        index_dir: dir,
        path: (matches!(selector, ScanSelector::Latest) || args.path.is_some())
            .then_some(path.as_path()),
        cwd: &cwd,
        // An old scan is what a rescan renews: none is too old for it.
        max_age_ns: i64::MAX,
        now: clock::now(),
        trust: &args.trust,
    };
    let resolved = resolve_scan(&selector, &q, scanned).map_err(core_failure)?;
    let index = resolved.map(|r| r.index).ok_or_else(|| {
        Failure::new(
            Exit::NotFound,
            "scan_not_found",
            format!("no saved scan holds {}", path.display()),
        )
        .hint(format!("Run duscope scan --save {} first", path.display()))
    })?;
    // An imported tree has no scan options to scan with.
    if !scanned(&index) {
        let source = &index.meta().source;
        return Err(Failure::new(
            Exit::Usage,
            "usage",
            format!(
                "saved scan {} is an import ({source}), which has no scan options to scan with again",
                index.meta().scan_id
            ),
        )
        .hint(format!(
            "Run duscope scan --save {} to scan the tree here",
            index.root_path().display()
        )));
    }
    let (options, attrs) = rescan::options(&index).map_err(core_failure)?;
    Ok(Parent {
        id: index.meta().scan_id.clone(),
        root: index.root_path().to_owned(),
        options,
        attrs,
        index,
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use duscope_core::rescan::Fallback;

    use super::how;

    #[test]
    fn p5_core_01_the_footer_says_how_the_tree_was_read() {
        assert_eq!(
            how(Some(1204), 143_882, Duration::from_millis(940), None),
            " · rescanned 1,204 of 143,882 directories in 0.9 s (FSEvents)"
        );
        assert_eq!(
            how(None, 10, Duration::ZERO, Some(Fallback::OptionsChanged)),
            " · full rescan: options changed"
        );
        assert_eq!(how(None, 10, Duration::ZERO, None), "");
    }
}
