//! `diff`: what changed between two saved scans of one tree: both scans'
//! root, or `--under` a path both hold. Other roots are
//! `incompatible_scans`.

use std::path::{Path, PathBuf};

use duscope_core::envelope::{DiffEntry, DiffTotals, Envelope, QueryBlock, SCHEMA_VERSION, Tool};
use duscope_core::index::Index;
use duscope_core::query::{
    self, DEFAULT_MIN_DELTA, DiffQuery, DiffRows, Page, ScanQuery, ScanSelector, SizeKind, Subtree,
    TreeView, clock, resolve_scan,
};
use duscope_core::resolve::{resolve_in_index, resolve_path};

use super::next::Hints;
use super::source::{self, LISTED_EXCLUDED, core_failure, read_block, scanned};
use super::{Exit, Failure};
use crate::cli::{self, DiffArgs};
use crate::out::{Format, Output, thousands};

/// `--limit`'s default, and its cap without `-o` or `--all`.
const DEFAULT_LIMIT: usize = 25;
const MAX_LIMIT: usize = 100;

/// `diff`.
pub fn run(args: &DiffArgs, index_dir: Option<&Path>, output: &Output) -> Result<(), Failure> {
    written(args, index_dir, output, output.streams())
}

/// `diff`, its rows streamed when `stream` ([`write()`]).
fn written(
    args: &DiffArgs,
    index_dir: Option<&Path>,
    output: &Output,
    stream: bool,
) -> Result<(), Failure> {
    let compared = compare(args, index_dir, output)?;
    let (old_view, new_view) = compared.views();
    let (rows, totals) = query::diff_rows(&old_view, &new_view, &compared.q);
    drop((old_view, new_view));
    write(&compared, &rows, totals, output, stream)
}

/// The two saved scans `diff` compares, and what it asks of them.
struct Compared {
    old: Index,
    new: Index,
    /// The entry each scan compares from: the root, or the path both hold.
    bases: (u32, u32),
    /// `--under`, resolved.
    under: Option<PathBuf>,
    /// The index directory the scans were looked for in.
    dir: PathBuf,
    q: DiffQuery,
}

impl Compared {
    /// Each scan from the entry it compares from.
    fn views(&self) -> (Subtree<'_, Index>, Subtree<'_, Index>) {
        (
            Subtree::new(&self.old, self.bases.0),
            Subtree::new(&self.new, self.bases.1),
        )
    }
}

