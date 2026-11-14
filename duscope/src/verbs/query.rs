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
    SizeKind, StaleQuery, StaleRows, TimeField, TopQuery, TopRows, TreeQuery, TreeView, query_hash,
    timegrammar,
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
    if let Some(age) = &args.reuse {
        if style.is_some() {
            return Err(Failure::new(
                Exit::Usage,
                "usage",
                "--reuse prints a saved scan's rows, and the treemap is drawn only from a scan run now",
            )
            .hint("Run duscope scan --reuse with --treemap never, --plain or --json, or draw the saved scan with duscope view"));
        }
        if let Some(saved) = source::reusable(&args.walk, &args.exclude, age, index_dir)? {
            return reused(saved, args, output);
        }
    }
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
        if let Some(id) = &id {
            source::saved_as(&mut envelope, id.to_string());
        }
        let file = id.as_ref().and_then(|id| {
            index_dir
                .map(Path::to_owned)
                .or_else(|| duscope_core::index::default_dir(|name| std::env::var_os(name)))
                .map(|dir| duscope_core::index::index_path(&dir, id))
        });
        envelope.next = next::list(&envelope, file.as_deref(), args.walk.path.as_deref());
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

/// `scan --reuse`, when a saved scan answers: its rows, as a scan of the
/// tree would print them, and nothing saved.
fn reused(saved: Source, args: &ScanArgs, output: &Output) -> Result<(), Failure> {
    let file = saved.index_file();
    if let Some(id) = saved.scan_id() {
        log::info!("printing the saved scan {id} in place of a scan");
    }
    let (mut envelope, footer, _) =
        top_envelope(saved, &args.walk, None, "scan", false, true, None, output)?;
    envelope.next = next::list(&envelope, file.as_deref(), args.walk.path.as_deref());
    output.list(&envelope, &footer)
}

/// `top`.
pub fn top(args: &TopArgs, index_dir: Option<&Path>, output: &Output) -> Result<(), Failure> {
    top_written(args, index_dir, output, output.streams())
}

/// `top`, its rows streamed when `stream` ([`write_ranked`]).
fn top_written(
    args: &TopArgs,
    index_dir: Option<&Path>,
    output: &Output,
    stream: bool,
) -> Result<(), Failure> {
    if output.fields_first::<TopEntry>("top")? {
        return Ok(());
    }
    limit(Some(&args.query), output)?;
    let (cursor, index) = cursor_of(&args.query, &args.index)?;
    let source = source::source(&args.walk, &index, index_dir)?;
    let asked = top_asked(
        source,
        &args.walk,
        Some(&args.query),
        "top",
        args.files,
        !args.no_cut_through,
        cursor.as_ref(),
        output,
    )?;
    let rows = query::top_rows(&*asked.prepared.source.view(), &asked.q);
    let source = write_ranked(
        asked.prepared,
        &args.walk,
        "top",
        asked.listed,
        &rows,
        Vec::new(),
        asked.hash.as_deref(),
        output,
        stream,
    )?;
    source.finish(&args.walk, output)
}

/// What `top` (or `scan`) asks of a source: the preparation, the query,
/// the depth the rows are listed at, and the hash a cursor carries.
struct TopAsked {
    prepared: Prepared,
    q: TopQuery,
    listed: Option<u32>,
    hash: Option<String>,
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
    let asked = top_asked(
        source,
        walk,
        query,
        verb,
        files,
        cut_through,
        cursor,
        output,
    )?;
    let page = query::top(&*asked.prepared.source.view(), &asked.q);
    Ok(envelope(
        asked.prepared,
        walk,
        verb,
        asked.listed,
        page,
        Vec::new(),
        asked.hash.as_deref(),
        output,
    ))
}

/// [`TopAsked`] of `top` (or `scan`) on a source, from `cursor`'s page.
// The verb's switches, each a flag of its own.
#[allow(clippy::too_many_arguments, clippy::fn_params_excessive_bools)]
fn top_asked(
    source: Source,
    walk: &WalkArgs,
    query: Option<&QueryArgs>,
    verb: &'static str,
    files: bool,
    cut_through: bool,
    cursor: Option<&Cursor>,
    output: &Output,
) -> Result<TopAsked, Failure> {
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
    Ok(TopAsked {
        prepared,
        q,
        listed,
        hash,
    })
}

/// A verb's rows, ranked, and built one at a time from their ids.
trait Ranked {
    /// The row a list writes.
    type Row: Row + Hinted;
    /// The rows' ids, in order.
    fn ids(&self) -> &[u32];
    /// The row of `id`.
    fn row(&self, view: &dyn TreeView, id: u32) -> Self::Row;
    /// The page with no row built: its count, cursor and truncation.
    fn head(&self) -> Page<Self::Row>;
}

