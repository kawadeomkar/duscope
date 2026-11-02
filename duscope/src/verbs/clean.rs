//! `clean`: the plan of what to move to the Trash, and, with `--yes` or
//! `yes` typed at the prompt, moving it.
//!
//! The items are paths and `--from` handles from `explain --json`: a
//! handle's MAC and expiry are checked, and its path must still hold the
//! device and inode it names. The plan is made against a saved scan, the
//! newest that holds the first item (or `--scan`): its root bounds what may
//! move, and its sizes are the plan's. With none, the run fails
//! `scan_not_found`.
//!
//! A refused item makes the whole run a dry run (`refused_path`) unless
//! `--skip-refused`. Without `--yes` nothing moves: on a terminal the plan is
//! shown and `yes` asked for once; otherwise, or under `--no-input`, the
//! plan comes back with `confirmation_required`. The saved scan is not
//! changed: after a move an `index_untouched` warning names `rescan`.

use std::ffi::OsStr;
use std::fmt::Write as _;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use duscope_core::clean::{CleanItem, CleanOutcome, Plan, clean_execute, clean_plan};
use duscope_core::envelope::{
    CleanEnvelope, CleanQuery, CleanResult, ErrorObject, SCHEMA_VERSION, ScanBlock, Tool, Warning,
};
use duscope_core::handle::{Handle, Key};
use duscope_core::query::{ExplainQuery, ScanSelector, SizeKind, clock};

use super::explain::unresolved_last;
use super::source::{Lookup, core_failure, saved_block, stale_index};
use super::treemap::shell_quote;
use super::{Exit, Failure, scan};
use crate::cli::CleanArgs;
use crate::out::{self, Output, thousands};

/// `clean`.
pub fn run(args: &CleanArgs, index_dir: Option<&Path>, output: &Output) -> Result<(), Failure> {
    let lookup = Lookup::new(
        args.max_age.as_deref(),
        args.test_now.as_deref(),
        &args.trust,
        index_dir,
    )?;
    let mut items: Vec<CleanItem> = args
        .paths
        .iter()
        .map(|path| CleanItem {
            path: unresolved_last(&lookup.cwd, path),
            handle: None,
        })
        .collect();
    let handle_scan = handles(args, lookup.dir.as_deref(), &mut items)?;
    let first = items[0].path.clone();
    let (selector, path) = match (&args.scan, handle_scan) {
        (Some(text), _) => (ScanSelector::parse(text).map_err(core_failure)?, None),
        (None, Some(id)) if args.paths.is_empty() => {
            (ScanSelector::parse(&id).map_err(core_failure)?, None)
        }
        (None, _) => (ScanSelector::Latest, Some(first.as_path())),
    };
    let Some(resolved) = lookup.resolve(&selector, path)? else {
        return Err(no_scan(&first));
    };
    let caches = scan::cache_rules(args.caches_extra.as_deref())?;
    let env = |name: &str| std::env::var_os(name);
    let q = ExplainQuery {
        caches: &caches,
        env: &env,
    };
    let mut plan = clean_plan(&items, &resolved.index, &q).map_err(core_failure)?;

    let refused = plan.refused_count();
    let blocked = refused > 0 && !args.skip_refused;
    let moving = plan.trash_count();
    let scan_block = block(&resolved.index);
    let mut warnings: Vec<Warning> = resolved
        .stale
        .then(|| stale_index(&resolved.index, &lookup.max_age))
        .into_iter()
        .collect();
    let query = CleanQuery {
        command: "clean",
        paths: plan
            .plan
            .items
            .iter()
            .map(|item| item.path.clone())
            .collect(),
        handle_count: args.from.len() as u64,
        yes: args.yes,
        skip_refused: args.skip_refused,
    };

    // On a terminal, without --yes, the plan is shown and `yes` asked for
    // once.
    let mut act = args.yes && !blocked;
    let mut asked = false;
    if !args.yes
        && !blocked
        && !args.no_input
        && moving > 0
        && !output.is_machine()
        && out::interactive()
    {
        let shown = envelope(&plan, None, &scan_block, &query, None, &warnings);
        output.clean(&shown, &plan_line(&plan, output), true)?;
        warnings.clear();
        asked = true;
        act = out::ask("Type yes to move them to the Trash: ").is_some_and(|a| a == "yes");
    }
    let outcome = act.then(|| clean_execute(&mut plan));
    plan.plan.confirmation_required = !act && !blocked && moving > 0;
    if outcome.is_some_and(|o| o.trashed_count > 0) {
        warnings.push(index_untouched(&scan_block));
    }

    let failure = failure(&plan, blocked, outcome, asked);
    let error = failure.as_ref().map(|f| match outcome {
        // An item's own error, as the plan holds it.
        Some(_) if !blocked => first_error(&plan)
            .cloned()
            .unwrap_or_else(|| out::error_object(f)),
        _ => out::error_object(f),
    });
    let next = next(args, &plan, outcome.is_some(), &scan_block);
    let footer = footer(&plan, outcome, output);
    let answer = envelope(&plan, outcome, &scan_block, &query, error, &warnings);
    let answer = CleanEnvelope { next, ..answer };
    output.clean(&answer, &footer, !asked)?;
    match failure {
        Some(failure) => Err(failure.reported()),
        None => Ok(()),
    }
}

