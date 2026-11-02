//! `explain`: facts about one path, from the newest saved scan that holds
//! it (or `--scan`), else from a fresh `stat`; and with `--errors`, what a
//! saved scan could not read.

use std::path::{Path, PathBuf};

use duscope_core::envelope::{
    Envelope, Explain, ObjectEnvelope, ObjectQuery, QueryBlock, SCHEMA_VERSION, ScanBlock,
    ScanErrorEntry, Tool,
};
use duscope_core::index::Index;
use duscope_core::query::{
    ExplainQuery, Resolved, ScanQuery, ScanSelector, SizeKind, abs_path, clock, explain,
    explain_unindexed, resolve_scan, timegrammar,
};
use duscope_core::resolve::resolve_path;

use super::next::Hints;
use super::source::{core_failure, saved_block, stale_index};
use super::{Exit, Failure, scan};
use crate::cli::ExplainArgs;
use crate::out::{Output, thousands};

/// `--max-age` when neither the flag nor the configuration sets it.
const DEFAULT_MAX_AGE: &str = "7d";

/// What both forms of `explain` resolve a saved scan with.
struct Lookup {
    max_age: String,
    max_age_ns: i64,
    now: i64,
    cwd: PathBuf,
    /// The index directory, when one can be found.
    dir: Option<PathBuf>,
}

impl Lookup {
    fn new(args: &ExplainArgs, index_dir: Option<&Path>) -> Result<Lookup, Failure> {
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
        let cwd = std::env::current_dir().map_err(|e| {
            Failure::new(
                Exit::Runtime,
                "io_error",
                format!("cannot read the working directory: {e}"),
            )
        })?;
        let dir = index_dir
            .map(Path::to_owned)
            .or_else(|| duscope_core::index::default_dir(|name| std::env::var_os(name)));
        Ok(Lookup {
            max_age,
            max_age_ns,
            now,
            cwd,
            dir,
        })
    }

    /// The saved scan `selector` names for `path`: `None` when latest finds
    /// none. Without an index directory latest has nothing to find and a
    /// prefix nowhere to look; an index file needs none.
    fn resolve(
        &self,
        selector: &ScanSelector,
        path: Option<&Path>,
    ) -> Result<Option<Resolved>, Failure> {
        let dir = match (&self.dir, selector) {
            (Some(dir), _) => dir.as_path(),
            (None, ScanSelector::File(_)) => self.cwd.as_path(),
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
        };
        resolve_scan(selector, &q, |_| true).map_err(core_failure)
    }
}

/// `explain`.
pub fn run(args: &ExplainArgs, index_dir: Option<&Path>, output: &Output) -> Result<(), Failure> {
    let lookup = Lookup::new(args, index_dir)?;
    if let Some(selector) = &args.errors {
        if output.fields_first::<ScanErrorEntry>("explain --errors")? {
            return Ok(());
        }
        let selector = ScanSelector::parse(selector).map_err(core_failure)?;
        let Some(resolved) = lookup.resolve(&selector, None)? else {
            return Err(Failure::new(
                Exit::NotFound,
                "scan_not_found",
                format!("no saved scan holds {}", lookup.cwd.display()),
            )
            .hint("Run duscope scans to list the saved scans"));
        };
        return errors(&resolved.index, output);
    }
    let path = unresolved_last(&lookup.cwd, args.path.as_deref().unwrap_or(Path::new(".")));
    let selector = match &args.scan {
        Some(text) => ScanSelector::parse(text).map_err(core_failure)?,
        None => ScanSelector::Latest,
    };
    let caches = scan::cache_rules(args.caches_extra.as_deref())?;
    let env = |name: &str| std::env::var_os(name);
    let q = ExplainQuery {
        caches: &caches,
        env: &env,
    };
    let (scan, file, result, warnings) = match lookup.resolve(&selector, Some(&path))? {
        Some(r) => {
            let facts = explain(&r.index, r.base, &q).map_err(core_failure)?;
            let warnings = r
                .stale
                .then(|| stale_index(&r.index, &lookup.max_age))
                .into_iter()
                .collect();
            let size = if r.index.meta().size_kind == "apparent" {
                SizeKind::Apparent
            } else {
                SizeKind::Allocated
            };
            let file = r.index.file().to_owned();
            (
                Some(saved_block(&r.index, size)),
                Some(file),
                facts,
                warnings,
            )
        }
        None => (
            None,
            None,
            explain_unindexed(&path, &q).map_err(core_failure)?,
            Vec::new(),
        ),
    };
    let path = path.to_string_lossy().into_owned();
    let next = next(scan.as_ref(), file.as_deref(), &path, result.kind);
    let envelope = ObjectEnvelope {
        ok: true,
        schema_version: SCHEMA_VERSION,
        tool: Tool::CURRENT,
        scan,
        query: ObjectQuery {
            command: "explain",
            path,
        },
        result,
        warnings,
        next,
    };
    let lines = people(&envelope.result, output);
    output.object(&envelope, &plain(&envelope.result), &lines)
}

