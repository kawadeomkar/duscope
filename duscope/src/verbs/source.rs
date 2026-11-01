//! Where a list verb's rows come from: the newest saved scan that holds
//! the path, read at it, or a scan run for the verb and saved once its
//! output is out (P2-IDX-03).
//!
//! A saved scan is used only when it was taken with the options the verb
//! would scan with (excludes, `-x`, clones, links, and `--attrs full` when
//! asked): one taken otherwise holds another tree. `--scan` names a scan
//! outright, `--no-scan` fails where no saved scan answers, and `--no-save`
//! keeps a scan the verb runs from being saved.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

use duscope_core::CoreError;
use duscope_core::envelope::{ExcludedEntry, ScanBlock, VolumeEntry, Warning};
use duscope_core::index::{Index, ScanId, index_path};
use duscope_core::query::{
    self, ScanQuery, ScanSelector, SizeKind, Subtree, TreeView, resolve_scan, short_id, timegrammar,
};
use duscope_core::resolve::resolve_path;
use duscope_walk::Attrs;

use super::scan::{self, Scanned};
use super::{Exit, Failure};
use crate::cli::{AttrsArg, IndexArgs, WalkArgs};
use crate::out::{Output, iso8601, thousands};

/// `--max-age` when neither the flag nor the configuration sets it.
const DEFAULT_MAX_AGE: &str = "7d";

/// Where a verb's rows come from.
#[derive(Debug)]
pub enum Source {
    /// A scan run for the verb, and the id and index directory it is saved
    /// under once the output is out (`None` under `--no-save`, without an
    /// index directory, or when the caller saves it itself).
    Fresh {
        /// The scan.
        scanned: Box<Scanned>,
        /// Its id and the index directory, when it will be saved.
        save: Option<(ScanId, PathBuf)>,
    },
    /// A saved scan, read at the entry the verb is about.
    Saved {
        /// The scan.
        index: Box<Index>,
        /// The entry the verb is about.
        base: u32,
        /// The directories under it the scan could not read.
        unreadable: u64,
        /// `stale_index`, when the scan is older than `--max-age`.
        stale: Option<Warning>,
    },
}

/// The source `walk`'s path and `index_args` ask for, from the saved scans
/// under `index_dir` (or the default directory).
pub fn source(
    walk: &WalkArgs,
    index_args: &IndexArgs,
    index_dir: Option<&Path>,
) -> Result<Source, Failure> {
    let max_age = index_args.max_age.as_deref().unwrap_or(DEFAULT_MAX_AGE);
    let max_age_ns = timegrammar::parse_age(max_age)
        .map_err(|e| Failure::new(Exit::Usage, "usage", format!("--max-age {e}")))?;
    let selector = match &index_args.scan {
        Some(text) => ScanSelector::parse(text).map_err(core_failure)?,
        None => ScanSelector::Latest,
    };
    let dir = index_dir
        .map(Path::to_owned)
        .or_else(|| duscope_core::index::default_dir(|name| std::env::var_os(name)));
    let now = scan::now(walk)?;
    let cwd = std::env::current_dir().map_err(|e| {
        Failure::new(
            Exit::Runtime,
            "io_error",
            format!("cannot read the working directory: {e}"),
        )
    })?;
    let path = walk.path.as_ref().map(|p| resolve_path(&cwd, p).real);
    // An index file needs no index directory; a prefix does, and latest
    // without one has nothing to find.
    let index_dir = match (&dir, &selector) {
        (Some(dir), _) => Some(dir.as_path()),
        (None, ScanSelector::File(_)) => Some(cwd.as_path()),
        (None, ScanSelector::Prefix(_)) => {
            return Err(Failure::new(
                Exit::NotFound,
                "scan_not_found",
                "no index directory to look for the scan in: neither HOME nor XDG_CACHE_HOME is set",
            )
            .hint("Run duscope with --index-dir naming the directory the scans are in"));
        }
        (None, ScanSelector::Latest) => None,
    };
    if let Some(index_dir) = index_dir {
        let q = ScanQuery {
            index_dir,
            path: path.as_deref(),
            cwd: &cwd,
            max_age_ns,
            now,
        };
        let resolved =
            resolve_scan(&selector, &q, |index| usable(walk, index)).map_err(core_failure)?;
        if let Some(r) = resolved {
            let stale = r.stale.then(|| stale_index(&r.index, max_age));
            let unreadable = r
                .index
                .unreadable_dirs(r.base, r.index.node(r.base).last_id);
            return Ok(Source::Saved {
                index: Box::new(r.index),
                base: r.base,
                unreadable,
                stale,
            });
        }
    }
    if index_args.no_scan {
        let shown = path.as_deref().unwrap_or(&cwd).display().to_string();
        return Err(Failure::new(
            Exit::NotFound,
            "scan_not_found",
            format!("no saved scan holds {shown}, and --no-scan forbids scanning it"),
        )
        .hint(format!(
            "Run duscope scan --save {shown}, or drop --no-scan"
        )));
    }
    let scanned = scan::scan(walk, &[])?;
    let save = match (dir, index_args.no_save) {
        (Some(dir), false) => Some((scan::scan_id(&scanned)?, dir)),
        _ => None,
    };
    Ok(Source::Fresh {
        scanned: Box::new(scanned),
        save,
    })
}

