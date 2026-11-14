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

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use duscope_core::envelope::TopEntry;
use duscope_core::index::{Index, ScanId, index_path};
use duscope_core::query::{
    ScanQuery, ScanSelector, TreeView, clock, resolve_scan, short_id, timegrammar,
};
use duscope_core::rescan::{self, Checks, Fallback, FsEvents, ScanOptions, Stop};
use duscope_core::resolve::resolve_path;
use duscope_walk::{ScanEvent, Skip, WalkOptions};

use super::next;
use super::query::top_envelope;
use super::scan::{self, Scanned};
use super::source::{Source, core_failure, saved_as, scanned};
use super::treemap::shell_quote;
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
    let (scanned, fallback, took) = read_again(&parent, &walk, &excludes, mode, max_age_ns)?;
    let id = scan::scan_id(&scanned)?;
    let read_again = scanned.incremental;
    let fresh = Source::Fresh {
        scanned: Box::new(scanned),
        save: None,
    };
    let (mut envelope, footer, fresh) =
        top_envelope(fresh, &walk, None, "rescan", false, true, None, output)?;
    saved_as(&mut envelope, id.to_string());
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
    // --index-dir also says where a rescan looks for the saved scan, so the
    // hint names it by its file, which is read wherever the new one goes.
    scan::save(&scanned, &walk, &excludes, &id, Some(&dir), output, true).map_err(|failure| {
        let file = shell_quote(&parent.index.file().to_string_lossy());
        scan::save_hint(
            failure,
            format!(
                "Run duscope rescan --trust {file} --scan {file} with --index-dir naming a \
                 writable directory"
            ),
        )
    })
}

/// Reads the tree of `parent` again as `mode` asks, the `parent` and
/// fallback recorded on the scan: its new scan, the fallback it took and how
/// long the checks, the replays and the roll-up took (the footer's
/// incremental rescan).
fn read_again(
    parent: &Parent,
    walk: &WalkArgs,
    excludes: &[String],
    mode: Mode,
    max_age_ns: i64,
) -> Result<(Scanned, Option<Fallback>, Duration), Failure> {
    let began = Instant::now();
    let (mut scanned, fallback) = match mode {
        Mode::Full => (scan::scan(walk, excludes)?, None),
        Mode::Auto | Mode::Incremental => match incremental(parent, walk, excludes, max_age_ns)? {
            Ok(scanned) => (scanned, None),
            Err(stop) => {
                let fallback = refused(mode, stop, parent)?;
                (scan::scan(walk, excludes)?, fallback)
            }
        },
    };
    let took = began.elapsed();
    scanned.parent = Some(parent.id.clone());
    scanned.fallback = fallback.map(Fallback::as_str);
    Ok((scanned, fallback, took))
}

/// `r` in the viewer, whose root is `shown`, over the saved scan in `file`,
/// saved under `dir`, without a word on the terminal, which is the viewer's.
/// Where `shown` is a folder below the saved scan's root, it is scanned
/// alone with the saved scan's options and saved as a scan of its own, as
/// `scan --save` of it with those options would be: its parent is no rescan
/// of it, and what lies outside it has not been read, so nothing of the
/// saved scan is carried into the new index. Otherwise the saved scan is
/// read again as `rescan` reads it (only what changed where it can, else
/// the whole tree). The new index's file.
pub fn in_viewer(file: &Path, dir: &Path, shown: &Path) -> Result<PathBuf, Failure> {
    let index = Index::open(file).map_err(core_failure)?;
    let parent = Parent::of(index)?;
    let mut walk = walk_args(&parent);
    let excludes = parent.options.excludes.clone();
    let scanned = match folder_alone(&parent, shown) {
        Some(folder) => {
            walk.path = Some(folder);
            scan::scan(&walk, &excludes)?
        }
        None => read_again(&parent, &walk, &excludes, Mode::Auto, rescan::MAX_AGE_NS)?.0,
    };
    let id = scan::scan_id(&scanned)?;
    scan::write_index(&scanned, &walk, &excludes, &id, dir)?;
    Ok(index_path(dir, &id))
}