/// `explain`'s hints: what the scan could not read, when it could not
/// read something, and a directory's largest entries and caches.
fn next(scan: Option<&ScanBlock>, file: Option<&Path>, path: &str, kind: &str) -> Vec<String> {
    let Some(mut hints) = Hints::new(scan.and_then(|s| s.id.as_deref()), file) else {
        return Vec::new();
    };
    if scan.is_some_and(|s| s.error_count > 0) {
        hints.errors();
    }
    if kind == "dir" {
        hints
            .run("top", Some(path), &[])
            .run("caches", Some(path), &[]);
    }
    hints.done()
}

/// `given`, absolute, with every symbolic link above its last component
/// resolved and the last one kept: `explain` speaks of a link, not of what
/// it leads to.
fn unresolved_last(cwd: &Path, given: &Path) -> PathBuf {
    let absolute: PathBuf = cwd.join(given).components().collect();
    match (absolute.parent(), absolute.file_name()) {
        (Some(parent), Some(name)) => resolve_path(cwd, parent).real.join(name),
        _ => resolve_path(cwd, given).real,
    }
}

/// The facts for people: one line each, name and value.
fn people(e: &Explain, output: &Output) -> Vec<(String, String)> {
    let size = |n: u64| output.units.size(n);
    let mut lines = vec![
        ("path".to_owned(), e.path.clone()),
        ("kind".to_owned(), e.kind.to_owned()),
        (
            "size".to_owned(),
            format!(
                "{} allocated · {} apparent",
                size(e.allocated_bytes),
                size(e.apparent_bytes)
            ),
        ),
    ];
    if let (Some(files), Some(dirs)) = (e.file_count, e.dir_count) {
        lines.push((
            "contents".to_owned(),
            format!("{} files · {} dirs", thousands(files), thousands(dirs)),
        ));
    }
    lines.push(("modified".to_owned(), e.mtime.clone()));
    if let Some(nlink) = e.nlink {
        lines.push(("links".to_owned(), thousands(nlink)));
    }
    if let Some(outside) = e.links_outside_subtree {
        lines.push(("links outside".to_owned(), thousands(outside)));
    }
    if e.is_clone == Some(true) || e.clone_members_outside.is_some_and(|n| n > 0) {
        lines.push((
            "clones".to_owned(),
            format!(
                "group of {} · {} outside",
                e.clone_group_size.map_or("-".to_owned(), thousands),
                e.clone_members_outside.map_or("-".to_owned(), thousands)
            ),
        ));
    }
    if let (Some(bytes), Some(kind)) = (e.freeable_now_bytes, e.freeable_kind) {
        lines.push((
            "freeable now".to_owned(),
            format!("{} ({})", size(bytes), kind.replace('_', " ")),
        ));
    }
    if let Some(by) = e.regenerable_by {
        lines.push(("regenerable by".to_owned(), by.to_owned()));
    }
    if e.cachedir_tag {
        lines.push(("CACHEDIR.TAG".to_owned(), "valid".to_owned()));
    }
    if let Some(git) = &e.git {
        let mut state = vec![if git.ignored {
            "ignored"
        } else {
            "not ignored"
        }];
        if git.precious {
            state.push("precious");
        }
        state.push(if git.tracked_inside {
            "tracked files inside"
        } else {
            "nothing tracked inside"
        });
        lines.push((
            "git".to_owned(),
            format!("{} · in {}", state.join(" · "), git.repo_root),
        ));
    }
    if !e.flags.is_empty() {
        lines.push(("flags".to_owned(), e.flags.join(" ")));
    }
    lines.push(("confidence".to_owned(), e.confidence.to_owned()));
    for line in &e.evidence {
        lines.push(("evidence".to_owned(), line.clone()));
    }
    lines
}

