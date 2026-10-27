//! The list verbs on a fresh scan: `top`, `tree`, `by-type`, `stale`, and
//! the rows `scan` prints. Each turns its flags into a query of
//! `duscope_core::query`, runs it on the arena, and writes the envelope.

use duscope_core::CoreError;
use duscope_core::envelope::{
    ByTypeEntry, Envelope, QueryBlock, SCHEMA_VERSION, StaleEntry, Tool, TopEntry, Warning,
};
use duscope_core::query::{
    self, ByTypeQuery, DEFAULT_MIN_SIZE, Filters, Page, Regex, SizeKind, StaleQuery, TimeField,
    TopQuery, TreeQuery, TreeView, timegrammar,
};
use duscope_core::rules::Classes;

use super::scan::{self, Scanned};
use super::{Exit, Failure};
use crate::cli::{
    ByTypeArgs, KindArg, QueryArgs, ScanArgs, SortKey, StaleArgs, TimeField as TimeArg, TopArgs,
    TreeArgs, WalkArgs,
};
use crate::out::{Format, Output};

/// `--limit`'s default, and its cap without `--format ndjson`.
const DEFAULT_LIMIT: usize = 25;
const MAX_LIMIT: usize = 100;

/// `stale`'s window without `--older-than`.
const DEFAULT_STALE_AGE: &str = "180d";

/// What every list verb settles before it runs its query.
struct Prepared {
    scanned: Scanned,
    size: SizeKind,
    now: i64,
    filters: Filters,
    limit: Option<usize>,
    sort: query::SortKey,
    reverse: bool,
    /// Warnings the flags themselves raise (`tz_unavailable`).
    warnings: Vec<Warning>,
}

/// How a verb reads `--depth`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DepthUse {
    /// It sets how deep the verb lists (`top`, `tree`).
    Lists,
    /// It filters rows deeper than it (`by-type`, `stale`).
    Filters,
}

/// The rows of `scan`: the root's largest entries, as `top` lists them by
/// default; on a terminal, or with `--treemap always`, the treemap.
pub fn scan(args: &ScanArgs, output: &Output) -> Result<(), Failure> {
    let scanned = scan::scan(&args.walk, &args.exclude)?;
    if output.format == Format::Treemap {
        return super::treemap::draw(&scanned, args, output);
    }
    let (envelope, footer) = top_envelope(scanned, &args.walk, None, "scan", false, true, output)?;
    output.list(&envelope, &footer)
}

/// `top`.
pub fn top(args: &TopArgs, output: &Output) -> Result<(), Failure> {
    let scanned = scan::scan(&args.walk, &[])?;
    let (envelope, footer) = top_envelope(
        scanned,
        &args.walk,
        Some(&args.query),
        "top",
        args.files,
        !args.no_cut_through,
        output,
    )?;
    output.list(&envelope, &footer)
}

/// The envelope and footer of `top` (or `scan`, whose `query` is `None`:
/// the defaults) on a scan.
pub(super) fn top_envelope(
    scanned: Scanned,
    walk: &WalkArgs,
    query: Option<&QueryArgs>,
    verb: &'static str,
    files: bool,
    cut_through: bool,
    output: &Output,
) -> Result<(Envelope<TopEntry>, String), Failure> {
    let prepared = prepare(scanned, walk, query, output, DepthUse::Lists, false)?;
    let depth = query.and_then(|q| q.depth);
    let q = TopQuery {
        depth,
        cut_through,
        files,
        filters: prepared.filters.clone(),
        sort: prepared.sort,
        reverse: prepared.reverse,
        limit: prepared.limit,
        size: prepared.size,
    };
    let page = query::top(&prepared.scanned.view(), &q);
    // The depth the rows were listed at: --depth 0 lists depth 1.
    let listed = if files {
        depth
    } else {
        Some(depth.unwrap_or(1).max(1))
    };
    Ok(envelope(
        prepared,
        walk,
        verb,
        listed,
        page,
        Vec::new(),
        output,
    ))
}

