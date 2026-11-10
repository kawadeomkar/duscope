//! `clean`: the plan of what to move to the Trash, or with `--permanent` to
//! delete, and, with `--yes` or `yes` typed at the prompt, acting on it.
//!
//! The items are paths and `--from` handles from `explain --json`: a
//! handle's MAC and expiry are checked, and its path must still hold the
//! device and inode it names. The plan is made against a saved scan, the
//! newest that holds the first item (or `--scan`): its root bounds what may
//! move, and its sizes are the plan's. With none, the run fails
//! `scan_not_found`.
//!
//! A refused item makes the whole run a dry run (`refused_path`) unless
//! `--skip-refused`; so does a plan whose `total_frees_bytes` is not within
//! 1% of `--expect-bytes` (`expectation_mismatch`), checked before anything
//! is touched. Without `--yes` nothing moves: on a terminal the plan is
//! shown and `yes` asked for once; otherwise, or under `--no-input`, the
//! plan comes back with `confirmation_required`. The saved scan is not
//! changed: after a move or a deletion an `index_untouched` warning names
//! the rescan that measures the tree again (for an import, a new scan), and
//! `next` holds it.
//!
//! The MCP tool `duscope_delete` runs this verb through two flags of its
//! own, never typed: `--expect-item-bytes`, one figure per `--from` in
//! their order, which refuses an item off by more than 1%, and one whose
//! bytes the saved scan does not know whatever its figure; and `--dry-run`,
//! which answers the plan as a successful answer with no prompt and no
//! `--yes` hint, since a client asks for it as a plan, not a confirmation.
//! Under a tool call, acting claims the call's point of no return
//! (`CallSignals::commit`) just before anything moves: a call cancelled,
//! past its deadline or whose client has gone by then moves nothing
//! (`interrupted`), and one committed runs to its end.

use std::ffi::OsStr;
use std::fmt::Write as _;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use duscope_core::clean::{
    CleanItem, CleanOutcome, Method, Plan, UNEXPECTED, clean_execute, clean_plan,
};
use duscope_core::envelope::{
    CleanEnvelope, CleanQuery, CleanResult, ErrorObject, SCHEMA_VERSION, ScanBlock, Tool, Warning,
};
use duscope_core::handle::{Handle, Key};
use duscope_core::index::Index;
use duscope_core::query::{ExplainQuery, ScanSelector, SizeKind, TreeView, clock};

use super::explain::unresolved_last;
use super::next::{self, Hints};
use super::source::{Lookup, core_failure, saved_block, scanned, stale_index};
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
    item_figures_given(args)?;
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
    let method = if args.permanent {
        Method::Permanent
    } else {
        Method::Trash
    };
    let mut plan = clean_plan(&items, &resolved.index, &q, method).map_err(core_failure)?;
    plan.refuse_unexpected(&args.expect_item_bytes);
    let query = query(args, &plan);

    let refused = plan.refused_count();
    // Nothing is touched past either: a refused item, or a plan that frees
    // other than --expect-bytes says.
    let mismatch = args.expect_bytes.filter(|&n| !plan.meets(n));
    let blocked = (refused > 0 && !args.skip_refused) || mismatch.is_some();
    let moving = plan.acting_count();
    let scan_block = block(&resolved.index);
    let mut warnings: Vec<Warning> = resolved
        .stale
        .then(|| stale_index(&resolved.index, &lookup.max_age))
        .into_iter()
        .collect();

    // On a terminal, without --yes, the plan is shown and `yes` asked for
    // once.
    let mut act = args.yes && !blocked;
    let mut asked = false;
    if !args.yes
        && !args.dry_run
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
        let question = match method {
            Method::Trash => "Type yes to move them to the Trash: ",
            Method::Permanent => "Type yes to delete them permanently: ",
        };
        act = out::ask(question).is_some_and(|a| a == "yes");
    }
    let stopped = stopped_first(&mut act);
    let outcome = act.then(|| clean_execute(&mut plan));
    plan.plan.confirmation_required = !act && !blocked && !stopped && moving > 0;
    // What moved or was deleted is still in the saved scan.
    let changed =
        outcome.is_some_and(|o| o.trashed_count + o.deleted_count > 0 || o.freed_bytes > 0);
    if changed {
        warnings.push(index_untouched(&resolved.index, method));
    }

    let failure = stopped.then(|| stopped_failure(method)).or_else(|| {
        failure(
            &plan,
            refused > 0 && !args.skip_refused,
            mismatch,
            outcome,
            asked,
            args,
        )
    });
    let error = failure.as_ref().map(|f| match outcome {
        // An item's own error, as the plan holds it.
        Some(_) if !blocked => first_error(&plan)
            .cloned()
            .unwrap_or_else(|| out::error_object(f)),
        _ => out::error_object(f),
    });
    let next = match outcome {
        Some(_) => renew(changed, &resolved.index),
        None if args.dry_run || stopped => Vec::new(),
        None => next(args, &plan, &scan_block),
    };
    let footer = footer(&plan, outcome, output);
    let answer = envelope(&plan, outcome, &scan_block, &query, error, &warnings);
    let answer = CleanEnvelope { next, ..answer };
    output.clean(&answer, &footer, !asked)?;
    match failure {
        Some(failure) => Err(failure.reported()),
        None => Ok(()),
    }
}