/// Whether `index` holds the tree a scan with `walk`'s options would: the
/// same excludes, filesystems, clone and link handling, and the attributes
/// the verb may ask for. An imported tree, which another tool read, is
/// read only when `--scan` names it.
fn usable(walk: &WalkArgs, index: &Index) -> bool {
    if index.meta().source != "scan" {
        return false;
    }
    let content = |options: &str| {
        let mut value: serde_json::Value = serde_json::from_str(options).unwrap_or_default();
        if let Some(map) = value.as_object_mut() {
            map.remove("threads");
            map.remove("size_kind");
        }
        value
    };
    let attrs = walk.attrs == AttrsArg::Basic || index.attrs() == Attrs::Full;
    attrs && content(&scan::saved_options(walk, &[])) == content(&index.meta().options)
}

/// `stale_index`: the saved scan finished longer ago than `--max-age`.
pub(super) fn stale_index(index: &Index, max_age: &str) -> Warning {
    let meta = index.meta();
    Warning {
        code: "stale_index",
        message: format!(
            "the saved scan {} finished {}, longer ago than --max-age {max_age}",
            meta.scan_id,
            iso8601(meta.finished_at)
        ),
        count: None,
        hint: Some(format!(
            "Run duscope scan --save {} to measure the tree again",
            index.root_path().display()
        )),
    }
}

impl Source {
    /// The index file the scan is in, or will be once saved; `None` for a
    /// scan not to be saved.
    pub fn index_file(&self) -> Option<PathBuf> {
        match self {
            Source::Saved { index, .. } => Some(index.file().to_owned()),
            Source::Fresh {
                save: Some((id, dir)),
                ..
            } => Some(duscope_core::index::index_path(dir, id)),
            Source::Fresh { save: None, .. } => None,
        }
    }