/// The folder `r` scans alone for a viewer rooted at `shown` over `parent`:
/// `shown` where it is a directory below the saved scan's root that the scan
/// entered, the walk would read it at that same path (a link followed under
/// `--follow-symlinks` would not be, and the viewer's cursor and marks name
/// that path), the saved options leave out in a scan of it what they left
/// out below the root, and no bind mount the scan did not enter as a cycle
/// lies at or below it. `None` where the saved scan is read again.
///
/// A bind cycle rests on where the walk starts: the walk stops at a mount
/// of a folder already on its path, and a walk that starts below that
/// folder has not met it, so it enters the mount and reads the tree it
/// leads to (`/mnt/x` bound to `/`, which a scan of `/` stops at, is all of
/// `/` again to a walk of `/mnt`). The saved scan does not record which
/// folder a mount binds, so any such mount at or below `shown` sends it to
/// the saved scan.
fn folder_alone(parent: &Parent, shown: &Path) -> Option<PathBuf> {
    let index = &parent.index;
    let below = index.below_root(shown)?;
    if below.is_empty() {
        return None;
    }
    let id = index.lookup(below)?;
    if !index.node(id).is_dir()
        || !index.entered(id)
        || !same_in_a_folder(&parent.options, &parent.root, shown)
        || bind_cycle_below(index, shown)
    {
        return None;
    }
    match std::fs::canonicalize(shown) {
        Ok(real) if real != shown => None,
        // A folder gone since the scan: the walk says so.
        _ => Some(shown.to_owned()),
    }
}

/// Whether a mount the scan in `index` did not enter as a bind cycle lies at
/// or below `folder`.
fn bind_cycle_below(index: &Index, folder: &Path) -> bool {
    let cycle = Skip::BindCycle.as_str();
    index.volumes().iter().any(|volume| {
        volume.reason.as_deref() == Some(cycle)
            && Path::new(OsStr::from_bytes(&volume.mount_point)).starts_with(folder)
    })
}

/// Whether the saved options leave out the same in a scan of `folder` as
/// in the scan of `root` above it. Their patterns are matched against the
/// path relative to the scan's root, so one anchored there (`/build`) or
/// naming more than one component (`proj/build`) would leave out something
/// else; one name at any depth (`node_modules`, `*.tmp`) leaves out the
/// same. A file of patterns that cannot be read is no such file: the walk
/// reports it. The walk's rule for macOS's Data volume rests on where it
/// starts ([`duscope_walk::walk_differs_below`]), the same rule then
/// reading another tree in the folder; so does a bind cycle, which
/// [`folder_alone`] asks of the saved scan's volumes.
fn same_in_a_folder(options: &ScanOptions, root: &Path, folder: &Path) -> bool {
    let walk = WalkOptions {
        one_file_system: options.one_file_system,
        follow_symlinks: options.follow_symlinks,
        ..WalkOptions::default()
    };
    !duscope_walk::walk_differs_below(root, folder, &walk)
        && options.excludes.iter().all(|p| one_name(p))
        && options
            .exclude_files
            .iter()
            .chain(&options.ignore_files)
            .all(|file| std::fs::read(file).is_ok_and(|text| lines_one_name(&text)))
}

/// Whether every pattern of a pattern file, its lines read as the walk reads
/// them, is [`one_name`]; a gitignore negation is judged by what it names.
fn lines_one_name(text: &[u8]) -> bool {
    String::from_utf8_lossy(text)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .all(|line| one_name(line.strip_prefix('!').unwrap_or(line)))
}

/// Whether `pattern` matches one name at any depth: not anchored at the
/// root, and a single component once its leading `**/` and trailing `/` are
/// set aside.
fn one_name(pattern: &str) -> bool {
    let trimmed = pattern.trim_end_matches('/');
    if trimmed.starts_with('/') || trimmed.starts_with("./") {
        return false;
    }
    let mut rest = trimmed;
    while let Some(after) = rest.strip_prefix("**/") {
        rest = after;
    }
    !rest.contains('/')
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
    Parent::of(index)
}