/// Under an MCP tool call, acting (`act`) is the call's point of no return,
/// claimed just before anything moves: whether the call was stopped first
/// (cancelled, past its deadline, its client gone), when `act` is cleared
/// and nothing moves. A call that commits is answered with what this run
/// did, however long it takes.
fn stopped_first(act: &mut bool) -> bool {
    let stopped = *act && super::mcp::running_call().is_some_and(|call| !call.commit());
    if stopped {
        log::info!("the tool call was stopped before clean acted: nothing is moved or deleted");
        *act = false;
    }
    stopped
}

/// The failure of a run whose MCP tool call was stopped (cancelled, past
/// its deadline, its client gone) before it acted: nothing was moved or
/// deleted. The call was answered already, or is owed no answer.
fn stopped_failure(method: Method) -> Failure {
    let moved = match method {
        Method::Trash => "moved",
        Method::Permanent => "deleted",
    };
    Failure::new(
        Exit::Interrupted,
        "interrupted",
        format!("the tool call was stopped before anything was {moved}, so nothing was"),
    )
    .hint("Call the tool again for a new plan")
}

/// `--expect-item-bytes` gives one figure per `--from` handle, or none.
fn item_figures_given(args: &CleanArgs) -> Result<(), Failure> {
    if args.expect_item_bytes.is_empty() || args.expect_item_bytes.len() == args.from.len() {
        return Ok(());
    }
    Err(Failure::new(
        Exit::Usage,
        "usage",
        "--expect-item-bytes takes one figure per --from handle",
    )
    .hint("Give one --expect-item-bytes for each --from, in the same order"))
}