impl Ranked for TopRows {
    type Row = TopEntry;

    fn ids(&self) -> &[u32] {
        &self.ids
    }

    fn row(&self, view: &dyn TreeView, id: u32) -> TopEntry {
        TopRows::row(self, view, id)
    }

    fn head(&self) -> Page<TopEntry> {
        Page {
            entries: Vec::new(),
            total_count: self.total_count,
            next: self.next,
            truncated: self.truncated,
        }
    }
}

impl Ranked for StaleRows {
    type Row = StaleEntry;

    fn ids(&self) -> &[u32] {
        &self.ids
    }

    fn row(&self, view: &dyn TreeView, id: u32) -> StaleEntry {
        StaleRows::row(self, view, id)
    }

    fn head(&self) -> Page<StaleEntry> {
        Page {
            entries: Vec::new(),
            total_count: self.total_count,
            next: self.next,
            truncated: self.truncated,
        }
    }
}

/// Writes a verb's ranked `rows` and gives the source back. With `stream`
/// (a format that [`Output::streams`]), each row is built as it is written
/// and none is held: the saved scan is checked before the first row, and
/// each row once built ([`Source::failed`]). A row built from a failed read
/// is empty, as is every row after it, so the rows end before it, and the
/// failure is returned (`ndjson` has no trailer). Otherwise every row is
/// built first, as the envelope's entries (which the hints read), and the
/// scan checked before they are written.
// The parts of an envelope, each from where the verb has it.
#[allow(clippy::too_many_arguments)]
fn write_ranked<R: Ranked>(
    prepared: Prepared,
    walk: &WalkArgs,
    verb: &'static str,
    depth: Option<u32>,
    rows: &R,
    extra: Vec<Warning>,
    hash: Option<&str>,
    output: &Output,
    stream: bool,
) -> Result<Source, Failure> {
    let mut page = rows.head();
    if !stream {
        let view = prepared.source.view();
        page.entries = rows.ids().iter().map(|&id| rows.row(&*view, id)).collect();
        drop(view);
        let (envelope, footer, source) =
            envelope(prepared, walk, verb, depth, page, extra, hash, output);
        source.check()?;
        output.list(&envelope, &footer)?;
        return Ok(source);
    }
    let (mut envelope, footer, source) =
        envelope(prepared, walk, verb, depth, page, extra, hash, output);
    envelope.returned_count = rows.ids().len() as u64;
    source.check()?;
    let view = source.view();
    let built = rows
        .ids()
        .iter()
        .map(|&id| rows.row(&*view, id))
        .take_while(|_| !source.failed());
    output.list_rows(&envelope, built, || source.check(), &footer)?;
    drop(view);
    Ok(source)
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
    stale_written(args, index_dir, output, output.streams())
}

