//! The list verbs: `top`, `tree`, `by-type`, `stale`, `caches`, and the
//! rows `scan` prints. Each turns its flags into a query of
//! `duscope_core::query`, runs it on its source (a saved scan, or a scan
//! run for it), and writes the envelope.

use std::path::Path;

use duscope_core::CoreError;
use duscope_core::envelope::{
    ByTypeEntry, CacheEntry, Envelope, ProjectEntry, QueryBlock, SCHEMA_VERSION, StaleEntry, Tool,
    TopEntry, TreeEntry, Warning,
};
use duscope_core::query::{
    self, After, ByTypeQuery, CachesQuery, Cursor, DEFAULT_MIN_SIZE, Filters, Page, Regex,
    SizeKind, StaleQuery, TimeField, TopQuery, TreeQuery, query_hash, timegrammar,
};
use duscope_core::rules::Classes;
use duscope_walk::VolumeInfo;

use super::next::{self, Hinted};
use super::scan;
use super::source::{self, Source, core_failure};
use super::{Exit, Failure};
use crate::cli::{
    ByTypeArgs, CachesArgs, IndexArgs, KindArg, QueryArgs, ScanArgs, SortKey, StaleArgs,
    TimeField as TimeArg, TopArgs, TreeArgs, WalkArgs,
};
use crate::out::{Format, Output, Row};

/// `--limit`'s default, and its cap without `--format ndjson` (which
/// streams every row).
const DEFAULT_LIMIT: usize = 25;
const MAX_LIMIT: usize = 100;

/// `stale`'s window without `--older-than`.
const DEFAULT_STALE_AGE: &str = "180d";

/// What every list verb settles before it runs its query.
struct Prepared {
    source: Source,
    size: SizeKind,
    now: i64,
    filters: Filters,
    limit: Option<usize>,
    sort: query::SortKey,
    reverse: bool,
    /// Warnings the flags themselves raise (`tz_unavailable`).
    warnings: Vec<Warning>,
}

