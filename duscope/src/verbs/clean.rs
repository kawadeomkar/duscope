//! `clean`: the plan of what to move to the Trash, and, with `--yes` or
//! `yes` typed at the prompt, moving it.
//!
//! The items are paths and `--from` handles from `explain --json`: a
//! handle's MAC and expiry are checked, and its path must still hold the
//! device and inode it names. The plan's sizes come from the newest saved
//! scan that holds the first item (or `--scan`), else from a fresh `stat`.
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
use duscope_core::query::{
    ExplainQuery, Resolved, ScanQuery, ScanSelector, SizeKind, clock, resolve_scan, timegrammar,
};

use super::explain::unresolved_last;
use super::source::{core_failure, saved_block, scanned, stale_index};
use super::treemap::shell_quote;
use super::{Exit, Failure, scan};
use crate::cli::CleanArgs;
use crate::out::{self, Output, thousands};

/// `--max-age` when neither the flag nor the configuration sets it.
const DEFAULT_MAX_AGE: &str = "7d";

/// `clean`.
pub fn run(args: &CleanArgs, index_dir: Option<&Path>, output: &Output) -> Result<(), Failure> {
    let lookup = Lookup::new(args, index_dir)?;
    let mut items: Vec<CleanItem> = args
        .paths
        .iter()
        .map(|path| CleanItem {
            path: unresolved_last(&lookup.cwd, path),
            handle: None,
        })
        .collect();
    let handle_scan = handles(args, lookup.dir.as_deref(), &mut items)?;
    let selector = match (&args.scan, handle_scan) {
        (Some(text), _) => Some(ScanSelector::parse(text).map_err(core_failure)?),
        (None, Some(id)) if args.paths.is_empty() => {
            Some(ScanSelector::parse(&id).map_err(core_failure)?)
        }
        (None, _) => None,
    };
    let resolved = lookup.resolve(selector.as_ref(), &items[0].path)?;
    let caches = scan::cache_rules(args.caches_extra.as_deref())?;
    let env = |name: &str| std::env::var_os(name);
    let q = ExplainQuery {
        caches: &caches,
        env: &env,
    };
    let mut plan =
        clean_plan(&items, resolved.as_ref().map(|r| &r.index), &q).map_err(core_failure)?;

    let refused = plan.refused_count();
    let blocked = refused > 0 && !args.skip_refused;
    let moving = plan.trash_count();
    let scan_block = resolved.as_ref().map(|r| block(&r.index));
    let mut warnings: Vec<Warning> = resolved
        .iter()
        .filter(|r| r.stale)
        .map(|r| stale_index(&r.index, &lookup.max_age))
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
        let shown = envelope(&plan, None, scan_block.clone(), &query, None, &warnings);
        output.clean(&shown, &plan_line(&plan, output), true)?;
        warnings.clear();
        asked = true;
        act = out::ask("Type yes to move them to the Trash: ").is_some_and(|a| a == "yes");
    }
    let outcome = act.then(|| clean_execute(&mut plan));
    plan.plan.confirmation_required = !act && !blocked && moving > 0;
    if outcome.is_some_and(|o| o.trashed_count > 0) {
        warnings.push(index_untouched(scan_block.as_ref()));
    }

    let failure = failure(&plan, blocked, outcome, asked);
    let error = failure.as_ref().map(|f| match outcome {
        // An item's own error, as the plan holds it.
        Some(_) if !blocked => first_error(&plan)
            .cloned()
            .unwrap_or_else(|| out::error_object(f)),
        _ => out::error_object(f),
    });
    let next = next(&plan, &query, outcome.is_some(), scan_block.as_ref());
    let footer = footer(&plan, outcome, output);
    let answer = envelope(&plan, outcome, scan_block, &query, error, &warnings);
    let answer = CleanEnvelope { next, ..answer };
    output.clean(&answer, &footer, !asked)?;
    match failure {
        Some(failure) => Err(failure.reported()),
        None => Ok(()),
    }
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

