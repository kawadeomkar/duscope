//! `diff`: what changed between two saved scans of one tree: both scans'
//! root, or `--under` a path both hold. Other roots are
//! `incompatible_scans`.

use std::path::{Path, PathBuf};

use duscope_core::envelope::{DiffEntry, DiffTotals, Envelope, QueryBlock, SCHEMA_VERSION, Tool};
use duscope_core::index::Index;
use duscope_core::query::{
    self, DEFAULT_MIN_DELTA, DiffQuery, ScanQuery, ScanSelector, SizeKind, Subtree, TreeView,
    clock, resolve_scan,
};
use duscope_core::resolve::{resolve_in_index, resolve_path};

use super::next::Hints;
use super::source::{core_failure, saved_block, scanned};
use super::{Exit, Failure};
use crate::cli::{self, DiffArgs};
use crate::out::{Format, Output, thousands};

/// `--limit`'s default, and its cap without `-o` or `--all`.
const DEFAULT_LIMIT: usize = 25;
const MAX_LIMIT: usize = 100;

/// `diff`.
pub fn run(args: &DiffArgs, index_dir: Option<&Path>, output: &Output) -> Result<(), Failure> {
    let limit = row_limit(args, output)?;
    let cwd = std::env::current_dir().map_err(|e| {
        Failure::new(
            Exit::Runtime,
            "io_error",
            format!("cannot read the working directory: {e}"),
        )
    })?;
    let dir: PathBuf = index_dir
        .map(Path::to_owned)
        .or_else(|| duscope_core::index::default_dir(|name| std::env::var_os(name)))
        .ok_or_else(|| {
            Failure::new(
                Exit::NotFound,
                "scan_not_found",
                "no index directory to look for the scans in: neither HOME nor XDG_CACHE_HOME \
                 is set",
            )
            .hint("Run duscope with --index-dir naming the directory the scans are in")
        })?;
    let under = args
        .under
        .as_deref()
        .map(|path| resolve_path(&cwd, path).real);
    let old = open(&args.old, &dir, &cwd, under.as_deref(), &args.trust)?;
    let new = open(&args.new, &dir, &cwd, under.as_deref(), &args.trust)?;
    // The entry each scan compares from: the root, or the path both hold.
    let (old_base, new_base) = match &under {
        Some(path) => {
            let (Some(a), Some(b)) = (resolve_in_index(&old, path), resolve_in_index(&new, path))
            else {
                // A read that failed is a damaged index, not a missing path.
                check(&old)?;
                check(&new)?;
                return Err(incompatible(&old, &new, Some(path)));
            };
            (a, b)
        }
        None if old.root_path() == new.root_path() => (0, 0),
        None => return Err(incompatible(&old, &new, None)),
    };
    let size = match args.size {
        Some(cli::SizeKind::Apparent) => SizeKind::Apparent,
        _ => SizeKind::Allocated,
    };
    let min_delta = args.min_delta.unwrap_or(DEFAULT_MIN_DELTA);
    let q = DiffQuery {
        min_delta,
        depth: args.depth,
        size,
        limit,
    };
    let old_view = Subtree::new(&old, old_base);
    let new_view = Subtree::new(&new, new_base);
    let (page, mut totals) = query::diff(&old_view, &new_view, &q);
    totals.old_scan = old.meta().scan_id.to_string();
    totals.new_scan = new.meta().scan_id.to_string();
    let path = new_view.root_path().to_string_lossy().into_owned();
    drop((old_view, new_view));
    let mut filters = vec![format!("--min-delta={min_delta}")];
    if let Some(path) = &under {
        filters.push(format!("--under={}", path.display()));
    }
    let returned_count = page.entries.len() as u64;
    let ids = (
        query::short_id(&dir, &totals.old_scan),
        query::short_id(&dir, &totals.new_scan),
    );
    let footer = footer(&totals, &ids, output);
    let mut envelope = Envelope {
        ok: true,
        schema_version: SCHEMA_VERSION,
        tool: Tool::CURRENT,
        scan: saved_block(&new, size),
        query: QueryBlock {
            command: "diff",
            path,
            depth: args.depth,
            limit: limit.map(|l| l as u64),
            sort: "abs_delta_desc".to_owned(),
            filters,
        },
        entries: page.entries,
        returned_count,
        total_count: page.total_count,
        truncated: page.truncated,
        next_cursor: None,
        warnings: Vec::new(),
        next: Vec::new(),
    };
    envelope.next = next(&envelope, new.file());
    check(&old)?;
    check(&new)?;
    output.list_diff(&envelope, &totals, &footer)
}