/// `tree`.
pub fn tree(args: &TreeArgs, output: &Output) -> Result<(), Failure> {
    let scanned = scan::scan(&args.walk, &[])?;
    let prepared = prepare(
        scanned,
        &args.walk,
        Some(&args.query),
        output,
        DepthUse::Lists,
        false,
    )?;
    let q = TreeQuery {
        depth: args.query.depth,
        min_pct: args.min_pct.unwrap_or(0.0),
        filters: prepared.filters.clone(),
        sort: prepared.sort,
        reverse: prepared.reverse,
        limit: prepared.limit,
        size: prepared.size,
    };
    let page = query::tree(&prepared.scanned.view(), &q);
    let depth = Some(args.query.depth.unwrap_or(2));
    let (envelope, footer) = envelope(
        prepared,
        &args.walk,
        "tree",
        depth,
        page,
        Vec::new(),
        output,
    );
    output.list(&envelope, &footer)
}

/// `by-type`.
pub fn by_type(args: &ByTypeArgs, output: &Output) -> Result<(), Failure> {
    let classes = match &args.classes {
        None => Classes::vendored(),
        Some(file) => {
            let text = std::fs::read_to_string(file).map_err(|e| {
                Failure::new(
                    Exit::Usage,
                    "usage",
                    format!("--classes {}: {e}", file.display()),
                )
            })?;
            let user = Classes::parse(&text, &file.display().to_string())
                .map_err(|e| Failure::new(Exit::Usage, "usage", format!("--classes {e}")))?;
            Classes::vendored().merged(user)
        }
    };
    let scanned = scan::scan(&args.walk, &[])?;
    let prepared = prepare(
        scanned,
        &args.walk,
        Some(&args.query),
        output,
        DepthUse::Filters,
        false,
    )?;
    let q = ByTypeQuery {
        classes: &classes,
        examples: args.examples.unwrap_or(3),
        filters: prepared.filters.clone(),
        limit: prepared.limit,
        size: prepared.size,
    };
    let page: Page<ByTypeEntry> = query::by_type(&prepared.scanned.view(), &q);
    let (envelope, footer) = envelope(
        prepared,
        &args.walk,
        "by-type",
        args.query.depth,
        page,
        Vec::new(),
        output,
    );
    output.list(&envelope, &footer)
}

/// `stale`.
pub fn stale(args: &StaleArgs, output: &Output) -> Result<(), Failure> {
    let older = args
        .query
        .older_than
        .as_deref()
        .unwrap_or(DEFAULT_STALE_AGE);
    let older_than = timegrammar::parse_age(older)
        .map_err(|e| Failure::new(Exit::Usage, "usage", format!("--older-than {e}")))?;
    let scanned = scan::scan(&args.walk, &[])?;
    let mut prepared = prepare(
        scanned,
        &args.walk,
        Some(&args.query),
        output,
        DepthUse::Filters,
        true,
    )?;
    prepared
        .filters
        .given
        .insert(0, format!("--older-than={older}"));
    let min_size = args.query.min_size.unwrap_or(DEFAULT_MIN_SIZE);
    prepared
        .filters
        .given
        .insert(1, format!("--min-size={min_size}"));
    if args.atime {
        prepared.filters.given.push("--atime".to_owned());
    }
    let q = StaleQuery {
        now: prepared.now,
        older_than,
        atime: args.atime,
        min_size,
        filters: prepared.filters.clone(),
        sort: prepared.sort,
        reverse: prepared.reverse,
        limit: prepared.limit,
        size: prepared.size,
    };
    let page: Page<StaleEntry> =
        query::stale(&prepared.scanned.view(), &q).map_err(core_failure)?;
    let mut warnings = Vec::new();
    // The scan is older than a tenth of the window it is asked about,
    // measured on the clock that timed the scan (a fresh scan never is).
    let age = scan::now_ns().saturating_sub(prepared.scanned.finished_ns);
    if age > older_than / 10 {
        warnings.push(Warning {
            code: "scan_age",
            message: format!(
                "the scan is {} old, more than a tenth of the {older} window",
                crate::out::thousands(u64::try_from(age / 3_600_000_000_000).unwrap_or(0)) + " h"
            ),
            count: None,
            hint: Some("Rerun duscope stale to measure from a fresh scan".to_owned()),
        });
    }
    if args.atime
        && let Some(option) = prepared.scanned.arena.volumes().first().and_then(|v| {
            v.options
                .iter()
                .find(|o| matches!(o.as_str(), "noatime" | "relatime" | "lazytime"))
                .cloned()
        })
    {
        warnings.push(Warning {
            code: "atime_unreliable",
            message: format!("the volume is mounted {option}, so access times lag reads"),
            count: None,
            hint: Some("Rely on stale without --atime, or remount with strictatime".to_owned()),
        });
    }
    let (envelope, footer) = envelope(
        prepared,
        &args.walk,
        "stale",
        args.query.depth,
        page,
        warnings,
        output,
    );
    output.list(&envelope, &footer)
}