/// How `clean` finds the saved scan its plan's sizes come from.
struct Lookup {
    /// The index directory, when one can be found.
    dir: Option<PathBuf>,
    cwd: PathBuf,
    max_age: String,
    max_age_ns: i64,
    now: i64,
    /// `--trust`.
    trust: Vec<PathBuf>,
}

impl Lookup {
    fn new(args: &CleanArgs, index_dir: Option<&Path>) -> Result<Lookup, Failure> {
        let cwd = std::env::current_dir().map_err(|e| {
            Failure::new(
                Exit::Runtime,
                "io_error",
                format!("cannot read the working directory: {e}"),
            )
        })?;
        let max_age = args
            .max_age
            .clone()
            .unwrap_or_else(|| DEFAULT_MAX_AGE.to_owned());
        let max_age_ns = timegrammar::parse_age(&max_age)
            .map_err(|e| Failure::new(Exit::Usage, "usage", format!("--max-age {e}")))?;
        let now = match &args.test_now {
            Some(text) => timegrammar::parse_date(text)
                .map(|date| date.ns)
                .map_err(|e| Failure::new(Exit::Usage, "usage", format!("--test-now {e}")))?,
            None => clock::now(),
        };
        Ok(Lookup {
            dir: index_dir
                .map(Path::to_owned)
                .or_else(|| duscope_core::index::default_dir(|name| std::env::var_os(name))),
            cwd,
            max_age,
            max_age_ns,
            now,
            trust: args.trust.clone(),
        })
    }

    /// The scan `selector` names (an item outside it is refused), or without
    /// one the newest saved scan that holds `first`; `None` when latest finds
    /// none or there is no index directory to look in.
    fn resolve(
        &self,
        selector: Option<&ScanSelector>,
        first: &Path,
    ) -> Result<Option<Resolved>, Failure> {
        let (selector, path) = match selector {
            Some(selector) => (selector, None),
            None => (&ScanSelector::Latest, Some(first)),
        };
        let dir = match (&self.dir, selector) {
            (Some(dir), _) => dir.as_path(),
            (None, ScanSelector::File(_)) => Path::new(""),
            (None, ScanSelector::Latest) => return Ok(None),
            (None, ScanSelector::Prefix(_)) => {
                return Err(Failure::new(
                    Exit::NotFound,
                    "scan_not_found",
                    "no index directory to look for the scan in: neither HOME nor \
                     XDG_CACHE_HOME is set",
                )
                .hint("Run duscope with --index-dir naming the directory the scans are in"));
            }
        };
        let q = ScanQuery {
            index_dir: dir,
            path,
            cwd: &self.cwd,
            max_age_ns: self.max_age_ns,
            now: self.now,
            trust: &self.trust,
        };
        resolve_scan(selector, &q, scanned).map_err(core_failure)
    }
}

/// The envelope of `plan`.
fn envelope(
    plan: &Plan,
    outcome: Option<CleanOutcome>,
    scan: Option<ScanBlock>,
    query: &CleanQuery,
    error: Option<ErrorObject>,
    warnings: &[Warning],
) -> CleanEnvelope {
    let outcome = outcome.unwrap_or_default();
    CleanEnvelope {
        ok: error.is_none(),
        schema_version: SCHEMA_VERSION,
        tool: Tool::CURRENT,
        scan,
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
fn index_untouched(scan: Option<&ScanBlock>) -> Warning {
    let hint = match scan.and_then(|s| s.id.as_deref()) {
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

/// `clean`'s hints: after a plan that waits for a yes, the same command
/// with `--yes`; after a move, the rescan that measures the tree again.
fn next(plan: &Plan, query: &CleanQuery, acted: bool, scan: Option<&ScanBlock>) -> Vec<String> {
    if acted {
        return scan
            .and_then(|s| s.id.as_deref())
            .map(|id| vec![format!("duscope rescan {id}")])
            .unwrap_or_default();
    }
    if !plan.plan.confirmation_required {
        return Vec::new();
    }
    let mut command = String::from("duscope clean");
    for path in &query.paths {
        command.push(' ');
        command.push_str(&shell_quote(path));
    }
    if query.skip_refused {
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