/// The rows to return: `--limit`, 25 by default and 100 at most unless
/// `-o` names a file; every row with `--all` or `ndjson`.
fn row_limit(args: &DiffArgs, output: &Output) -> Result<Option<usize>, Failure> {
    if output.format == Format::Ndjson || args.all {
        return Ok(None);
    }
    match args.limit {
        Some(asked) if asked > MAX_LIMIT && output.file.is_none() => Err(Failure::new(
            Exit::Usage,
            "usage",
            format!("--limit {asked} is more than the {MAX_LIMIT} rows a list returns"),
        )
        .hint("Rerun duscope with --format ndjson or --all to stream every row, or -o FILE")),
        asked => Ok(Some(asked.unwrap_or(DEFAULT_LIMIT))),
    }
}

/// The saved scan a selector names. `under` picks `latest`; whether a
/// named scan holds it is the comparison's to say (`incompatible_scans`),
/// not the lookup's.
fn open(
    text: &str,
    dir: &Path,
    cwd: &Path,
    under: Option<&Path>,
    trust: &[PathBuf],
) -> Result<Index, Failure> {
    let selector = ScanSelector::parse(text).map_err(core_failure)?;
    let path = match selector {
        ScanSelector::Latest => under,
        _ => None,
    };
    let q = ScanQuery {
        index_dir: dir,
        path,
        cwd,
        // Old scans are what diff compares: none is stale to it.
        max_age_ns: i64::MAX,
        now: clock::now(),
        trust,
    };
    let resolved = resolve_scan(&selector, &q, scanned)
        .map_err(core_failure)?
        .ok_or_else(|| {
            Failure::new(
                Exit::NotFound,
                "scan_not_found",
                format!("no saved scan answers {text}"),
            )
            .hint("Run duscope scans to list the saved scans")
        })?;
    Ok(resolved.index)
}

/// `diff`'s hints, on the new scan: the largest change still in it, and
/// the largest entries under the path.
fn next(e: &Envelope<DiffEntry>, file: &Path) -> Vec<String> {
    let Some(mut hints) = Hints::new(e.scan.id.as_deref(), Some(file)) else {
        return Vec::new();
    };
    let changed = e
        .entries
        .iter()
        .find(|row| row.change != "removed")
        .map(|row| row.path.as_str());
    hints
        .run("explain", changed, &[])
        .run("top", Some(&e.query.path), &[])
        .done()
}

/// `index_corrupt` when a read of `index` failed after it opened.
fn check(index: &Index) -> Result<(), Failure> {
    match index.failure() {
        Some(message) => Err(core_failure(duscope_core::CoreError::IndexCorrupt {
            file: index.file().display().to_string(),
            message,
        })),
        None => Ok(()),
    }
}

/// `incompatible_scans`: two roots, and no `--under` path both hold.
fn incompatible(old: &Index, new: &Index, under: Option<&Path>) -> Failure {
    let message = match under {
        Some(path) => format!(
            "the scans do not both hold {}: {} is a scan of {}, {} of {}",
            path.display(),
            old.meta().scan_id,
            old.root_path().display(),
            new.meta().scan_id,
            new.root_path().display()
        ),
        None => format!(
            "the scans have different roots: {} is a scan of {}, {} of {}",
            old.meta().scan_id,
            old.root_path().display(),
            new.meta().scan_id,
            new.root_path().display()
        ),
    };
    Failure::new(Exit::Usage, "incompatible_scans", message)
        .hint("Rerun duscope diff with --under naming a path both scans hold")
}

/// The one line for people: bytes come and gone, the net, the entries,
/// and the two scans by their shortest ids.
fn footer(t: &DiffTotals, (old_id, new_id): &(String, String), output: &Output) -> String {
    let size = |n: u64| output.units.size(n);
    let net = if t.net_bytes < 0 {
        format!("-{}", size(t.net_bytes.unsigned_abs()))
    } else {
        format!("+{}", size(t.net_bytes.unsigned_abs()))
    };
    format!(
        "+{} added · -{} removed · net {net} · {} entries added, {} removed, {} changed · {old_id} → {new_id}",
        size(t.added_bytes),
        size(t.removed_bytes),
        thousands(t.added_count),
        thousands(t.removed_count),
        thousands(t.changed_count),
    )
}