/// The facts for `--plain`: name and value, raw bytes for the path.
fn plain(e: &Explain) -> Vec<(&'static str, Vec<u8>)> {
    let opt = |v: Option<u64>| v.map_or_else(Vec::new, |n| n.to_string().into_bytes());
    vec![
        ("path", e.path_bytes.clone()),
        ("kind", e.kind.as_bytes().to_vec()),
        (
            "allocated_bytes",
            e.allocated_bytes.to_string().into_bytes(),
        ),
        ("apparent_bytes", e.apparent_bytes.to_string().into_bytes()),
        ("nlink", opt(e.nlink)),
        ("links_outside_subtree", opt(e.links_outside_subtree)),
        ("freeable_now_bytes", opt(e.freeable_now_bytes)),
        (
            "freeable_kind",
            e.freeable_kind.unwrap_or_default().as_bytes().to_vec(),
        ),
        (
            "regenerable_by",
            e.regenerable_by.unwrap_or_default().as_bytes().to_vec(),
        ),
        ("confidence", e.confidence.as_bytes().to_vec()),
    ]
}

/// `--errors`: the directories a saved scan listed but could not read, and
/// the entries it listed but could not examine, path bytewise.
fn errors(index: &Index, output: &Output) -> Result<(), Failure> {
    let rows = index.scan_errors().map_err(core_failure)?;
    let mut entries: Vec<ScanErrorEntry> = rows
        .into_iter()
        .map(|(id, name, errno)| {
            let mut path = abs_path(index, id);
            let kind = match name {
                Some(name) => {
                    if path.last() != Some(&b'/') {
                        path.push(b'/');
                    }
                    path.extend_from_slice(&name);
                    "entry"
                }
                None => "directory",
            };
            ScanErrorEntry {
                path: String::from_utf8_lossy(&path).into_owned(),
                path_bytes: path,
                kind,
                errno,
                message: std::io::Error::from_raw_os_error(errno).to_string(),
            }
        })
        .collect();
    entries.sort_by(|a, b| a.path_bytes.cmp(&b.path_bytes));
    let block = saved_block(index, SizeKind::Allocated);
    let count = entries.len() as u64;
    let footer = format!(
        "{} unreadable · saved scan {}",
        thousands(count),
        index.meta().scan_id
    );
    let envelope = Envelope {
        ok: true,
        schema_version: SCHEMA_VERSION,
        tool: Tool::CURRENT,
        query: QueryBlock {
            command: "explain --errors",
            path: block.root.clone(),
            depth: None,
            limit: None,
            sort: "path".to_owned(),
            filters: Vec::new(),
        },
        scan: block,
        entries,
        returned_count: count,
        total_count: count,
        truncated: false,
        next_cursor: None,
        warnings: Vec::new(),
        next: Vec::new(),
    };
    output.list(&envelope, &footer)
}