/// Scans' common preparation: the size kind, now, the filters, the limit
/// and the sort; `--strict` turns unreadable directories into exit 4.
fn prepare(
    scanned: Scanned,
    walk: &WalkArgs,
    args: Option<&QueryArgs>,
    output: &Output,
    depth: DepthUse,
    stale: bool,
) -> Result<Prepared, Failure> {
    if args.is_some_and(|a| a.strict) && scanned.summary.unreadable_dirs > 0 {
        return Err(Failure::new(
            Exit::Permission,
            "permission_denied",
            format!(
                "{} directories could not be read, and --strict asks for all of them",
                scanned.summary.unreadable_dirs
            ),
        )
        .hint("Run without --strict to accept totals that leave them out"));
    }
    let now = match &walk.test_now {
        Some(text) => {
            timegrammar::parse_date(text)
                .map_err(|e| Failure::new(Exit::Usage, "usage", format!("--test-now {e}")))?
                .ns
        }
        None => timegrammar::now(),
    };
    let (filters, tz_unavailable) = match args {
        Some(args) => filters(args, now, depth, stale)?,
        None => (Filters::default(), false),
    };
    if filters.needs_full() && !scanned.view().full_attrs() {
        return Err(core_failure(CoreError::AttrsNotCollected(
            "--time atime or btime",
        )));
    }
    let mut warnings = Vec::new();
    if tz_unavailable {
        warnings.push(Warning {
            code: "tz_unavailable",
            message: "the system has no zoneinfo for the local zone; dates were read as UTC"
                .to_owned(),
            count: None,
            hint: Some("Install the tzdata package, or give dates in RFC 3339".to_owned()),
        });
    }
    let limit = (output.format != Format::Ndjson).then(|| {
        args.and_then(|a| a.limit)
            .unwrap_or(DEFAULT_LIMIT)
            .min(MAX_LIMIT)
    });
    Ok(Prepared {
        size: scan::size_kind(walk),
        now,
        filters,
        limit,
        sort: match args.and_then(|a| a.sort) {
            None | Some(SortKey::Size) => query::SortKey::Size,
            Some(SortKey::Count) => query::SortKey::Count,
            Some(SortKey::Mtime) => query::SortKey::Mtime,
            Some(SortKey::Path) => query::SortKey::Path,
        },
        reverse: args.is_some_and(|a| a.reverse),
        warnings,
        scanned,
    })
}