/// No saved scan holds the first item: a plan needs one, for the root that
/// bounds what may move and for its sizes.
fn no_scan(first: &Path) -> Failure {
    let dir = first.parent().unwrap_or(first).to_string_lossy();
    Failure::new(
        Exit::NotFound,
        "scan_not_found",
        format!(
            "no saved scan holds {}: clean plans from a saved scan",
            first.display()
        ),
    )
    .hint(format!(
        "Run duscope scan --save {} first, or name a saved scan with --scan",
        shell_quote(&dir)
    ))
}

/// The scan block of the saved scan the plan's sizes come from.
fn block(index: &duscope_core::index::Index) -> ScanBlock {
    let size = if index.meta().size_kind == "apparent" {
        SizeKind::Apparent
    } else {
        SizeKind::Allocated
    };
    saved_block(index, size)
}

/// Why the run did not do all it was asked, if it did not: a refused item
/// (`blocked`, without `--skip-refused`), an item that could not be moved, or
/// a plan that waits for a yes (`asked`: the prompt was answered otherwise).
fn failure(
    plan: &Plan,
    blocked: bool,
    outcome: Option<CleanOutcome>,
    asked: bool,
) -> Option<Failure> {
    if blocked {
        let message = format!(
            "{} of the {} items cannot be moved to the Trash, so nothing was moved",
            thousands(plan.refused_count() as u64),
            thousands(plan.plan.items.len() as u64)
        );
        return Some(
            Failure::new(Exit::Runtime, "refused_path", message)
                .hint("Rerun duscope clean with --skip-refused to move the others"),
        );
    }
    if let Some(outcome) = outcome {
        return first_error(plan).map(|error| {
            let message = if outcome.failed_count > 1 {
                format!(
                    "{} items were not moved; the first: {}",
                    thousands(outcome.failed_count),
                    error.message
                )
            } else {
                error.message.clone()
            };
            let mut failure = Failure::new(Exit::Runtime, error.code, message);
            failure.hint.clone_from(&error.hint);
            failure
        });
    }
    (plan.trash_count() > 0).then(|| {
        let message = if asked {
            "the answer was not yes, so nothing was moved"
        } else {
            "nothing was moved: clean moves items to the Trash only with --yes"
        };
        Failure::new(Exit::Runtime, "confirmation_required", message)
            .hint("Rerun the command with --yes to move them to the Trash")
    })
}

/// Verifies each `--from` handle with the key in the index directory and
/// adds its path to `items`, with the device and inode it names; the scan
/// the first handle answered from, when it names one.
fn handles(
    args: &CleanArgs,
    dir: Option<&Path>,
    items: &mut Vec<CleanItem>,
) -> Result<Option<String>, Failure> {
    if args.from.is_empty() {
        return Ok(None);
    }
    let Some(dir) = dir else {
        return Err(Failure::new(
            Exit::Runtime,
            "handle_invalid",
            "no index directory holds the key handles are signed with: neither HOME nor \
             XDG_CACHE_HOME is set",
        )
        .hint("Run duscope with --index-dir naming the directory explain used"));
    };
    let key = Key::open(dir).map_err(core_failure)?;
    let mut scan = None;
    for token in &args.from {
        // The clock, not --test-now: a handle holds fifteen minutes.
        let payload = Handle::verify(&key, token, clock::now()).map_err(core_failure)?;
        if scan.is_none() && !payload.scan_id.is_empty() {
            scan = Some(payload.scan_id.clone());
        }
        items.push(CleanItem {
            path: PathBuf::from(OsStr::from_bytes(&payload.path)),
            handle: Some((payload.dev, payload.ino)),
        });
    }
    Ok(scan)
}