/// The shared flags a verb reads itself, not as filters.
#[derive(Debug, Clone, Copy, Default)]
struct Owns {
    /// `--min-size` (`stale`, `caches`).
    min_size: bool,
    /// `--older-than` (`stale`).
    older_than: bool,
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
pub fn scan(args: &ScanArgs, index_dir: Option<&Path>, output: &Output) -> Result<(), Failure> {
    if output.format != Format::Treemap && output.fields_first::<TopEntry>("scan")? {
        return Ok(());
    }
    // The map's flags are settled first: a refused one costs no scan.
    let style = if output.format == Format::Treemap {
        Some(super::treemap::style(args, output)?)
    } else {
        None
    };
    let scanned = scan::scan(&args.walk, &args.exclude)?;
    // The id is chosen before anything is written, so the output can carry
    // it; the index is written only after the output is out.
    let id = args.save.then(|| scan::scan_id(&scanned)).transpose()?;
    let scanned = if let Some(style) = &style {
        super::treemap::draw(&scanned, args, output, style)?;
        scanned
    } else {
        // scan saves its own scan below, with its excludes and its line.
        let fresh = Source::Fresh {
            scanned: Box::new(scanned),
            save: None,
        };
        let (mut envelope, footer, fresh) =
            top_envelope(fresh, &args.walk, None, "scan", false, true, None, output)?;
        envelope.scan.id = id.as_ref().map(ToString::to_string);
        envelope.next = next::list(&envelope);
        output.list(&envelope, &footer)?;
        match fresh {
            Source::Fresh { scanned, .. } => *scanned,
            Source::Saved { .. } => return Ok(()),
        }
    };
    match &id {
        Some(id) => scan::save(
            &scanned,
            &args.walk,
            &args.exclude,
            id,
            index_dir,
            output,
            true,
        ),
        None => Ok(()),
    }
}

/// `top`.
pub fn top(args: &TopArgs, index_dir: Option<&Path>, output: &Output) -> Result<(), Failure> {
    if output.fields_first::<TopEntry>("top")? {
        return Ok(());
    }
    limit(Some(&args.query), output)?;
    let (cursor, index) = cursor_of(&args.query, &args.index)?;
    let source = source::source(&args.walk, &index, index_dir)?;
    let (envelope, footer, source) = top_envelope(
        source,
        &args.walk,
        Some(&args.query),
        "top",
        args.files,
        !args.no_cut_through,
        cursor.as_ref(),
        output,
    )?;
    source.check()?;
    output.list(&envelope, &footer)?;
    source.finish(&args.walk, output)
}

/// The envelope and footer of `top` (or `scan`, whose `query` is `None`:
/// the defaults, and no pages) on a source, from `cursor`'s page, and the
/// source back for saving.
// The verb's switches, each a flag of its own.
#[allow(clippy::too_many_arguments, clippy::fn_params_excessive_bools)]
pub(super) fn top_envelope(
    source: Source,
    walk: &WalkArgs,
    query: Option<&QueryArgs>,
    verb: &'static str,
    files: bool,
    cut_through: bool,
    cursor: Option<&Cursor>,
    output: &Output,
) -> Result<(Envelope<TopEntry>, String, Source), Failure> {
    let prepared = prepare(
        source,
        walk,
        query,
        output,
        DepthUse::Lists,
        Owns::default(),
    )?;
    let depth = query.and_then(|q| q.depth);
    // The depth the rows were listed at: --depth 0 lists depth 1.
    let listed = if files {
        depth
    } else {
        Some(depth.unwrap_or(1).max(1))
    };
    let shape = [
        if files { "files" } else { "dirs" },
        if cut_through {
            "cut-through"
        } else {
            "no-cut-through"
        },
    ];
    let hash = query.map(|_| page_hash(&prepared, verb, listed, &shape));
    let after = match (cursor, &hash) {
        (Some(cursor), Some(hash)) => Some(check_cursor(cursor, &prepared, hash)?),
        _ => None,
    };
    let q = TopQuery {
        depth,
        cut_through,
        files,
        filters: prepared.filters.clone(),
        sort: prepared.sort,
        reverse: prepared.reverse,
        limit: prepared.limit,
        size: prepared.size,
        after,
    };
    let page = query::top(&*prepared.source.view(), &q);
    Ok(envelope(
        prepared,
        walk,
        verb,
        listed,
        page,
        Vec::new(),
        hash.as_deref(),
        output,
    ))
}

/// `tree`.
pub fn tree(args: &TreeArgs, index_dir: Option<&Path>, output: &Output) -> Result<(), Failure> {
    if output.fields_first::<TreeEntry>("tree")? {
        return Ok(());
    }
    no_pages(&args.query, "tree")?;
    limit(Some(&args.query), output)?;
    let source = source::source(&args.walk, &args.index, index_dir)?;
    let prepared = prepare(
        source,
        &args.walk,
        Some(&args.query),
        output,
        DepthUse::Lists,
        Owns::default(),
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
    let page = query::tree(&*prepared.source.view(), &q);
    let depth = Some(args.query.depth.unwrap_or(2));
    let (envelope, footer, source) = envelope(
        prepared,
        &args.walk,
        "tree",
        depth,
        page,
        Vec::new(),
        None,
        output,
    );
    source.check()?;
    output.list(&envelope, &footer)?;
    source.finish(&args.walk, output)
}

/// The file classes: the vendored ones, with the table `file` names (from
/// `label`, a flag or a configuration key) merged over them.
pub fn classes(file: Option<&Path>, label: &str) -> Result<Classes, Failure> {
    let Some(file) = file else {
        return Ok(Classes::vendored());
    };
    let text = std::fs::read_to_string(file).map_err(|e| {
        Failure::new(
            Exit::Usage,
            "usage",
            format!("{label} {}: {e}", file.display()),
        )
    })?;
    let user = Classes::parse(&text, &file.display().to_string())
        .map_err(|e| Failure::new(Exit::Usage, "usage", format!("{label} {e}")))?;
    Ok(Classes::vendored().merged(user))
}

/// `by-type`.
pub fn by_type(
    args: &ByTypeArgs,
    index_dir: Option<&Path>,
    output: &Output,
) -> Result<(), Failure> {
    // --classes, else the configuration's [classes] extra.
    let label = if args.classes == args.walk.classes_extra {
        "[classes] extra"
    } else {
        "--classes"
    };
    let classes = classes(args.classes.as_deref(), label)?;
    if output.fields_first::<ByTypeEntry>("by-type")? {
        return Ok(());
    }
    no_pages(&args.query, "by-type")?;
    limit(Some(&args.query), output)?;
    let source = source::source(&args.walk, &args.index, index_dir)?;
    let prepared = prepare(
        source,
        &args.walk,
        Some(&args.query),
        output,
        DepthUse::Filters,
        Owns::default(),
    )?;
    let q = ByTypeQuery {
        classes: &classes,
        examples: args.examples.unwrap_or(3),
        filters: prepared.filters.clone(),
        limit: prepared.limit,
        size: prepared.size,
        sort: prepared.sort,
        reverse: prepared.reverse,
    };
    let page: Page<ByTypeEntry> = query::by_type(&*prepared.source.view(), &q);
    let (envelope, footer, source) = envelope(
        prepared,
        &args.walk,
        "by-type",
        args.query.depth,
        page,
        Vec::new(),
        None,
        output,
    );
    source.check()?;
    output.list(&envelope, &footer)?;
    source.finish(&args.walk, output)
}

/// `stale`.
pub fn stale(args: &StaleArgs, index_dir: Option<&Path>, output: &Output) -> Result<(), Failure> {
    if output.fields_first::<StaleEntry>("stale")? {
        return Ok(());
    }
    let older = args
        .query
        .older_than
        .as_deref()
        .unwrap_or(DEFAULT_STALE_AGE);
    let older_than = timegrammar::parse_age(older)
        .map_err(|e| Failure::new(Exit::Usage, "usage", format!("--older-than {e}")))?;
    limit(Some(&args.query), output)?;
    let (cursor, index) = cursor_of(&args.query, &args.index)?;
    let source = source::source(&args.walk, &index, index_dir)?;
    let mut prepared = prepare(
        source,
        &args.walk,
        Some(&args.query),
        output,
        DepthUse::Filters,
        Owns {
            min_size: true,
            older_than: true,
        },
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
    let hash = page_hash(&prepared, "stale", args.query.depth, &[]);
    let after = cursor
        .as_ref()
        .map(|cursor| check_cursor(cursor, &prepared, &hash))
        .transpose()?;
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
        after,
    };
    let page: Page<StaleEntry> =
        query::stale(&*prepared.source.view(), &q).map_err(core_failure)?;
    let mut warnings = Vec::new();
    warnings.extend(scan_age(
        prepared.now,
        prepared.source.finished_ns(),
        older_than,
        older,
    ));
    if args.atime {
        warnings.extend(prepared.source.atime_unreliable());
    }
    let (envelope, footer, source) = envelope(
        prepared,
        &args.walk,
        "stale",
        args.query.depth,
        page,
        warnings,
        Some(&hash),
        output,
    );
    source.check()?;
    output.list(&envelope, &footer)?;
    source.finish(&args.walk, output)
}

/// `caches`: each cache by default, each project's with `--by-project`.
pub fn caches(args: &CachesArgs, index_dir: Option<&Path>, output: &Output) -> Result<(), Failure> {
    // A scan that leaves the caches out holds none of their bytes.
    if args.walk.exclude_caches {
        return Err(Failure::new(
            Exit::Usage,
            "usage",
            "caches lists the directories --exclude-caches leaves out, so the two do not go together",
        )
        .hint("Run duscope caches without --exclude-caches"));
    }
    let rules = scan::cache_rules(args.walk.caches_extra.as_deref())?;
    let listed = if args.by_project {
        output.fields_first::<ProjectEntry>("caches --by-project")?
    } else {
        output.fields_first::<CacheEntry>("caches")?
    };
    if listed {
        return Ok(());
    }
    if args.by_project {
        no_pages(&args.query, "caches --by-project")?;
    }
    limit(Some(&args.query), output)?;
    let (cursor, index) = cursor_of(&args.query, &args.index)?;
    let source = source::source(&args.walk, &index, index_dir)?;
    let mut prepared = prepare(
        source,
        &args.walk,
        Some(&args.query),
        output,
        DepthUse::Filters,
        Owns {
            min_size: true,
            older_than: false,
        },
    )?;
    // --min-size is the query's own, with its default; by project, it is a
    // project's total.
    let min_size = args
        .query
        .min_size
        .unwrap_or(query::caches::DEFAULT_MIN_SIZE);
    let given = &mut prepared.filters.given;
    given.insert(0, format!("--min-size={min_size}"));
    if args.loose {
        given.push("--loose".to_owned());
    }
    let verb = if args.by_project {
        "caches --by-project"
    } else {
        "caches"
    };
    // The rules decide which directories are rows: a cursor of other
    // rules pages another list.
    let rules_file = args
        .walk
        .caches_extra
        .as_ref()
        .map(|file| file.display().to_string())
        .unwrap_or_default();
    let hash = page_hash(&prepared, verb, args.query.depth, &[&rules_file]);
    let after = cursor
        .as_ref()
        .map(|cursor| check_cursor(cursor, &prepared, &hash))
        .transpose()?;
    let source = &prepared.source;
    let holds = |dir: &Path, name: &str| source.holds_file(dir, name);
    let q = CachesQuery {
        caches: &rules,
        above: &holds,
        loose: args.loose,
        min_size,
        filters: prepared.filters.clone(),
        sort: prepared.sort,
        reverse: prepared.reverse,
        limit: prepared.limit,
        size: prepared.size,
        after,
    };
    if args.by_project {
        let page: Page<ProjectEntry> = query::projects(&*source.view(), &q);
        write(prepared, &args.walk, verb, &args.query, page, None, output)
    } else {
        let page: Page<CacheEntry> = query::caches(&*source.view(), &q);
        write(
            prepared,
            &args.walk,
            verb,
            &args.query,
            page,
            Some(&hash),
            output,
        )
    }
}

/// Writes a verb's page as its envelope, and saves a scan run for it.
fn write<T: Row + Hinted>(
    prepared: Prepared,
    walk: &WalkArgs,
    verb: &'static str,
    query: &QueryArgs,
    page: Page<T>,
    hash: Option<&str>,
    output: &Output,
) -> Result<(), Failure> {
    let (envelope, footer, source) = envelope(
        prepared,
        walk,
        verb,
        query.depth,
        page,
        Vec::new(),
        hash,
        output,
    );
    source.check()?;
    output.list(&envelope, &footer)?;
    source.finish(walk, output)
}

/// `--cursor`, decoded, and the saved scans to read: the one the cursor
/// pages, unless `--scan` names one (which must be the same scan).
fn cursor_of(query: &QueryArgs, index: &IndexArgs) -> Result<(Option<Cursor>, IndexArgs), Failure> {
    let mut index = index.clone();
    let Some(text) = &query.cursor else {
        return Ok((None, index));
    };
    let cursor = Cursor::decode(text).map_err(core_failure)?;
    if index.scan.is_none() {
        index.scan = Some(cursor.scan.clone());
    }
    Ok((Some(cursor), index))
}

/// Refuses `--cursor` for a verb whose lists do not page.
fn no_pages(query: &QueryArgs, verb: &str) -> Result<(), Failure> {
    match query.cursor {
        Some(_) => Err(core_failure(CoreError::InvalidCursor(format!(
            "{verb} does not page: --cursor pages top, stale and caches"
        )))),
        None => Ok(()),
    }
}

/// The hash a cursor carries of the query it pages: the verb, the path,
/// the depth, the sort, the filters as given, and the verb's own switches
/// (`shape`).
fn page_hash(prepared: &Prepared, verb: &str, depth: Option<u32>, shape: &[&str]) -> String {
    let path = prepared
        .source
        .view()
        .root_path()
        .to_string_lossy()
        .into_owned();
    let depth = depth.map(|d| d.to_string()).unwrap_or_default();
    let sort = prepared.sort.describe(prepared.size, prepared.reverse);
    let mut parts: Vec<&str> = vec![verb, &path, &depth, &sort];
    parts.extend(prepared.filters.given.iter().map(String::as_str));
    // Between the filters and the switches, so neither runs into the other.
    parts.push("\u{1}");
    parts.extend(shape);
    query_hash(parts)
}

/// The key the page `cursor` names starts after, when the cursor is this
/// command's: of the scan it reads, its query, and a row of it.
fn check_cursor(cursor: &Cursor, prepared: &Prepared, hash: &str) -> Result<After, Failure> {
    let invalid = |why: &str| core_failure(CoreError::InvalidCursor(format!("--cursor {why}")));
    if prepared.source.scan_id().as_deref() != Some(cursor.scan.as_str()) {
        return Err(invalid(
            "pages another scan than the one this command reads",
        ));
    }
    if cursor.filters != hash {
        return Err(invalid(
            "pages another query: the verb, path, depth, sort and filters must be the ones \
             that returned it",
        ));
    }
    let view = prepared.source.view();
    if !query::after_fits(&*view, &cursor.after, prepared.sort, prepared.size) {
        return Err(invalid("names no row of this scan"));
    }
    Ok(cursor.after)
}

/// Scans' common preparation: the size kind, now, the filters, the limit
/// and the sort; `--strict` turns unreadable directories into exit 4.
fn prepare(
    source: Source,
    walk: &WalkArgs,
    args: Option<&QueryArgs>,
    output: &Output,
    depth: DepthUse,
    owns: Owns,
) -> Result<Prepared, Failure> {
    let unreadable = source.unreadable_dirs();
    if args.is_some_and(|a| a.strict) && unreadable > 0 {
        return Err(Failure::new(
            Exit::Permission,
            "permission_denied",
            format!(
                "{unreadable} directories could not be read, and --strict asks for all of them"
            ),
        )
        .hint("Run without --strict to accept totals that leave them out"));
    }
    let now = scan::now(walk)?;
    let (filters, tz_unavailable) = match args {
        Some(args) => filters(args, now, depth, owns)?,
        None => (Filters::default(), false),
    };
    if filters.needs_full() && !source.view().full_attrs() {
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
    let limit = limit(args, output)?;
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
        source,
    })
}

/// The rows a list returns: `--limit`, 25 by default, at most 100 unless
/// `-o FILE` takes them; every row under `--all` or `--format ndjson`. A
/// `--limit` above 100 is otherwise a usage error, raised before the scan.
fn limit(args: Option<&QueryArgs>, output: &Output) -> Result<Option<usize>, Failure> {
    if output.format == Format::Ndjson || args.is_some_and(|a| a.all) {
        return Ok(None);
    }
    match args.and_then(|a| a.limit) {
        // A file takes any number of rows.
        Some(asked) if asked > MAX_LIMIT && output.file.is_none() => Err(Failure::new(
            Exit::Usage,
            "usage",
            format!("--limit {asked} is more than the {MAX_LIMIT} rows a list returns"),
        )
        .hint("Rerun duscope with --format ndjson or --all to stream every row, or -o FILE")),
        asked => Ok(Some(asked.unwrap_or(DEFAULT_LIMIT))),
    }
}

/// The shared filters the flags ask for, with each as given; and whether
/// a date was read as UTC for want of zoneinfo.
fn filters(
    args: &QueryArgs,
    now: i64,
    depth: DepthUse,
    owns: Owns,
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
    if !owns.min_size
        && let Some(min) = args.min_size
    {
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
    if !owns.older_than
        && let Some(age) = &args.older_than
    {
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

/// The envelope of a verb's page, the footer for people, and the source
/// back.
// The parts of an envelope, each from where the verb has it.
#[allow(clippy::too_many_arguments)]
fn envelope<T: Hinted>(
    prepared: Prepared,
    walk: &WalkArgs,
    verb: &'static str,
    depth: Option<u32>,
    page: Page<T>,
    extra: Vec<Warning>,
    hash: Option<&str>,
    output: &Output,
) -> (Envelope<T>, String, Source) {
    let Prepared {
        source,
        size,
        filters,
        limit,
        sort,
        reverse,
        warnings: flag_warnings,
        ..
    } = prepared;
    let block = source.scan_block(walk, size);
    let footer = source.footer(&block, output);
    let mut warnings = source.warnings(walk);
    warnings.extend(flag_warnings);
    warnings.extend(extra);
    let returned_count = page.entries.len() as u64;
    // The next page's cursor: a saved scan's (or one this command saves),
    // for a verb that pages, when rows remain.
    let next_cursor = match (page.next, hash, &block.id) {
        (Some(after), Some(hash), Some(scan)) => Some(
            Cursor {
                scan: scan.clone(),
                after,
                filters: hash.to_owned(),
            }
            .encode(),
        ),
        _ => None,
    };
    let truncated = page.truncated;
    // The path the rows are under: the scan's root, or the path inside a
    // saved scan's root.
    let path = source.view().root_path().to_string_lossy().into_owned();
    let mut envelope = Envelope {
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
        truncated,
        next_cursor,
        warnings,
        next: Vec::new(),
    };
    envelope.next = next::list(&envelope);
    (envelope, footer, source)
}

/// `scan_age`: the scan finished longer ago than a tenth of the window
/// `stale` asks about (`older`, `older_than` nanoseconds), both read on the
/// clock the scan was timed by. A fresh scan never is.
fn scan_age(now: i64, finished_ns: i64, older_than: i64, older: &str) -> Option<Warning> {
    let age = now.saturating_sub(finished_ns);
    (age > older_than / 10).then(|| Warning {
        code: "scan_age",
        message: format!(
            "the scan is {} h old, more than a tenth of the {older} window",
            crate::out::thousands(u64::try_from(age / 3_600_000_000_000).unwrap_or(0))
        ),
        count: None,
        hint: Some("Rerun duscope stale to measure from a fresh scan".to_owned()),
    })
}

/// `atime_unreliable`: the volumes the scan entered that are mounted
/// `noatime`, `relatime` or `lazytime`, whose access times lag reads.
pub(super) fn atime_unreliable(volumes: &[VolumeInfo]) -> Option<Warning> {
    lagging_atime(
        volumes
            .iter()
            .filter(|v| v.entered())
            .map(|v| (v.mount_point.display().to_string(), v.options.clone())),
    )
}

/// `atime_unreliable` from the volumes a scan entered: each one's mount
/// point and mount options.
pub(super) fn lagging_atime(
    entered: impl Iterator<Item = (String, Vec<String>)>,
) -> Option<Warning> {
    let lagging: Vec<(String, String)> = entered
        .filter_map(|(mount_point, options)| {
            let option = options
                .into_iter()
                .find(|o| matches!(o.as_str(), "noatime" | "relatime" | "lazytime"))?;
            Some((mount_point, option))
        })
        .collect();
    let message = match lagging.as_slice() {
        [] => return None,
        [(mount_point, option)] => {
            format!("{mount_point} is mounted {option}, so its access times lag reads")
        }
        more => format!(
            "{} volumes the scan entered are mounted noatime, relatime or lazytime, so their access times lag reads",
            more.len()
        ),
    };
    Some(Warning {
        code: "atime_unreliable",
        message,
        count: Some(lagging.len() as u64),
        hint: Some(
            "Run duscope stale without --atime, which judges files by their modification and change times"
                .to_owned(),
        ),
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use duscope_core::query::timegrammar;
    use duscope_walk::{Normalization, Skip, VolumeClass, VolumeInfo};

    use super::{atime_unreliable, scan_age};

    const HOUR: i64 = 3_600_000_000_000;

    #[test]
    fn p1_cli_03_stale_warns_scan_age_past_a_tenth_of_its_window() {
        let window = timegrammar::parse_age("180d").unwrap();
        let now = 2_114_380_800_000_000_000;
        // 18 days is a tenth of 180: older than that warns.
        let old = scan_age(now, now - 19 * 24 * HOUR, window, "180d").unwrap();
        assert_eq!(old.code, "scan_age");
        assert_eq!(
            old.message,
            "the scan is 456 h old, more than a tenth of the 180d window"
        );
        assert!(scan_age(now, now - 17 * 24 * HOUR, window, "180d").is_none());
        // A scan timed on the same clock as the query is never old.
        assert!(scan_age(now, now, window, "180d").is_none());
    }

    fn volume(mount_point: &str, options: &[&str], reason: Option<Skip>) -> VolumeInfo {
        VolumeInfo {
            dev: 1,
            mnt_id: 1,
            mount_point: PathBuf::from(mount_point),
            fs_type: "ext4".to_owned(),
            class: VolumeClass::Local,
            read_only: false,
            case_sensitive: Some(true),
            normalization: Normalization::None,
            options: options.iter().map(|&o| o.to_owned()).collect(),
            total_bytes: 0,
            free_bytes: 0,
            reason,
        }
    }

    #[test]
    fn p1_cli_03_atime_unreliable_reads_every_entered_volume() {
        let strict = volume("/", &["rw", "strictatime"], None);
        assert!(atime_unreliable(std::slice::from_ref(&strict)).is_none());
        // A second, noatime volume the scan entered warns, though the
        // first is fine.
        let data = volume("/data", &["rw", "noatime"], None);
        let one = atime_unreliable(&[strict.clone(), data.clone()]).unwrap();
        assert_eq!(one.code, "atime_unreliable");
        assert_eq!(one.count, Some(1));
        assert_eq!(
            one.message,
            "/data is mounted noatime, so its access times lag reads"
        );
        assert!(!one.hint.unwrap().contains("strictatime"));
        // A volume the scan did not enter does not count.
        let skipped = volume("/net", &["relatime"], Some(Skip::Network));
        let still_one = atime_unreliable(&[strict, data.clone(), skipped]).unwrap();
        assert_eq!(still_one.count, Some(1));
        let two = atime_unreliable(&[data, volume("/home", &["relatime"], None)]).unwrap();
        assert_eq!(two.count, Some(2));
    }
}