    /// The rows' view: the arena, or the saved scan at the path.
    pub fn view(&self) -> Box<dyn TreeView + '_> {
        match self {
            Source::Fresh { scanned, .. } => Box::new(scanned.view()),
            Source::Saved { index, base, .. } => Box::new(Subtree::new(index.as_ref(), *base)),
        }
    }

    /// Whether the directory at `dir`, above the rows' root, holds a file
    /// `name`: the saved scan's answer where it holds `dir`, else the
    /// filesystem's.
    pub fn holds_file(&self, dir: &Path, name: &str) -> bool {
        match self {
            Source::Fresh { .. } => query::fs_holds_file(dir, name),
            Source::Saved { index, .. } => query::holds_file(index, dir, name),
        }
    }

    /// The id of the saved scan the rows come from, or of the one a scan
    /// run for the verb is saved as; `None` when it is not saved.
    pub fn scan_id(&self) -> Option<String> {
        match self {
            Source::Fresh { save, .. } => save.as_ref().map(|(id, _)| id.to_string()),
            Source::Saved { index, .. } => Some(index.meta().scan_id.to_string()),
        }
    }

    /// When the scan started, Unix nanoseconds.
    pub fn started_ns(&self) -> i64 {
        match self {
            Source::Fresh { scanned, .. } => scanned.started_ns,
            Source::Saved { index, .. } => index.meta().started_at,
        }
    }

    /// The scan an incremental scan updated.
    pub fn parent_scan_id(&self) -> Option<String> {
        match self {
            Source::Fresh { .. } => None,
            Source::Saved { index, .. } => index
                .meta()
                .parent_scan_id
                .as_ref()
                .map(ToString::to_string),
        }
    }

    /// The directories under the path the scan could not read, and the
    /// entries they listed but could not examine.
    pub fn errors_under(&self) -> u64 {
        match self {
            Source::Fresh { scanned, .. } => {
                scanned.summary.unreadable_dirs + scanned.summary.entry_errors
            }
            Source::Saved { index, base, .. } => {
                index.errors_under(*base, index.node(*base).last_id)
            }
        }
    }

    /// Whether the rows' root is the scan's own root.
    pub fn at_root(&self) -> bool {
        match self {
            Source::Fresh { .. } => true,
            Source::Saved { base, .. } => *base == 0,
        }
    }

    /// Directories under the path the scan could not read.
    pub fn unreadable_dirs(&self) -> u64 {
        match self {
            Source::Fresh { scanned, .. } => scanned.summary.unreadable_dirs,
            Source::Saved { unreadable, .. } => *unreadable,
        }
    }

    /// Fails with `index_corrupt` when a read of a saved scan failed after
    /// the file opened: the rows would be missing or empty.
    pub fn check(&self) -> Result<(), Failure> {
        match self {
            Source::Saved { index, .. } => match index.failure() {
                Some(message) => Err(core_failure(CoreError::IndexCorrupt {
                    file: index.file().display().to_string(),
                    message,
                })),
                None => Ok(()),
            },
            Source::Fresh { .. } => Ok(()),
        }
    }

    /// When the scan finished, Unix nanoseconds.
    pub fn finished_ns(&self) -> i64 {
        match self {
            Source::Fresh { scanned, .. } => scanned.finished_ns,
            Source::Saved { index, .. } => index.meta().finished_at,
        }
    }

    /// The scan block: the scan's root, options, times and totals, and its
    /// volumes; a saved scan's id, or the id a fresh one is saved under.
    pub fn scan_block(&self, walk: &WalkArgs, size: SizeKind) -> ScanBlock {
        match self {
            Source::Fresh { scanned, save } => {
                let mut block = scan::scan_block(scanned, walk, size);
                block.id = save.as_ref().map(|(id, _)| id.to_string());
                block
            }
            Source::Saved { index, .. } => saved_block(index, size),
        }
    }

    /// The one line for people: the total under the path, its counts, and
    /// how the scan was had: run now (and the id it is saved as), or read
    /// from a saved scan.
    pub fn footer(&self, block: &ScanBlock, output: &Output) -> String {
        match self {
            Source::Fresh { scanned, save } => {
                let footer = scan::footer(block, scanned, output);
                match save {
                    Some((id, dir)) => format!("{footer}, saved as {}", short_id(dir, id.as_str())),
                    None => footer,
                }
            }
            Source::Saved { index, .. } => {
                let root = self.view().node(0);
                let total = if block.size_kind == "apparent" {
                    root.apparent
                } else {
                    root.allocated
                };
                let dir = index.file().parent().and_then(Path::parent);
                let id = index.meta().scan_id.as_str();
                let finished = iso8601(index.meta().finished_at);
                format!(
                    "{} {} · {} files · {} dirs · saved scan {} from {}",
                    output.units.size(total),
                    block.size_kind,
                    thousands(root.file_count),
                    thousands(root.dir_count),
                    dir.map_or_else(|| id.to_owned(), |dir| short_id(dir, id)),
                    finished.get(..16).unwrap_or(&finished).replace('T', " "),
                )
            }
        }
    }

    /// The scan's warnings, the path's for `upper_bound`; and
    /// `stale_index`. `clones_unavailable` warns when this verb asked for
    /// `--clones`, as a fresh scan does.
    pub fn warnings(&self, walk: &WalkArgs) -> Vec<Warning> {
        match self {
            Source::Fresh { scanned, .. } => scan::warnings(scanned),
            Source::Saved { index, stale, .. } => {
                // What a scan of the path would have met: the volumes under
                // it, the directories in it.
                let view = self.view();
                let under = view.root_path();
                let mounts_excluded = index
                    .volumes()
                    .iter()
                    .filter(|v| {
                        !v.entered
                            && Path::new(OsStr::from_bytes(&v.mount_point)).starts_with(under)
                    })
                    .count() as u64;
                let mut warnings = scan::quality_warnings(
                    self.unreadable_dirs(),
                    mounts_excluded,
                    index.meta().vanished_count,
                );
                warnings.extend(scan::clones_unavailable(
                    walk.clones && index.meta().clones_unavailable,
                ));
                warnings.extend(scan::upper_bound(view.node(0).upper_bound));
                warnings.extend(stale.clone());
                warnings
            }
        }
    }

    /// `atime_unreliable`, for `stale --atime`.
    pub fn atime_unreliable(&self) -> Option<Warning> {
        match self {
            Source::Fresh { scanned, .. } => {
                super::query::atime_unreliable(scanned.arena.volumes())
            }
            Source::Saved { index, .. } => super::query::lagging_atime(
                index
                    .volumes()
                    .iter()
                    .filter(|v| v.entered)
                    .map(|v| (lossy(&v.mount_point), v.options.clone())),
            ),
        }
    }

    /// Once the output is out: a fresh scan's index, under the index
    /// directory, its id in the summary line already rather than a line of
    /// its own. A failure is `index_write_failed` (exit 1), as for
    /// `scan --save`: the output stands, but the id it carries names no
    /// file.
    pub fn finish(&self, walk: &WalkArgs, output: &Output) -> Result<(), Failure> {
        if let Source::Fresh {
            scanned,
            save: Some((id, dir)),
        } = self
        {
            scan::save(scanned, walk, &[], id, Some(dir), output, false).map_err(|failure| {
                failure
                    .hint("Rerun with --no-save, or with --index-dir naming a writable directory")
            })?;
        }
        Ok(())
    }
}

