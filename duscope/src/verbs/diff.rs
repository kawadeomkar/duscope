//! `diff`: what changed between two saved scans of one tree: both scans'
//! root, or `--under` a path both hold. Other roots are
//! `incompatible_scans`.

use std::path::{Path, PathBuf};

use duscope_core::envelope::{DiffTotals, Envelope, QueryBlock, SCHEMA_VERSION, Tool};
use duscope_core::index::Index;
use duscope_core::query::{
    self, DEFAULT_MIN_DELTA, DiffQuery, ScanQuery, ScanSelector, SizeKind, Subtree, TreeView,
    clock, resolve_scan, timegrammar,
};
use duscope_core::resolve::{resolve_in_index, resolve_path};

use super::source::{core_failure, saved_block, stale_index};
use super::{Exit, Failure};
use crate::cli::{self, DiffArgs};
use crate::out::{Format, Output, thousands};

/// `--max-age` when neither the flag nor the configuration sets it.
const DEFAULT_MAX_AGE: &str = "7d";

/// `--limit`'s default, and its cap without `-o` or `--all`.
const DEFAULT_LIMIT: usize = 25;
const MAX_LIMIT: usize = 100;

/// `diff`.
pub fn run(args: &DiffArgs, index_dir: Option<&Path>, output: &Output) -> Result<(), Failure> {
    let limit = if output.format == Format::Ndjson || args.all {
        None
    } else {
        match args.limit {
            Some(asked) if asked > MAX_LIMIT && output.file.is_none() => {
                return Err(Failure::new(
                    Exit::Usage,
                    "usage",
                    format!("--limit {asked} is more than the {MAX_LIMIT} rows a list returns"),
                )
                .hint(
                    "Rerun duscope with --format ndjson or --all to stream every row, or -o FILE",
                ));
            }
            asked => Some(asked.unwrap_or(DEFAULT_LIMIT)),
        }
    };
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
        .unwrap_or_else(|| cwd.clone());
    let max_age = args.max_age.as_deref().unwrap_or(DEFAULT_MAX_AGE);
    let max_age_ns = timegrammar::parse_age(max_age)
        .map_err(|e| Failure::new(Exit::Usage, "usage", format!("--max-age {e}")))?;
    let under = args
        .under
        .as_deref()
        .map(|path| resolve_path(&cwd, path).real);
    let open = |text: &str| -> Result<(Index, bool), Failure> {
        let selector = ScanSelector::parse(text).map_err(core_failure)?;
        // The path picks `latest`; whether a named scan holds it is the
        // comparison's to say (incompatible_scans), not the lookup's.
        let path = match selector {
            ScanSelector::Latest => under.as_deref(),
            _ => None,
        };
        let q = ScanQuery {
            index_dir: &dir,
            path,
            cwd: &cwd,
            max_age_ns,
            now: clock::now(),
        };
        let resolved = resolve_scan(&selector, &q, |_| true)
            .map_err(core_failure)?
            .ok_or_else(|| {
                Failure::new(
                    Exit::NotFound,
                    "scan_not_found",
                    format!("no saved scan answers {text}"),
                )
                .hint("Run duscope scans to list the saved scans")
            })?;
        Ok((resolved.index, resolved.stale))
    };
    let (old, old_stale) = open(&args.old)?;
    let (new, new_stale) = open(&args.new)?;
    // The entry each scan compares from: the root, or the path both hold.
    let (old_base, new_base) = match &under {
        Some(path) => match (resolve_in_index(&old, path), resolve_in_index(&new, path)) {
            (Some(a), Some(b)) => (a, b),
            _ => return Err(incompatible(&old, &new, Some(path))),
        },
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
    let mut warnings = Vec::new();
    for (index, stale) in [(&old, old_stale), (&new, new_stale)] {
        if stale {
            warnings.push(stale_index(index, max_age));
        }
    }
    let returned_count = page.entries.len() as u64;
    let footer = footer(&totals, output);
    let envelope = Envelope {
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
        warnings,
        next: Vec::new(),
    };
    for index in [&old, &new] {
        if let Some(message) = index.failure() {
            return Err(core_failure(duscope_core::CoreError::IndexCorrupt {
                file: index.file().display().to_string(),
                message,
            }));
        }
    }
    output.list_diff(&envelope, &totals, &footer)
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

/// The one line for people: bytes come and gone, the net, the entries.
fn footer(t: &DiffTotals, output: &Output) -> String {
    let size = |n: u64| output.units.size(n);
    let net = if t.net_bytes < 0 {
        format!("-{}", size(t.net_bytes.unsigned_abs()))
    } else {
        format!("+{}", size(t.net_bytes.unsigned_abs()))
    };
    format!(
        "+{} added · -{} removed · net {net} · {} entries added, {} removed, {} changed · {} → {}",
        size(t.added_bytes),
        size(t.removed_bytes),
        thousands(t.entries_added),
        thousands(t.entries_removed),
        thousands(t.entries_changed),
        &t.old_scan[..t.old_scan.len().min(4)],
        &t.new_scan[..t.new_scan.len().min(4)],
    )
}