/// The envelope of `plan`.
fn envelope(
    plan: &Plan,
    outcome: Option<CleanOutcome>,
    scan: &ScanBlock,
    query: &CleanQuery,
    error: Option<ErrorObject>,
    warnings: &[Warning],
) -> CleanEnvelope {
    let outcome = outcome.unwrap_or_default();
    CleanEnvelope {
        ok: error.is_none(),
        schema_version: SCHEMA_VERSION,
        tool: Tool::CURRENT,
        scan: Some(scan.clone()),
        query: query.clone(),
        result: CleanResult {
            plan: plan.plan.clone(),
            trashed_count: outcome.trashed_count,
            trashed_bytes: outcome.trashed_bytes,
            freed_bytes: 0,
        },
        error,
        warnings: warnings.to_vec(),
        next: Vec::new(),
    }
}

/// The first item that could not be moved: its error.
fn first_error(plan: &Plan) -> Option<&ErrorObject> {
    plan.plan.items.iter().find_map(|item| item.error.as_ref())
}

/// The saved scan is not changed by a move: a rescan measures the tree
/// again.
fn index_untouched(scan: &ScanBlock) -> Warning {
    let hint = match scan.id.as_deref() {
        Some(id) => format!("Run duscope rescan {id} to measure the tree again"),
        None => "Run duscope scan to measure the tree again".to_owned(),
    };
    Warning {
        code: "index_untouched",
        message: "the saved scans still hold what moved to the Trash: they are never changed"
            .to_owned(),
        count: None,
        hint: Some(hint),
    }
}

/// `clean`'s hints: after a plan that waits for a yes, the same items
/// against the same saved scan with `--yes` (a handle's item by its handle
/// again); after a move, the rescan that measures the tree again. None for
/// a path that is not UTF-8, which a command line cannot carry.
fn next(args: &CleanArgs, plan: &Plan, acted: bool, scan: &ScanBlock) -> Vec<String> {
    if acted {
        return scan
            .id
            .as_deref()
            .map(|id| vec![format!("duscope rescan {id}")])
            .unwrap_or_default();
    }
    if !plan.plan.confirmation_required {
        return Vec::new();
    }
    let mut command = String::from("duscope clean");
    // The plan holds the paths first, in their order, then the handles'.
    for item in plan.plan.items.iter().take(args.paths.len()) {
        let Ok(path) = std::str::from_utf8(&item.path_bytes) else {
            return Vec::new();
        };
        let _ = write!(command, " {}", shell_quote(path));
    }
    for token in &args.from {
        let _ = write!(command, " --from {}", shell_quote(token));
    }
    if let Some(selector) = args.scan.as_deref().or(scan.id.as_deref()) {
        let _ = write!(command, " --scan {}", shell_quote(selector));
    }
    for file in &args.trust {
        let Some(file) = file.to_str() else {
            return Vec::new();
        };
        let _ = write!(command, " --trust {}", shell_quote(file));
    }
    if args.skip_refused {
        command.push_str(" --skip-refused");
    }
    command.push_str(" --yes");
    vec![command]
}

/// The plan in one line: what it would move.
fn plan_line(plan: &Plan, output: &Output) -> String {
    let refused = plan.refused_count();
    let mut line = format!(
        "{} to move to the Trash: {} on disk, {} freed once the Trash is emptied",
        count(plan.trash_count(), "item"),
        output.units.size(plan.plan.total_allocated_bytes),
        output.units.size(plan.plan.total_frees_bytes),
    );
    if refused > 0 {
        let _ = write!(line, " · {} refused", thousands(refused as u64));
    }
    line
}

/// The line under the table: what moved, or what would.
fn footer(plan: &Plan, outcome: Option<CleanOutcome>, output: &Output) -> String {
    match outcome {
        Some(o) => {
            let mut line = format!(
                "moved {} to the Trash, {} (not yet freed)",
                count(
                    usize::try_from(o.trashed_count).unwrap_or(usize::MAX),
                    "item"
                ),
                output.units.size(o.trashed_bytes)
            );
            if o.failed_count > 0 {
                let _ = write!(line, " · {} not moved", thousands(o.failed_count));
            }
            line
        }
        None => format!("{} · dry run: nothing moved", plan_line(plan, output)),
    }
}

/// `n` things, the noun in the plural past one.
fn count(n: usize, noun: &str) -> String {
    format!(
        "{} {noun}{}",
        thousands(n as u64),
        if n == 1 { "" } else { "s" }
    )
}