/// The scans `args` names, opened, and the query of them.
fn compare(
    args: &DiffArgs,
    index_dir: Option<&Path>,
    output: &Output,
) -> Result<Compared, Failure> {
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
    let bases = match &under {
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
    let q = DiffQuery {
        min_delta: args.min_delta.unwrap_or(DEFAULT_MIN_DELTA),
        depth: args.depth,
        size,
        limit,
    };
    Ok(Compared {
        old,
        new,
        bases,
        under,
        dir,
        q,
    })
}

/// `diff`'s rows, ranked, and built one at a time from the two scans.
trait Changes {
    /// How many rows there are.
    fn len(&self) -> usize;
    /// The row at `at`, counted from 0.
    fn row(&self, old: &dyn TreeView, new: &dyn TreeView, at: usize) -> DiffEntry;
    /// The page with no row built: its count and truncation.
    fn head(&self) -> Page<DiffEntry>;
}

impl Changes for DiffRows {
    fn len(&self) -> usize {
        DiffRows::len(self)
    }

    fn row(&self, old: &dyn TreeView, new: &dyn TreeView, at: usize) -> DiffEntry {
        DiffRows::row(self, old, new, at)
    }

    fn head(&self) -> Page<DiffEntry> {
        Page {
            entries: Vec::new(),
            total_count: self.total_count,
            next: None,
            truncated: self.truncated,
        }
    }
}

/// Writes the `rows` of `compared` with their `totals`. With `stream` (a
/// format that [`Output::streams`]), each row is built as it is written
/// and none is held: both scans are checked before the first row, and
/// after each row is built ([`Index::failed`]); the rows end before the
/// first built from a failed read, and the failure is returned (`ndjson`
/// has no trailer). Otherwise every row is built first, as the
/// envelope's entries (which the hints read), and the scans checked
/// before they are written.
fn write<C: Changes>(
    compared: &Compared,
    rows: &C,
    mut totals: DiffTotals,
    output: &Output,
    stream: bool,
) -> Result<(), Failure> {
    let Compared {
        old,
        new,
        bases,
        under,
        dir,
        q,
    } = compared;
    let (old_view, new_view) = compared.views();
    totals.old_scan = old.meta().scan_id.to_string();
    totals.new_scan = new.meta().scan_id.to_string();
    let path = new_view.root_path().to_string_lossy().into_owned();
    let mut page = rows.head();
    if !stream {
        page.entries = (0..rows.len())
            .map(|at| rows.row(&old_view, &new_view, at))
            .collect();
    }
    let mut filters = vec![format!("--min-delta={}", q.min_delta)];
    if let Some(path) = under {
        filters.push(format!("--under={}", path.display()));
    }
    let returned_count = if stream {
        rows.len()
    } else {
        page.entries.len()
    } as u64;
    let block = read_block(new, &[bases.1], q.size, Some(LISTED_EXCLUDED));
    let warnings = source::excluded_truncated(&block, Some(&path))
        .into_iter()
        .collect();
    let ids = (
        query::short_id(dir, &totals.old_scan),
        query::short_id(dir, &totals.new_scan),
    );
    let footer = footer(&totals, &ids, output);
    let mut envelope = Envelope {
        ok: true,
        schema_version: SCHEMA_VERSION,
        tool: Tool::CURRENT,
        scan: block,
        query: QueryBlock {
            command: "diff",
            path,
            depth: q.depth,
            limit: q.limit.map(|l| l as u64),
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
    envelope.next = next(&envelope, new.file());
    check(old)?;
    check(new)?;
    if !stream {
        return output.list_diff(&envelope, &totals, &footer);
    }
    let built = (0..rows.len())
        .map(|at| rows.row(&old_view, &new_view, at))
        .take_while(|_| !old.failed() && !new.failed());
    output.list_rows(
        &envelope,
        built,
        || {
            check(old)?;
            check(new)
        },
        &footer,
    )
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

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use clap::Parser;
    use duscope_core::envelope::DiffEntry;
    use duscope_core::query::{self, Page, TreeView};

    use super::{Changes, compare, write, written};
    use crate::cli::{Cli, Verb};
    use crate::out::{Capture, Fields, Format, Output, Units};
    use crate::verbs::Failure;

    /// `tree` scanned, changed (files removed, added, grown and shrunk, a
    /// directory removed, a file become a directory) and scanned again,
    /// each scan saved in `env`'s index directory: the root and the two
    /// scans' ids.
    fn saved_pair(env: &duscope_testkit::TestEnv) -> (PathBuf, String, String) {
        let root = env.work_dir().canonicalize().unwrap().join("tree");
        let dirs = ["a", "a/deep", "b", "gone", "gone/inner"];
        for dir in dirs {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        let file = |i: usize| root.join(dirs[i % 3]).join(format!("{i:02}.bin"));
        for i in 0..40usize {
            std::fs::write(file(i), vec![b'x'; (i * 997) % 20_000 + 1]).unwrap();
        }
        std::fs::write(root.join("gone/inner/x"), vec![b'x'; 9000]).unwrap();
        std::fs::write(root.join("b/becomes"), b"a file").unwrap();
        let save = || {
            let mut argv: Vec<std::ffi::OsString> = ["duscope", "scan", "--save", "--json"]
                .into_iter()
                .map(Into::into)
                .collect();
            argv.extend([root.clone().into(), "--index-dir".into()]);
            argv.extend([env.index_dir().into(), "--config".into(), "none".into()]);
            let (code, written) = crate::run_captured(argv);
            assert_eq!(code, 0, "{}", String::from_utf8_lossy(&written));
            let envelope: serde_json::Value = serde_json::from_slice(&written).unwrap();
            envelope["scan"]["id"].as_str().unwrap().to_owned()
        };
        let old = save();
        for i in 0..8 {
            std::fs::remove_file(file(i)).unwrap();
        }
        for i in 10..30usize {
            let size = if i % 2 == 0 { (i + 2) * 8192 } else { 1 };
            std::fs::write(file(i), vec![b'x'; size]).unwrap();
        }
        for i in 40..52usize {
            std::fs::write(file(i), vec![b'x'; i * 1000]).unwrap();
        }
        std::fs::remove_dir_all(root.join("gone")).unwrap();
        std::fs::remove_file(root.join("b/becomes")).unwrap();
        std::fs::create_dir(root.join("b/becomes")).unwrap();
        let new = save();
        (root, old, new)
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

    fn fields() -> Fields {
        Fields::Named(
            ["name", "change", "delta_bytes"]
                .map(str::to_owned)
                .to_vec(),
        )
    }

    /// `diff` written collected and streamed to the same output gives the
    /// same bytes: `ndjson` (every row, the trailer's counts), with
    /// `--fields` and `--detail`, and `--plain` with `--all`, with
    /// `--fields`, `--header` and `-0`, and with a limit.
    #[test]
    fn p2_qry_14_diff_streamed_rows_are_the_collected_rows_byte_for_byte() {
        let env = duscope_testkit::TestEnv::new();
        let (_, old, new) = saved_pair(&env);
        let dir = env.index_dir();
        let fields = fields();
        let shapes: [(Format, Option<&Fields>, bool, &[&str]); 6] = [
            (Format::Ndjson, None, false, &[]),
            (Format::Ndjson, Some(&fields), false, &[]),
            (Format::Ndjson, Some(&Fields::Detail), false, &[]),
            (Format::Plain, None, false, &["--all"]),
            (Format::Plain, Some(&fields), true, &["--all"]),
            (Format::Plain, None, false, &["--limit", "3"]),
        ];
        for (format, chosen, header_null, extra) in shapes {
            let mut line = vec!["duscope", "diff", &old, &new, "--min-delta", "1"];
            line.extend_from_slice(extra);
            let cli = Cli::try_parse_from(&line).unwrap();
            let Some(Verb::Diff(args)) = &cli.verb else {
                unreachable!()
            };
            let written = |stream: bool| {
                let (output, capture) = captured(format, chosen, header_null);
                written(args, Some(&dir), &output, stream).unwrap();
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

    /// `rows`, with a read of the old scan (`old`) or of the new one
    /// failing before the row at `at` (counted from 1) is built: the read
    /// of the id past that scan's last entry, which the index fails as it
    /// fails a read of a file overwritten under it; from then on each of
    /// that scan's entries reads as empty.
    struct FailAt<'r, C> {
        rows: &'r C,
        at: usize,
        old: bool,
    }

    impl<C: Changes> Changes for FailAt<'_, C> {
        fn len(&self) -> usize {
            self.rows.len()
        }

        fn row(&self, old: &dyn TreeView, new: &dyn TreeView, at: usize) -> DiffEntry {
            if at + 1 == self.at {
                let failing = if self.old { old } else { new };
                failing.node(failing.len());
            }
            self.rows.row(old, new, at)
        }

        fn head(&self) -> Page<DiffEntry> {
            self.rows.head()
        }
    }

    /// `diff` as `cli` asks, on the saved scans in `dir`, written to
    /// `output` as the verb writes it, streamed when `stream`; with
    /// `fail`, a read of the old scan (`true`) or the new one fails at
    /// that row ([`FailAt`]).
    fn diffed(
        cli: &Cli,
        dir: &Path,
        output: &Output,
        stream: bool,
        fail: Option<(usize, bool)>,
    ) -> Result<(), Failure> {
        let Some(Verb::Diff(args)) = &cli.verb else {
            unreachable!()
        };
        let compared = compare(args, Some(dir), output)?;
        let (old, new) = compared.views();
        let (rows, totals) = query::diff_rows(&old, &new, &compared.q);
        drop((old, new));
        match fail {
            Some((at, old)) => {
                let failing = FailAt {
                    rows: &rows,
                    at,
                    old,
                };
                write(&compared, &failing, totals, output, stream)
            }
            None => write(&compared, &rows, totals, output, stream),
        }
    }

    /// A read of either scan that fails at row n of `diff` ends the
    /// streamed rows there: stdout holds the header and the n - 1 rows
    /// before it, each the row the collected list writes, and none built
    /// from the failed read or after it; `ndjson` has no `end` line, and
    /// the error envelope main writes next is its last line; the failure
    /// is `index_corrupt`. A format that collects the rows (JSON) writes
    /// none of them.
    #[test]
    fn p2_qry_14_a_read_that_fails_mid_stream_ends_the_diff_rows_there() {
        let env = duscope_testkit::TestEnv::new();
        let (_, old, new) = saved_pair(&env);
        let dir = env.index_dir();
        let line = ["duscope", "diff", &old, &new, "--min-delta", "1", "--all"];
        let cli = Cli::try_parse_from(line).unwrap();
        let fields = fields();
        let shapes: [(Format, Option<&Fields>, bool); 4] = [
            (Format::Ndjson, None, false),
            (Format::Ndjson, Some(&fields), false),
            (Format::Plain, None, false),
            (Format::Plain, Some(&fields), true),
        ];
        for (format, chosen, header_null) in shapes {
            let (output, capture) = captured(format, chosen, header_null);
            diffed(&cli, &dir, &output, false, None).unwrap();
            let whole = capture.take();
            let terminator = if header_null { 0 } else { b'\n' };
            let records: Vec<&[u8]> = whole.split_inclusive(|&b| b == terminator).collect();
            // The lines before the rows (ndjson's header, --header's
            // names) and after them (ndjson's trailer).
            let ndjson = format == Format::Ndjson;
            let before = usize::from(ndjson || header_null);
            let rows = records.len() - before - usize::from(ndjson);
            assert!(rows >= 30, "{format:?}: {rows} rows");
            for (at, old) in [1, 2, rows / 2, rows]
                .into_iter()
                .flat_map(|at| [(at, true), (at, false)])
            {
                let side = if old { "old" } else { "new" };
                let shape = format!("{format:?} {chosen:?}, the {side} scan at row {at} of {rows}");
                let (output, capture) = captured(format, chosen, header_null);
                let failure = diffed(&cli, &dir, &output, true, Some((at, old))).unwrap_err();
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
                }
            }
        }
        // JSON holds its rows for the envelope: a failed read writes none,
        // whether the verb would stream them or not.
        for stream in [false, true] {
            let (output, capture) = captured(Format::Json, None, false);
            let failure = diffed(&cli, &dir, &output, stream, Some((2, false))).unwrap_err();
            assert_eq!(failure.code, "index_corrupt", "{stream}");
            assert!(capture.take().is_empty(), "{stream}");
        }
    }
}