/// A source as an index file, for the verbs that read one (`query`,
/// `export`): a saved scan as it is; a scan run for the verb saved first,
/// in the index directory, or under `--no-save` in a scratch directory
/// that goes when this does.
#[derive(Debug)]
pub struct Indexed {
    /// The index.
    pub index: Box<Index>,
    /// The entry the verb is about.
    pub base: u32,
    /// The scan for people: `saved scan 3f9a`, `saved as 3f9a`, `not saved`.
    pub label: String,
    /// The scratch directory, after the index so it goes after it.
    _scratch: Scratch,
}

/// A scratch directory, removed when this goes.
#[derive(Debug)]
struct Scratch(Option<PathBuf>);

impl Drop for Scratch {
    fn drop(&mut self) {
        if let Some(dir) = &self.0
            && let Err(e) = std::fs::remove_dir_all(dir)
        {
            log::warn!("cannot remove {}: {e}", dir.display());
        }
    }
}

impl Source {
    /// The source as an index (see [`Indexed`]); `walk` and `excludes`
    /// are the fresh scan's, for its saved options, and `verb` names the
    /// directory of an index not kept.
    pub fn indexed(
        self,
        verb: &str,
        walk: &WalkArgs,
        excludes: &[String],
        output: &Output,
    ) -> Result<Indexed, Failure> {
        match self {
            Source::Saved { index, base, .. } => {
                let dir = index.file().parent().and_then(Path::parent);
                let id = index.meta().scan_id.as_str();
                let short = dir.map_or_else(|| id.to_owned(), |dir| short_id(dir, id));
                Ok(Indexed {
                    index,
                    base,
                    label: format!("saved scan {short}"),
                    _scratch: Scratch(None),
                })
            }
            Source::Fresh { scanned, save } => {
                let (id, dir, label, scratch) = if let Some((id, dir)) = save {
                    let label = format!("saved as {}", short_id(&dir, id.as_str()));
                    (id, dir, label, Scratch(None))
                } else {
                    let id = scan::scan_id(&scanned)?;
                    let dir = scratch_dir(verb)?;
                    (id, dir.clone(), "not saved".to_owned(), Scratch(Some(dir)))
                };
                // On a failure below, `scratch` goes, and its directory.
                scan::save(&scanned, walk, excludes, &id, Some(&dir), output, false)?;
                let index = Index::open(&index_path(&dir, &id)).map_err(core_failure)?;
                Ok(Indexed {
                    index: Box::new(index),
                    base: 0,
                    label,
                    _scratch: scratch,
                })
            }
        }
    }
}

/// A new directory, the owner's only, for the index of a scan not to be
/// kept (`--no-save`, or no index directory): `duscope-<verb>-…` under the
/// temporary directory.
fn scratch_dir(verb: &str) -> Result<PathBuf, Failure> {
    let mut n = 0;
    loop {
        let dir = std::env::temp_dir().join(format!("duscope-{verb}-{}-{n}", std::process::id()));
        match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && n < 100 => n += 1,
            Err(e) => {
                return Err(Failure::new(
                    Exit::Runtime,
                    "io_error",
                    format!("cannot make a directory for the scan's index: {e}"),
                )
                .hint("Rerun without --no-save, or with --index-dir"));
            }
        }
    }
}