impl Parent {
    /// The saved scan `index` as a rescan's parent: its id, root and
    /// options. An imported tree has no scan options to scan with.
    fn of(index: Index) -> Result<Parent, Failure> {
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
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use duscope_core::arena::Arena;
    use duscope_core::index::{Index, IndexWriter, ScanId, ScanMeta, index_path};
    use duscope_core::rescan::{Fallback, ScanOptions};
    use duscope_core::rollup::roll_up;
    use duscope_walk::{Attrs, MemProvider, ScanEvent, Skip, WalkOptions, walk_with};

    use super::{Parent, folder_alone, how, lines_one_name, one_name, same_in_a_folder};

    #[test]
    fn p4_tui_06_a_pattern_of_one_name_leaves_out_the_same_in_a_folder() {
        for same in [
            "node_modules",
            "*.tmp",
            "build/",
            "**/target",
            "**/target/",
            "**",
        ] {
            assert!(one_name(same), "{same}");
        }
        for tied in [
            "/build",
            "./build",
            "/build/",
            "proj/build",
            "proj/build/",
            "**/a/b",
            "a/**",
            "docs/*.pdf",
        ] {
            assert!(!one_name(tied), "{tied}");
        }
    }

    #[test]
    fn p4_tui_06_a_pattern_file_is_judged_by_the_lines_the_walk_reads() {
        assert!(lines_one_name(
            b"# a /comment/ with slashes\n\nnode_modules\r\n  *.log  \n!keep.log\n"
        ));
        assert!(!lines_one_name(b"node_modules\n/target\n"));
        assert!(!lines_one_name(b"!/keep\n"));
        assert!(!lines_one_name(b"docs/build\n"));
    }

    /// As a rescan's parent, a saved scan of an in-memory tree at `root`:
    /// `nowhere/proj` and a file `nowhere/f`, `nowhere/skip` left out by an
    /// exclude, `nowhere/nas` a mount the walk did not enter,
    /// `nowhere/bound/loop` a bind mount it did not enter as a cycle, and
    /// `System/Volumes/Data`. Nothing under `nowhere` exists on the disk.
    fn parent(dir: &Path, root: &str) -> Parent {
        let mut tree = MemProvider::new(root);
        tree.file("nowhere/proj/a", 10)
            .file("nowhere/f", 10)
            .file("nowhere/skip/b", 10)
            .dir("nowhere/nas")
            .file("nowhere/bound/c", 10)
            .dir("nowhere/bound/loop")
            .dir("System/Volumes/Data");
        let mut globs = globset::GlobSetBuilder::new();
        globs.add(globset::Glob::new("**/skip").unwrap());
        let opts = WalkOptions {
            excludes: globs.build().unwrap(),
            ..WalkOptions::default()
        };
        let mut arena = Arena::new(Attrs::Basic);
        let mut volume = None;
        walk_with(&tree, tree.root_path(), &opts, |event| {
            if let ScanEvent::Volume(found) = &event {
                volume.get_or_insert_with(|| found.clone());
            }
            arena.push(event).unwrap();
        })
        .unwrap();
        let mut mount: duscope_walk::VolumeInfo = volume.unwrap();
        mount.dev += 1;
        mount.mnt_id += 1;
        mount.mount_point = tree.root_path().join("nowhere/nas");
        mount.reason = Some(Skip::Network);
        arena.push(ScanEvent::Volume(mount.clone())).unwrap();
        mount.dev += 1;
        mount.mnt_id += 1;
        mount.mount_point = tree.root_path().join("nowhere/bound/loop");
        mount.reason = Some(Skip::BindCycle);
        arena.push(ScanEvent::Volume(mount)).unwrap();
        let rollup = roll_up(&mut arena);
        let root = root.as_bytes().to_vec();
        let meta = ScanMeta {
            scan_id: ScanId::new(&root, 1, [0; 8]),
            root: root.clone(),
            root_as_given: root,
            started_at: 1,
            finished_at: 2,
            options: "{}".to_owned(),
            platform: "test".to_owned(),
            scanning_uid: 501,
            thread_count: 4,
            size_kind: "allocated",
            volume_total: 1 << 40,
            volume_free: 1 << 39,
            unaccounted_bytes: None,
            error_count: 0,
            vanished_count: 0,
            excluded_count: 1,
            clones_unavailable: false,
            incremental: false,
            parent_scan_id: None,
            source: "scan".to_owned(),
            tool_version: "0.0.0".to_owned(),
        };
        IndexWriter::write(&arena, &rollup, &meta, dir).unwrap();
        Parent::of(Index::open(&index_path(dir, &meta.scan_id)).unwrap()).unwrap()
    }

    #[test]
    fn p4_tui_06_what_a_scan_of_slash_did_not_enter_goes_to_the_saved_scan() {
        let dir = tempfile::tempdir().unwrap();
        let parent = parent(dir.path(), "/");
        let alone = |path: &str| folder_alone(&parent, Path::new(path));
        assert_eq!(
            alone("/nowhere/proj"),
            Some(PathBuf::from("/nowhere/proj")),
            "a folder the scan entered"
        );
        assert_eq!(alone("/nowhere/nas"), None, "a mount it did not enter");
        assert_eq!(alone("/nowhere/skip"), None, "an exclude's folder");
        assert_eq!(alone("/nowhere/f"), None, "a file");
        assert_eq!(alone("/"), None, "the root itself");
        // The way into the Data volume, as the walk would decide it; only
        // the system's firmlink table is read.
        let system = Path::new("/System/Volumes");
        assert_eq!(
            folder_alone(&parent, system).is_some(),
            !duscope_walk::walk_differs_below(Path::new("/"), system, &WalkOptions::default())
        );
    }

    #[test]
    fn p4_tui_06_a_bind_cycle_at_or_below_a_folder_goes_to_the_saved_scan() {
        let dir = tempfile::tempdir().unwrap();
        let parent = parent(dir.path(), "/");
        let alone = |path: &str| folder_alone(&parent, Path::new(path));
        // `/nowhere/bound/loop` binds a folder above it, so the scan of `/`
        // did not enter it. Which folder the scan does not say, and from a
        // folder below that one it is no cycle: a walk of `/nowhere/bound`
        // or `/nowhere` could read the whole tree through it (`/mnt/x`
        // bound to `/`, under a viewer of `/mnt`).
        assert_eq!(alone("/nowhere/bound"), None, "a folder that holds it");
        assert_eq!(alone("/nowhere"), None, "a folder above that");
        assert_eq!(alone("/nowhere/bound/loop"), None, "the mount itself");
        assert_eq!(
            alone("/nowhere/proj"),
            Some(PathBuf::from("/nowhere/proj")),
            "a folder beside it"
        );
    }

    #[test]
    fn p4_tui_06_what_a_scan_below_slash_did_not_enter_goes_to_the_saved_scan() {
        let dir = tempfile::tempdir().unwrap();
        let parent = parent(dir.path(), "/tree");
        assert_eq!(folder_alone(&parent, Path::new("/tree/nowhere/nas")), None);
    }

    /// Whether the saved options leave a folder to be scanned alone, for
    /// `root` and `folder` that need not exist.
    fn alone_under(options: &ScanOptions, root: &str, folder: &str) -> bool {
        same_in_a_folder(options, Path::new(root), Path::new(folder))
    }

    #[test]
    fn p4_tui_06_the_walks_way_into_the_data_volume_decides_as_the_walk_does() {
        let mut x: ScanOptions = serde_json::from_str("{}").unwrap();
        x.one_file_system = true;
        let mut follow: ScanOptions = serde_json::from_str("{}").unwrap();
        follow.follow_symlinks = true;
        let plain: ScanOptions = serde_json::from_str("{}").unwrap();
        let walk = |options: &ScanOptions| WalkOptions {
            one_file_system: options.one_file_system,
            follow_symlinks: options.follow_symlinks,
            ..WalkOptions::default()
        };
        // Stats the system's firmlink table where it has one; walks nothing.
        for (root, folder) in [
            ("/", "/System"),
            ("/", "/System/Volumes"),
            ("/", "/System/Volumes/Data"),
            ("/", "/System/Library"),
            ("/", "/usr/lib"),
            ("/System", "/System/Library"),
            ("/System", "/System/Volumes"),
        ] {
            for options in [&plain, &x, &follow] {
                assert_eq!(
                    alone_under(options, root, folder),
                    !duscope_walk::walk_differs_below(
                        Path::new(root),
                        Path::new(folder),
                        &walk(options)
                    ),
                    "{root} {folder}"
                );
            }
        }
    }

    #[test]
    fn p4_tui_06_the_saved_options_decide_whether_a_folder_is_scanned_alone() {
        let plain = || -> ScanOptions { serde_json::from_str("{}").unwrap() };
        let same = |options: &ScanOptions| alone_under(options, "/tree", "/tree/proj");
        assert!(same(&plain()));
        let mut names = plain();
        names.excludes = vec!["node_modules".to_owned(), "*.tmp".to_owned()];
        assert!(same(&names));
        let mut anchored = plain();
        anchored.excludes = vec!["node_modules".to_owned(), "/Library".to_owned()];
        assert!(!same(&anchored));

        let dir = tempfile::tempdir().unwrap();
        let of_names = dir.path().join("names");
        std::fs::write(&of_names, "target/\n*.log\n").unwrap();
        let of_paths = dir.path().join("paths");
        std::fs::write(&of_paths, "target/\n/docs/build\n").unwrap();
        let text = |path: &std::path::Path| path.to_str().unwrap().to_owned();
        let mut ignored = plain();
        ignored.ignore_files = vec![text(&of_names)];
        assert!(same(&ignored));
        ignored.ignore_files = vec![text(&of_paths)];
        assert!(!same(&ignored));
        let mut excluded = plain();
        excluded.exclude_files = vec![text(&of_paths)];
        assert!(!same(&excluded));
        // One that cannot be read: the rescan of the saved scan reports it.
        excluded.exclude_files = vec![text(&dir.path().join("gone"))];
        assert!(!same(&excluded));
    }

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