/// What `clean` was asked, with the items as `plan` names them.
fn query(args: &CleanArgs, plan: &Plan) -> CleanQuery {
    CleanQuery {
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
        permanent: args.permanent,
        expect_bytes: args.expect_bytes,
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
/// without `--skip-refused` (`refused`), a plan that frees other than
/// `--expect-bytes` says (`mismatch`, the bytes expected), an item that could
/// not be moved or deleted, or a plan that waits for a yes (`asked`: the
/// prompt was answered otherwise).
fn failure(
    plan: &Plan,
    refused: bool,
    mismatch: Option<u64>,
    outcome: Option<CleanOutcome>,
    asked: bool,
    args: &CleanArgs,
) -> Option<Failure> {
    let (moved, verb) = match plan.method() {
        Method::Trash => ("moved", "move"),
        Method::Permanent => ("deleted", "delete"),
    };
    if refused {
        if let Some(failure) = unexpected(plan, &args.expect_item_bytes, moved) {
            return Some(failure);
        }
        let cannot = match plan.method() {
            Method::Trash => "cannot be moved to the Trash",
            Method::Permanent => "cannot be deleted",
        };
        let message = format!(
            "{} of the {} items {cannot}, so nothing was {moved}",
            thousands(plan.refused_count() as u64),
            thousands(plan.plan.items.len() as u64)
        );
        return Some(
            Failure::new(Exit::Runtime, "refused_path", message).hint(format!(
                "Rerun duscope clean with --skip-refused to {verb} the others"
            )),
        );
    }
    if let Some(expected) = mismatch {
        let message = format!(
            "the plan frees {} bytes, not within 1% of --expect-bytes {}, so nothing was {moved}",
            thousands(plan.plan.total_frees_bytes),
            thousands(expected)
        );
        return Some(
            Failure::new(Exit::Runtime, "expectation_mismatch", message)
                .hint("Run duscope clean without --yes and review the plan again"),
        );
    }
    if let Some(outcome) = outcome {
        return first_error(plan).map(|error| {
            let message = if outcome.failed_count > 1 {
                format!(
                    "{} items were not {moved}; the first: {}",
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
    // The tool's dry run is a plan, not a question left open.
    if args.dry_run {
        return None;
    }
    (plan.acting_count() > 0).then(|| {
        let (message, hint) = match plan.method() {
            Method::Trash => (
                "nothing was moved: clean moves items to the Trash only with --yes",
                "Rerun the command with --yes to move them to the Trash",
            ),
            Method::Permanent => (
                "nothing was deleted: clean deletes items only with --yes",
                "Rerun the command with --yes to delete them permanently",
            ),
        };
        let message = if asked {
            format!("the answer was not yes, so nothing was {moved}")
        } else {
            message.to_owned()
        };
        Failure::new(Exit::Runtime, "confirmation_required", message).hint(hint)
    })
}

/// The failure of a run whose only refused items are the ones
/// `--expect-item-bytes` refused: which figure the first one missed.
fn unexpected(plan: &Plan, expected: &[u64], moved: &str) -> Option<Failure> {
    let items = &plan.plan.items;
    if !items
        .iter()
        .filter(|item| item.action == "refused")
        .all(|item| item.reason == Some(UNEXPECTED))
    {
        return None;
    }
    let (at, item) = items
        .iter()
        .enumerate()
        .find(|(_, item)| item.reason == Some(UNEXPECTED))?;
    let count = format!(
        "{} of the {} items free other than they were expected to, so nothing was {moved}",
        thousands(plan.refused_count() as u64),
        thousands(items.len() as u64),
    );
    // An item the saved scan does not hold: what it frees is not known, so
    // no figure is met until a scan measures it.
    let Some(frees) = item.frees_bytes else {
        let message = format!(
            "{count}: the saved scan does not hold {}, so what it frees is not known",
            item.path
        );
        return Some(
            Failure::new(Exit::Runtime, "expectation_mismatch", message).hint(
                "Rescan the directory that holds it (duscope_scan, or duscope rescan), then \
                 explain it again for a handle and its bytes",
            ),
        );
    };
    let message = format!(
        "{count}: {} frees {} bytes, not within 1% of {}",
        item.path,
        thousands(frees),
        thousands(expected.get(at).copied().unwrap_or(0)),
    );
    Some(
        Failure::new(Exit::Runtime, "expectation_mismatch", message)
            .hint("Review the plan's frees_bytes for each item and pass them as expect_bytes"),
    )
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
            deleted_count: outcome.deleted_count,
            freed_bytes: outcome.freed_bytes,
        },
        error,
        warnings: warnings.to_vec(),
        next: Vec::new(),
    }
}

/// The first item that could not be moved or deleted: its error.
fn first_error(plan: &Plan) -> Option<&ErrorObject> {
    plan.plan.items.iter().find_map(|item| item.error.as_ref())
}

/// The saved scan is not changed by a move or a deletion: a rescan of it,
/// named as `next` names it, measures the tree again; an import, which has
/// no scan options to rescan with, is renewed by a new scan of its root.
fn index_untouched(index: &Index, method: Method) -> Warning {
    let command = if scanned(index) {
        let id = index.meta().scan_id.to_string();
        next::command("rescan", None, &[], &id, index.file())
    } else {
        let root = index.root_path().to_string_lossy();
        format!("duscope scan --save {}", shell_quote(&root))
    };
    let what = match method {
        Method::Trash => "what moved to the Trash",
        Method::Permanent => "what was deleted",
    };
    Warning {
        code: "index_untouched",
        message: format!("the saved scans still hold {what}: they are never changed"),
        count: None,
        hint: Some(format!("Run {command} to measure the tree again")),
    }
}

/// `clean`'s hint once it acted: when something moved or was deleted
/// (`changed`), the rescan that measures the tree again, as `scans --check`
/// suggests it. None when nothing changed, whose items' errors say what to
/// do, nor for an import, which a rescan refuses.
fn renew(changed: bool, index: &Index) -> Vec<String> {
    if !changed || !scanned(index) {
        return Vec::new();
    }
    let id = index.meta().scan_id.to_string();
    let root = index.root_path().to_string_lossy();
    Hints::new(Some(&id), Some(index.file()))
        .map(|mut hints| hints.run("rescan", Some(&root), &[]).done())
        .unwrap_or_default()
}

/// `clean`'s hint for a plan that waits for a yes: the same items against
/// the same saved scan with `--yes` (a handle's item by its handle again).
/// None for a path that is not UTF-8, which a command line cannot carry.
fn next(args: &CleanArgs, plan: &Plan, scan: &ScanBlock) -> Vec<String> {
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
    if args.permanent {
        command.push_str(" --permanent");
    }
    if let Some(n) = args.expect_bytes {
        let _ = write!(command, " --expect-bytes {n}");
    }
    command.push_str(" --yes");
    vec![command]
}

/// The plan in one line: what it would move or delete.
fn plan_line(plan: &Plan, output: &Output) -> String {
    let refused = plan.refused_count();
    let (to, freed) = match plan.method() {
        Method::Trash => ("move to the Trash", "freed once the Trash is emptied"),
        Method::Permanent => ("delete permanently", "freed"),
    };
    let mut line = format!(
        "{} to {to}: {} on disk, {} {freed}",
        count(plan.acting_count(), "item"),
        output.units.size(plan.plan.total_allocated_bytes),
        output.units.size(plan.plan.total_frees_bytes),
    );
    if refused > 0 {
        let _ = write!(line, " · {} refused", thousands(refused as u64));
    }
    line
}

/// The line under the table: what moved or was deleted, or what would be.
fn footer(plan: &Plan, outcome: Option<CleanOutcome>, output: &Output) -> String {
    let items = |n: u64| count(usize::try_from(n).unwrap_or(usize::MAX), "item");
    match (outcome, plan.method()) {
        (Some(o), Method::Trash) => {
            let mut line = format!(
                "moved {} to the Trash, {} (not yet freed)",
                items(o.trashed_count),
                output.units.size(o.trashed_bytes)
            );
            if o.failed_count > 0 {
                let _ = write!(line, " · {} not moved", thousands(o.failed_count));
            }
            line
        }
        (Some(o), Method::Permanent) => {
            let mut line = format!(
                "deleted {}, {} freed",
                items(o.deleted_count),
                output.units.size(o.freed_bytes)
            );
            if o.failed_count > 0 {
                let _ = write!(line, " · {} not deleted", thousands(o.failed_count));
            }
            line
        }
        (None, Method::Trash) => format!("{} · dry run: nothing moved", plan_line(plan, output)),
        (None, Method::Permanent) => {
            format!("{} · dry run: nothing deleted", plan_line(plan, output))
        }
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