/// The scan block of a saved scan.
pub(super) fn saved_block(index: &Index, size: SizeKind) -> ScanBlock {
    let meta = index.meta();
    let root = index.node(0);
    let options: serde_json::Value = serde_json::from_str(&meta.options).unwrap_or_default();
    let volumes = index.volumes();
    let root_path = index.root_path();
    let mut excluded: Vec<(Vec<u8>, &'static str)> = index
        .excluded()
        .into_iter()
        .map(|(id, reason)| {
            let mut path = root_path.as_os_str().as_bytes().to_vec();
            path.push(b'/');
            path.extend_from_slice(&index.path(id));
            (path, reason)
        })
        .collect();
    excluded.sort();
    ScanBlock {
        id: Some(meta.scan_id.to_string()),
        root: root_path.to_string_lossy().into_owned(),
        root_as_given: lossy(&meta.root_as_given),
        started_at: iso8601(meta.started_at),
        finished_at: iso8601(meta.finished_at),
        size_kind: size.as_str(),
        attrs: match index.attrs() {
            Attrs::Basic => "basic",
            Attrs::Full => "full",
        },
        thread_count: meta.thread_count,
        fs_type: volumes.first().map(|v| v.fs_type.clone()),
        device_confined: options["one_file_system"].as_bool().unwrap_or(false),
        total_allocated_bytes: root.allocated,
        total_apparent_bytes: root.apparent,
        file_count: root.file_count,
        dir_count: root.dir_count,
        error_count: meta.error_count,
        vanished_count: meta.vanished_count,
        incremental: meta.incremental,
        volumes: volumes
            .iter()
            .map(|v| VolumeEntry {
                mount_point: lossy(&v.mount_point),
                fs_type: v.fs_type.clone(),
                entered: v.entered,
                reason: v.reason.as_deref().and_then(reason_name),
                read_only: v.read_only,
                total_bytes: v.total_bytes.unwrap_or(0),
                free_bytes: v.free_bytes.unwrap_or(0),
                dev: v.dev,
                options: v.options.clone(),
            })
            .collect(),
        excluded_count: meta.excluded_count,
        excluded: excluded
            .into_iter()
            .map(|(path, reason)| ExcludedEntry {
                path: lossy(&path),
                reason,
            })
            .collect(),
    }
}

/// A volume's skip reason as the envelope spells it.
fn reason_name(reason: &str) -> Option<&'static str> {
    use duscope_walk::Skip;
    [
        Skip::Firmlinked,
        Skip::Kernel,
        Skip::Network,
        Skip::OneFileSystem,
        Skip::BindCycle,
        Skip::Excluded,
        Skip::ExcludedCache,
    ]
    .into_iter()
    .map(Skip::as_str)
    .find(|name| *name == reason)
}

fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// The failure an engine error is.
pub fn core_failure(error: CoreError) -> Failure {
    match error {
        CoreError::AttrsNotCollected(_) => {
            Failure::new(Exit::Runtime, "attrs_not_collected", error.to_string())
                .hint("Rerun the command with --attrs full")
        }
        CoreError::IndexCorrupt { .. } => {
            Failure::new(Exit::Index, "index_corrupt", error.to_string())
                .hint("Run duscope scan --save to measure the tree again")
        }
        CoreError::IndexVersion { .. } => {
            Failure::new(Exit::Index, "index_version_mismatch", error.to_string())
                .hint("Run duscope scan --save to write the index this duscope reads")
        }
        CoreError::ScanNotFound(_) => {
            Failure::new(Exit::NotFound, "scan_not_found", error.to_string())
                .hint("Run duscope scans to list the saved scans")
        }
        CoreError::Selector(_) => Failure::new(Exit::Usage, "usage", error.to_string())
            .hint("Run duscope scans to list the saved scans"),
        CoreError::InvalidCursor(_) => {
            Failure::new(Exit::Usage, "invalid_cursor", error.to_string())
                .hint("Rerun the command without --cursor for the first page")
        }
        CoreError::SqlDenied(_) => Failure::new(Exit::Runtime, "sql_denied", error.to_string())
            .hint("Rerun duscope query with one SELECT of the tables duscope schema lists"),
        CoreError::SqlTimeout(_) => Failure::new(Exit::Runtime, "sql_timeout", error.to_string())
            .hint("Rerun duscope query with a narrower statement, or a --timeout up to 60"),
        CoreError::SqlInvalid(_) => Failure::new(Exit::Usage, "usage", error.to_string())
            .hint("Run duscope schema to list the tables and their columns"),
        CoreError::ImportInvalid(_) => Failure::new(Exit::Usage, "usage", error.to_string())
            .hint("Rerun duscope import with --format naming the tool that wrote the file"),
        other => Failure::new(Exit::Runtime, "io_error", other.to_string()),
    }
}