/// The shared filters the flags ask for, with each as given; and whether
/// a date was read as UTC for want of zoneinfo.
fn filters(
    args: &QueryArgs,
    now: i64,
    depth: DepthUse,
    stale: bool,
) -> Result<(Filters, bool), Failure> {
    let usage = |flag: &str, e: String| Failure::new(Exit::Usage, "usage", format!("{flag} {e}"));
    let mut f = Filters::default();
    if !args.glob.is_empty() {
        f.glob = Some(
            query::globs(args.glob.iter().map(String::as_str)).map_err(|e| usage("--glob", e))?,
        );
        f.given
            .extend(args.glob.iter().map(|g| format!("--glob={g}")));
    }
    if let Some(re) = &args.regex {
        f.regex = Some(Regex::new(re).map_err(|e| usage("--regex", e.to_string()))?);
        f.given.push(format!("--regex={re}"));
    }
    if args.invert {
        f.invert = true;
        f.given.push("--invert".to_owned());
    }
    if !args.exclude.is_empty() {
        f.exclude = Some(
            query::globs(args.exclude.iter().map(String::as_str))
                .map_err(|e| usage("--exclude", e))?,
        );
        f.given
            .extend(args.exclude.iter().map(|g| format!("--exclude={g}")));
    }
    if !args.kinds.is_empty() {
        f.kinds = args
            .kinds
            .iter()
            .map(|k| match k {
                KindArg::File => "file",
                KindArg::Dir => "dir",
                KindArg::Symlink => "symlink",
                KindArg::Other => "other",
            })
            .collect();
        f.given.push(format!("--type={}", f.kinds.join(",")));
    }
    if !stale && let Some(min) = args.min_size {
        f.min_size = Some(min);
        f.given.push(format!("--min-size={min}"));
    }
    if let Some(max) = args.max_size {
        f.max_size = Some(max);
        f.given.push(format!("--max-size={max}"));
    }
    if let Some(min) = args.min_depth {
        f.min_depth = Some(min);
        f.given.push(format!("--min-depth={min}"));
    }
    if depth == DepthUse::Filters
        && let Some(max) = args.depth
    {
        f.max_depth = Some(max);
        f.given.push(format!("--max-depth={max}"));
    }
    f.time = match args.time {
        TimeArg::Mtime => TimeField::Mtime,
        TimeArg::Atime => TimeField::Atime,
        TimeArg::Ctime => TimeField::Ctime,
        TimeArg::Btime => TimeField::Btime,
    };
    let mut tz_unavailable = false;
    if !stale && let Some(age) = &args.older_than {
        let ns = timegrammar::parse_age(age).map_err(|e| usage("--older-than", e))?;
        f.before = Some(now.saturating_sub(ns));
        f.given.push(format!("--older-than={age}"));
    }
    if let Some(age) = &args.newer_than {
        let ns = timegrammar::parse_age(age).map_err(|e| usage("--newer-than", e))?;
        f.after = Some(now.saturating_sub(ns));
        f.given.push(format!("--newer-than={age}"));
    }
    if let Some(date) = &args.since {
        let d = timegrammar::parse_date(date).map_err(|e| usage("--since", e))?;
        tz_unavailable |= d.tz_unavailable;
        f.after = Some(f.after.map_or(d.ns, |a| a.max(d.ns)));
        f.given.push(format!("--since={date}"));
    }
    if let Some(date) = &args.until {
        let d = timegrammar::parse_date(date).map_err(|e| usage("--until", e))?;
        tz_unavailable |= d.tz_unavailable;
        f.before = Some(f.before.map_or(d.ns, |b| b.min(d.ns)));
        f.given.push(format!("--until={date}"));
    }
    if (f.before.is_some() || f.after.is_some()) && f.time != TimeField::Mtime {
        let name = match f.time {
            TimeField::Mtime => "mtime",
            TimeField::Atime => "atime",
            TimeField::Ctime => "ctime",
            TimeField::Btime => "btime",
        };
        f.given.push(format!("--time={name}"));
    }
    Ok((f, tz_unavailable))
}

/// The envelope of a verb's page, and the footer for people.
fn envelope<T>(
    prepared: Prepared,
    walk: &WalkArgs,
    verb: &'static str,
    depth: Option<u32>,
    page: Page<T>,
    extra: Vec<Warning>,
    output: &Output,
) -> (Envelope<T>, String) {
    let Prepared {
        scanned,
        size,
        filters,
        limit,
        sort,
        reverse,
        warnings: flag_warnings,
        ..
    } = prepared;
    let block = scan::scan_block(&scanned, walk, size);
    let footer = scan::footer(&block, &scanned, output);
    let mut warnings = scan::warnings(&scanned);
    warnings.extend(flag_warnings);
    warnings.extend(extra);
    let returned_count = page.entries.len() as u64;
    let path = block.root.clone();
    let envelope = Envelope {
        ok: true,
        schema_version: SCHEMA_VERSION,
        tool: Tool::CURRENT,
        scan: block,
        query: QueryBlock {
            command: verb,
            path,
            depth,
            limit: limit.map(|l| l as u64),
            sort: sort.describe(size, reverse),
            filters: filters.given,
        },
        entries: page.entries,
        returned_count,
        total_count: page.total_count,
        truncated: returned_count < page.total_count,
        next_cursor: None,
        warnings,
        next: Vec::new(),
    };
    (envelope, footer)
}

/// The failure an engine error is.
fn core_failure(error: CoreError) -> Failure {
    match error {
        CoreError::AttrsNotCollected(_) => {
            Failure::new(Exit::Runtime, "attrs_not_collected", error.to_string())
                .hint("Rerun the command with --attrs full")
        }
        other => Failure::new(Exit::Runtime, "io_error", other.to_string()),
    }
}
