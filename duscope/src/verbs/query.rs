//! The list verbs on a fresh scan: `top`, `tree`, `by-type`, `stale`, and
//! the rows `scan` prints. Each turns its flags into a query of
//! `duscope_core::query`, runs it on the arena, and writes the envelope.

use std::path::Path;

use duscope_core::CoreError;
use duscope_core::envelope::{
    ByTypeEntry, Envelope, QueryBlock, SCHEMA_VERSION, StaleEntry, Tool, TopEntry, Warning,
};
use duscope_core::query::{
    self, ByTypeQuery, DEFAULT_MIN_SIZE, Filters, Page, Regex, SizeKind, StaleQuery, TimeField,
    TopQuery, TreeQuery, TreeView, timegrammar,
};
use duscope_core::rules::Classes;
use duscope_walk::VolumeInfo;

use super::scan::{self, Scanned};
use super::{Exit, Failure};
use crate::cli::{
    ByTypeArgs, KindArg, QueryArgs, ScanArgs, SortKey, StaleArgs, TimeField as TimeArg, TopArgs,
    TreeArgs, WalkArgs,
};
use crate::out::{Format, Output};

/// `--limit`'s default, and its cap without `--format ndjson` (which
/// streams every row).
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
    // The map's flags are settled first: a refused one costs no scan.
    let style = if output.format == Format::Treemap {
        Some(super::treemap::style(args, output)?)
    } else {
        None
    };
    let scanned = scan::scan(&args.walk, &args.exclude)?;
    if let Some(style) = &style {
        return super::treemap::draw(&scanned, args, output, style);
    }
    let (envelope, footer) = top_envelope(scanned, &args.walk, None, "scan", false, true, output)?;
    output.list(&envelope, &footer)
}

/// `top`.
pub fn top(args: &TopArgs, output: &Output) -> Result<(), Failure> {
    limit(Some(&args.query), output)?;
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
    limit(Some(&args.query), output)?;
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
pub fn by_type(args: &ByTypeArgs, output: &Output) -> Result<(), Failure> {
    // --classes, else the configuration's [classes] extra.
    let label = if args.classes == args.walk.classes_extra {
        "[classes] extra"
    } else {
        "--classes"
    };
    let classes = classes(args.classes.as_deref(), label)?;
    limit(Some(&args.query), output)?;
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
        sort: prepared.sort,
        reverse: prepared.reverse,
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
    limit(Some(&args.query), output)?;
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
    warnings.extend(scan_age(
        prepared.now,
        prepared.scanned.finished_ns,
        older_than,
        older,
    ));
    if args.atime {
        warnings.extend(atime_unreliable(prepared.scanned.arena.volumes()));
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
    let now = scan::now(walk)?;
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
        scanned,
    })
}

/// The rows a list returns: `--limit`, 25 by default, at most 100; every
/// row under `--format ndjson`. A `--limit` above 100 is a usage error,
/// raised before the scan.
fn limit(args: Option<&QueryArgs>, output: &Output) -> Result<Option<usize>, Failure> {
    if output.format == Format::Ndjson {
        return Ok(None);
    }
    match args.and_then(|a| a.limit) {
        Some(asked) if asked > MAX_LIMIT => Err(Failure::new(
            Exit::Usage,
            "usage",
            format!("--limit {asked} is more than the {MAX_LIMIT} rows a list returns"),
        )
        .hint("Rerun duscope with --format ndjson to stream every row")),
        asked => Ok(Some(asked.unwrap_or(DEFAULT_LIMIT))),
    }
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
fn atime_unreliable(volumes: &[VolumeInfo]) -> Option<Warning> {
    let lagging: Vec<(&VolumeInfo, &str)> = volumes
        .iter()
        .filter(|v| v.entered())
        .filter_map(|v| {
            let option = v
                .options
                .iter()
                .find(|o| matches!(o.as_str(), "noatime" | "relatime" | "lazytime"))?;
            Some((v, option.as_str()))
        })
        .collect();
    let message = match lagging.as_slice() {
        [] => return None,
        [(volume, option)] => format!(
            "{} is mounted {option}, so its access times lag reads",
            volume.mount_point.display()
        ),
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