/// `stale`, its rows streamed when `stream` ([`write_ranked`]).
fn stale_written(
    args: &StaleArgs,
    index_dir: Option<&Path>,
    output: &Output,
    stream: bool,
) -> Result<(), Failure> {
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
    let rows = query::stale_rows(&*prepared.source.view(), &q).map_err(core_failure)?;
    let mut warnings = Vec::new();
    warnings.extend(
        scan_age(
            prepared.now,
            prepared.source.finished_ns(),
            older_than,
            older,
        )
        .map(|age| Warning {
            hint: scan_age_hint(&prepared.source),
            ..age
        }),
    );
    if args.atime {
        warnings.extend(prepared.source.atime_unreliable());
    }
    let source = write_ranked(
        prepared,
        &args.walk,
        "stale",
        args.query.depth,
        &rows,
        warnings,
        Some(&hash),
        output,
        stream,
    )?;
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

/// What is wrong with a pattern, from the `regex` crate's text about it, for
/// a failure's message, which is one sentence. That text draws the pattern
/// over several lines with a caret under the mistake, and its last line says
/// what the mistake is (`error: unclosed group`); its other error, a pattern
/// too large to compile, is one line already.
fn regex_mistake(text: &str) -> &str {
    let last = text.rsplit('\n').next().unwrap_or(text);
    last.strip_prefix("error: ").unwrap_or(last)
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
        let refused = |e: String| usage("--regex", format!("{re:?}: {}", regex_mistake(&e)));
        f.regex = Some(Regex::new(re).map_err(|e| refused(e.to_string()))?);
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
    let block = source.scan_block(walk, size, Some(source::LISTED_EXCLUDED));
    let footer = source.footer(&block, output);
    let mut warnings = source.warnings(walk);
    // A list verb takes --strict; the rows of scan, rescan and import do not.
    if !matches!(verb, "scan" | "rescan" | "import") {
        scan::offer_strict(&mut warnings);
    }
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
    warnings.extend(source::excluded_truncated(&block, Some(&path)));
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
    envelope.next = next::list(
        &envelope,
        source.index_file().as_deref(),
        walk.path.as_deref(),
    );
    (envelope, footer, source)
}

/// `scan_age`: the scan finished longer ago than a tenth of the window
/// `stale` asks about (`older`, `older_than` nanoseconds), both read on the
/// clock the scan was timed by. A fresh scan never is. The hint is the
/// source's, [`scan_age_hint`].
fn scan_age(now: i64, finished_ns: i64, older_than: i64, older: &str) -> Option<Warning> {
    let age = now.saturating_sub(finished_ns);
    (age > older_than / 10).then(|| Warning {
        code: "scan_age",
        message: format!(
            "the scan is {} h old, more than a tenth of the {older} window",
            crate::out::thousands(u64::try_from(age / 3_600_000_000_000).unwrap_or(0))
        ),
        count: None,
        hint: None,
    })
}

/// `scan_age`'s hint: `stale` reads the same saved scan however often it
/// runs, so the command that measures the tree again comes first, a rescan
/// of the saved scan or, for an import, a new scan of its root. A fresh
/// scan, never old, has none.
fn scan_age_hint(source: &Source) -> Option<String> {
    let Source::Saved { index, .. } = source else {
        return None;
    };
    Some(format!(
        "Run {}, then duscope stale again, to measure from a fresh scan",
        super::clean::measure_again(index)
    ))
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
    use std::path::{Path, PathBuf};

    use clap::Parser;
    use duscope_core::query::{self, StaleQuery, TreeView, timegrammar};
    use duscope_walk::{Normalization, Skip, VolumeClass, VolumeInfo};

    use super::{
        DepthUse, Owns, atime_unreliable, prepare, scan_age, source, stale_written, top_asked,
        top_written, write_ranked,
    };
    use crate::cli::{Cli, Verb};
    use crate::out::{Capture, Fields, Format, Output, Units};
    use crate::verbs::Failure;

    const HOUR: i64 = 3_600_000_000_000;

    /// A tree of 9 directories and 90 files of assorted sizes (one
    /// directory holding most of its parent's bytes, for cut-through),
    /// saved in `env`'s index directory: its root and the scan's id.
    fn saved_tree(env: &duscope_testkit::TestEnv) -> (PathBuf, String) {
        let root = env.work_dir().canonicalize().unwrap().join("tree");
        let dirs = [
            "a",
            "a/big",
            "a/big/deeper",
            "b",
            "b/c",
            "b/c/d",
            "e e",
            "f-g",
            "f",
        ];
        for dir in dirs {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        for i in 0..90usize {
            let dir = dirs[i % dirs.len()];
            let size = if dir == "a/big/deeper" {
                1 << 20
            } else {
                (i * 997) % 20_000
            };
            std::fs::write(root.join(dir).join(format!("{i:02}.txt")), vec![b'x'; size]).unwrap();
        }
        let mut argv: Vec<std::ffi::OsString> = ["duscope", "scan", "--save", "--json"]
            .into_iter()
            .map(Into::into)
            .collect();
        argv.extend([root.clone().into(), "--index-dir".into()]);
        argv.extend([env.index_dir().into(), "--config".into(), "none".into()]);
        let (code, written) = crate::run_captured(argv);
        assert_eq!(code, 0, "{}", String::from_utf8_lossy(&written));
        let envelope: serde_json::Value = serde_json::from_slice(&written).unwrap();
        (root, envelope["scan"]["id"].as_str().unwrap().to_owned())
    }

    /// An output in `format` with `fields`, `--header` and `-0` as given,
    /// that writes to the buffer returned beside it.
    fn captured(format: Format, fields: Option<&Fields>, header_null: bool) -> (Output, Capture) {
        let capture = Capture::default();
        let output = Output {
            format,
            color: false,
            units: Units::default(),
            null: header_null,
            header: header_null,
            quiet: true,
            fields: fields.cloned(),
            file: None,
            max_chars: 0,
            spill_dir: None,
            capture: Some(capture.clone()),
        };
        (output, capture)
    }

    /// `top` and `stale`, each written collected and streamed to the same
    /// output, give the same bytes: `ndjson` (every row, the trailer's
    /// counts), with `--fields` and `--detail`, and `--plain` with `--all`,
    /// with `--fields`, `--header` and `-0`, and with a limit.
    #[test]
    fn p2_qry_13_streamed_rows_are_the_collected_rows_byte_for_byte() {
        let env = duscope_testkit::TestEnv::new();
        let (root, id) = saved_tree(&env);
        let dir = env.index_dir();
        let named = |names: &[&str]| Fields::Named(names.iter().map(|&n| n.to_owned()).collect());
        let verbs: [(&str, &[&str], Fields); 3] = [
            (
                "top",
                &["--files"],
                named(&["path", "allocated_bytes", "kind"]),
            ),
            (
                "top",
                &["--depth", "2"],
                named(&["via", "file_count", "path"]),
            ),
            (
                "stale",
                &["--older-than", "0h", "--min-size", "0"],
                named(&["mtime", "nlink", "path"]),
            ),
        ];
        for (verb, flags, fields) in &verbs {
            let shapes: [(Format, Option<&Fields>, bool, &[&str]); 6] = [
                (Format::Ndjson, None, false, &[]),
                (Format::Ndjson, Some(fields), false, &[]),
                (Format::Ndjson, Some(&Fields::Detail), false, &[]),
                (Format::Plain, None, false, &["--all"]),
                (Format::Plain, Some(fields), true, &["--all"]),
                (Format::Plain, None, false, &["--limit", "3"]),
            ];
            for (format, chosen, header_null, extra) in shapes {
                let mut line = vec!["duscope", verb, root.to_str().unwrap(), "--scan", &id];
                line.extend_from_slice(flags);
                line.extend_from_slice(extra);
                let cli = Cli::try_parse_from(&line).unwrap();
                let written = |stream: bool| {
                    let (output, capture) = captured(format, chosen, header_null);
                    match &cli.verb {
                        Some(Verb::Top(args)) => top_written(args, Some(&dir), &output, stream),
                        Some(Verb::Stale(args)) => stale_written(args, Some(&dir), &output, stream),
                        _ => unreachable!(),
                    }
                    .unwrap();
                    capture.take()
                };
                let (collected, streamed) = (written(false), written(true));
                let rows = collected.iter().filter(|&&b| b == b'\n' || b == 0).count();
                assert!(rows >= 3, "{line:?} {format:?}: {rows} lines");
                assert_eq!(
                    String::from_utf8_lossy(&streamed),
                    String::from_utf8_lossy(&collected),
                    "{line:?} {format:?} {chosen:?}"
                );
            }
        }
    }

    /// `top` or `stale` as `cli` asks, on the saved scan in `dir`, written
    /// to `output` as the verb writes it (without `stale`'s warnings),
    /// streamed when `stream`. With `fail_at`, the read of that row
    /// (counted from 1) fails: its id is the one past the scan's last
    /// entry, which the index fails to read as it fails a read of a file
    /// overwritten under it, and from then on every entry reads as empty.
    fn ranked(
        cli: &Cli,
        dir: &Path,
        output: &Output,
        stream: bool,
        fail_at: Option<usize>,
    ) -> Result<(), Failure> {
        let fail = |ids: &mut [u32], view: &dyn TreeView| {
            if let Some(at) = fail_at {
                ids[at - 1] = view.len();
            }
        };
        match &cli.verb {
            Some(Verb::Top(args)) => {
                let source = source::source(&args.walk, &args.index, Some(dir))?;
                let asked = top_asked(
                    source,
                    &args.walk,
                    Some(&args.query),
                    "top",
                    args.files,
                    !args.no_cut_through,
                    None,
                    output,
                )?;
                let view = asked.prepared.source.view();
                let mut rows = query::top_rows(&*view, &asked.q);
                fail(&mut rows.ids, &*view);
                drop(view);
                write_ranked(
                    asked.prepared,
                    &args.walk,
                    "top",
                    asked.listed,
                    &rows,
                    Vec::new(),
                    asked.hash.as_deref(),
                    output,
                    stream,
                )?;
            }
            Some(Verb::Stale(args)) => {
                let source = source::source(&args.walk, &args.index, Some(dir))?;
                let owns = Owns {
                    min_size: true,
                    older_than: true,
                };
                let prepared = prepare(
                    source,
                    &args.walk,
                    Some(&args.query),
                    output,
                    DepthUse::Filters,
                    owns,
                )?;
                let q = StaleQuery {
                    now: prepared.now,
                    older_than: 0,
                    atime: false,
                    min_size: 0,
                    filters: prepared.filters.clone(),
                    sort: prepared.sort,
                    reverse: prepared.reverse,
                    limit: prepared.limit,
                    size: prepared.size,
                    after: None,
                };
                let view = prepared.source.view();
                let mut rows = query::stale_rows(&*view, &q).unwrap();
                fail(&mut rows.ids, &*view);
                drop(view);
                write_ranked(
                    prepared,
                    &args.walk,
                    "stale",
                    args.query.depth,
                    &rows,
                    Vec::new(),
                    None,
                    output,
                    stream,
                )?;
            }
            _ => unreachable!(),
        }
        Ok(())
    }

    /// A read of the saved scan that fails at row n of a ranked list ends
    /// the streamed rows there: stdout holds the header and the n - 1 rows
    /// before it, each the row the collected list writes, and none built
    /// from the failed read or after it (empty, the scan's root for a
    /// path); `ndjson` has no `end` line, and the error envelope main
    /// writes next is its last line; the failure is `index_corrupt`. A
    /// format that collects the rows (JSON) writes none of them.
    #[test]
    fn p2_qry_13_a_read_that_fails_mid_stream_ends_the_rows_there() {
        let env = duscope_testkit::TestEnv::new();
        let (root, id) = saved_tree(&env);
        let dir = env.index_dir();
        let named = |names: &[&str]| Fields::Named(names.iter().map(|&n| n.to_owned()).collect());
        let verbs: [(&[&str], Fields); 2] = [
            (
                &["top", "--files"],
                named(&["name", "allocated_bytes", "path"]),
            ),
            (&["stale"], named(&["name", "mtime", "path"])),
        ];
        for (verb, fields) in &verbs {
            let mut line = vec!["duscope"];
            line.extend_from_slice(verb);
            line.extend([root.to_str().unwrap(), "--scan", &id, "--all"]);
            let cli = Cli::try_parse_from(&line).unwrap();
            let shapes: [(Format, Option<&Fields>, bool); 4] = [
                (Format::Ndjson, None, false),
                (Format::Ndjson, Some(fields), false),
                (Format::Plain, None, false),
                (Format::Plain, Some(fields), true),
            ];
            for (format, chosen, header_null) in shapes {
                let (output, capture) = captured(format, chosen, header_null);
                ranked(&cli, &dir, &output, false, None).unwrap();
                let whole = capture.take();
                let terminator = if header_null { 0 } else { b'\n' };
                let records: Vec<&[u8]> = whole.split_inclusive(|&b| b == terminator).collect();
                // The lines before the rows (ndjson's header, --header's
                // names) and after them (ndjson's trailer).
                let ndjson = format == Format::Ndjson;
                let before = usize::from(ndjson || header_null);
                let rows = records.len() - before - usize::from(ndjson);
                assert!(rows >= 50, "{line:?} {format:?}: {rows} rows");
                for at in [1, 2, rows / 2, rows] {
                    let shape = format!("{line:?} {format:?} {chosen:?}, row {at} of {rows}");
                    let (output, capture) = captured(format, chosen, header_null);
                    let failure = ranked(&cli, &dir, &output, true, Some(at)).unwrap_err();
                    assert_eq!(failure.code, "index_corrupt", "{shape}");
                    let written = capture.take();
                    assert_eq!(
                        String::from_utf8_lossy(&written),
                        String::from_utf8_lossy(&records[..before + at - 1].concat()),
                        "{shape}"
                    );
                    if ndjson {
                        output.failure(&failure);
                        let error = capture.take();
                        let lines: Vec<serde_json::Value> = written
                            .split_inclusive(|&b| b == b'\n')
                            .chain([error.as_slice()])
                            .map(|line| serde_json::from_slice(line).unwrap())
                            .collect();
                        let (last, lines) = lines.split_last().unwrap();
                        assert_eq!(last["error"]["code"], "index_corrupt", "{shape}");
                        assert_eq!(lines[0]["type"], "scan", "{shape}");
                        for row in &lines[1..] {
                            assert_eq!(row["type"], "entry", "{shape}");
                            assert_ne!(row["name"], "", "{shape}");
                        }
                    } else if chosen.is_some() {
                        // `name` is the first field: no row's is empty.
                        let rows = written.split_inclusive(|&b| b == terminator).skip(before);
                        assert!(rows.into_iter().all(|row| row[0] != b'\t'), "{shape}");
                    }
                }
            }
            // JSON holds its rows for the envelope: a failed read writes
            // none, collected by the verb or by list_rows.
            for stream in [false, true] {
                let (output, capture) = captured(Format::Json, None, false);
                let failure = ranked(&cli, &dir, &output, stream, Some(20)).unwrap_err();
                assert_eq!(failure.code, "index_corrupt", "{line:?} {stream}");
                assert!(capture.take().is_empty(), "{line:?} {stream}");
            }
        }
    }

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
            fsevents: None,
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
