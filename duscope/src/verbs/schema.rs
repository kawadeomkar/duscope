//! `schema`: the index's tables and columns, for `query --sql`, with each
//! table's rows in the newest saved scan that holds the path (or the one
//! `--scan` names); and with `--envelope <verb>`, the JSON Schema of that
//! verb's envelope, as `schemas/envelope/` holds it. `schema` never scans:
//! without a saved scan the rows are null.

use std::borrow::Cow;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use duscope_core::envelope::{ObjectEnvelope, ObjectQuery, SCHEMA_VERSION, SchemaInfo, Tool};
use duscope_core::index::{INDEX_VERSION, Index, SCHEMA, layout};
use duscope_core::query::{ScanQuery, ScanSelector, SizeKind, clock, resolve_scan};
use duscope_core::resolve::resolve_path;

use super::source::{core_failure, saved_block};
use super::{Exit, Failure};
use crate::cli::SchemaArgs;
use crate::out::{Output, thousands};

/// Each envelope's JSON Schema, by the name `--envelope` takes: compact
/// copies of `schemas/envelope/<name>.schema.json`, which the published
/// crate does not carry (a test holds them equal).
const ENVELOPES: [(&str, &str); 16] = [
    ("scan", include_str!("schema/scan.schema.json")),
    ("top", include_str!("schema/top.schema.json")),
    ("tree", include_str!("schema/tree.schema.json")),
    ("by-type", include_str!("schema/by-type.schema.json")),
    ("stale", include_str!("schema/stale.schema.json")),
    ("caches", include_str!("schema/caches.schema.json")),
    ("summary", include_str!("schema/summary.schema.json")),
    ("diff", include_str!("schema/diff.schema.json")),
    ("explain", include_str!("schema/explain.schema.json")),
    ("query", include_str!("schema/query.schema.json")),
    ("scans", include_str!("schema/scans.schema.json")),
    ("rescan", include_str!("schema/rescan.schema.json")),
    ("import", include_str!("schema/import.schema.json")),
    ("schema", include_str!("schema/schema.schema.json")),
    ("error", include_str!("schema/error.schema.json")),
    ("spilled", include_str!("schema/spilled.schema.json")),
];

/// `schema`.
pub fn run(args: &SchemaArgs, index_dir: Option<&Path>, output: &Output) -> Result<(), Failure> {
    if let Some(name) = args.envelope.as_deref().filter(|name| !name.is_empty()) {
        return envelope(name, output);
    }
    let cwd = std::env::current_dir().map_err(|e| {
        Failure::new(
            Exit::Runtime,
            "io_error",
            format!("cannot read the working directory: {e}"),
        )
    })?;
    let path = args
        .path
        .as_deref()
        .map_or_else(|| cwd.clone(), |p| resolve_path(&cwd, p).real);
    if path.symlink_metadata().is_err() {
        return Err(Failure::new(
            Exit::NotFound,
            "scan_not_found",
            format!("{} does not exist", path.display()),
        )
        .hint("Check the path, or run duscope schema without one"));
    }
    // The list of envelopes reads no scan.
    let index = if args.envelope.is_some() {
        None
    } else {
        saved(args, index_dir, &cwd, &path)?
    };
    let tables = layout::tables(index.as_ref()).map_err(core_failure)?;
    let scan = index.as_ref().map(|i| saved_block(i, SizeKind::Allocated));
    if let Some(index) = &index
        && let Some(message) = index.failure()
    {
        return Err(core_failure(duscope_core::CoreError::IndexCorrupt {
            file: index.file().display().to_string(),
            message,
        }));
    }
    let result = SchemaInfo {
        index_version: INDEX_VERSION,
        tables,
        envelopes: ENVELOPES.iter().map(|&(name, _)| name).collect(),
    };
    let envelope = ObjectEnvelope {
        ok: true,
        schema_version: SCHEMA_VERSION,
        tool: Tool::CURRENT,
        scan,
        query: ObjectQuery {
            command: if args.envelope.is_some() {
                "schema --envelope"
            } else {
                "schema"
            },
            path: path.to_string_lossy().into_owned(),
        },
        result,
        warnings: Vec::new(),
        next: Vec::new(),
    };
    if output.is_json() {
        return output.object(&envelope, &[], &[]);
    }
    if args.envelope.is_some() {
        let names = envelope
            .result
            .envelopes
            .iter()
            .map(|name| vec![Cow::Borrowed(name.as_bytes())]);
        return output.records(&["envelope"], names);
    }
    if output.is_machine() {
        return output.records(&PLAIN, plain(&envelope.result));
    }
    output.bytes(people(&envelope).as_bytes())
}

/// `--envelope <name>`: the schema, compact for machines, laid out as the
/// file is for people.
fn envelope(name: &str, output: &Output) -> Result<(), Failure> {
    let Some(&(_, text)) = ENVELOPES.iter().find(|(n, _)| *n == name) else {
        let names: Vec<&str> = ENVELOPES.iter().map(|&(n, _)| n).collect();
        return Err(Failure::new(
            Exit::Usage,
            "usage",
            format!(
                "--envelope {name}: no envelope by that name; the envelopes are {}",
                names.join(", ")
            ),
        )
        .hint("Run duscope schema --envelope to list the envelopes"));
    };
    if output.is_machine() {
        return output.bytes(text.as_bytes());
    }
    output.bytes(pretty(text)?.as_bytes())
}

/// `text`, a compact schema, laid out as `schemas/envelope/` has it.
fn pretty(text: &str) -> Result<String, Failure> {
    serde_json::from_str::<serde_json::Value>(text)
        .and_then(|value| serde_json::to_string_pretty(&value))
        .map(|pretty| pretty + "\n")
        .map_err(|e| {
            Failure::new(
                Exit::Runtime,
                "io_error",
                format!("a schema is not JSON: {e}"),
            )
        })
}

/// The saved scan whose rows `schema` counts: the one `--scan` names, or
/// the newest that holds the path; `None` when none does, or there is no
/// index directory to look in.
fn saved(
    args: &SchemaArgs,
    index_dir: Option<&Path>,
    cwd: &Path,
    path: &Path,
) -> Result<Option<Index>, Failure> {
    let selector = match &args.scan {
        Some(text) => ScanSelector::parse(text).map_err(core_failure)?,
        None => ScanSelector::Latest,
    };
    let dir: Option<PathBuf> = index_dir
        .map(Path::to_owned)
        .or_else(|| duscope_core::index::default_dir(|name| std::env::var_os(name)));
    let dir = match (dir, &selector) {
        (Some(dir), _) => dir,
        (None, ScanSelector::File(_)) => cwd.to_owned(),
        (None, ScanSelector::Prefix(_)) => {
            return Err(Failure::new(
                Exit::NotFound,
                "scan_not_found",
                "no index directory to look for the scan in: neither HOME nor XDG_CACHE_HOME is set",
            )
            .hint("Run duscope schema with --index-dir naming the directory the scans are in"));
        }
        (None, ScanSelector::Latest) => return Ok(None),
    };
    let q = ScanQuery {
        index_dir: &dir,
        path: matches!(selector, ScanSelector::Latest).then_some(path),
        cwd,
        // The layout does not age: no saved scan is stale to it.
        max_age_ns: i64::MAX,
        now: clock::now(),
    };
    let resolved = resolve_scan(&selector, &q, |_| true).map_err(core_failure)?;
    Ok(resolved.map(|r| r.index))
}

/// `--plain`'s fields: a record per column.
const PLAIN: [&str; 6] = [
    "table",
    "column",
    "type",
    "nullable",
    "platforms",
    "row_count",
];

/// `--plain`'s records: a column of a table each, with the table's rows.
fn plain(schema: &SchemaInfo) -> impl Iterator<Item = Vec<Cow<'_, [u8]>>> {
    schema.tables.iter().flat_map(|table| {
        let rows = table.row_count.map(|n| n.to_string()).unwrap_or_default();
        table.columns.iter().map(move |column| {
            vec![
                Cow::Borrowed(table.name.as_bytes()),
                Cow::Borrowed(column.name.as_bytes()),
                Cow::Borrowed(column.r#type.as_bytes()),
                Cow::Owned(column.nullable.to_string().into_bytes()),
                Cow::Owned(column.platforms.join(",").into_bytes()),
                Cow::Owned(rows.clone().into_bytes()),
            ]
        })
    })
}

/// For people: the DDL, each table followed by its rows in the saved scan.
fn people(envelope: &ObjectEnvelope<SchemaInfo>) -> String {
    let rows = |name: &str| {
        envelope
            .result
            .tables
            .iter()
            .find(|t| t.name == name)
            .and_then(|t| t.row_count)
    };
    let scan = envelope.scan.as_ref().and_then(|s| s.id.as_deref());
    let mut out = String::new();
    let mut table = None;
    for line in SCHEMA.lines() {
        out.push_str(line);
        out.push('\n');
        if let Some(rest) = line.strip_prefix("CREATE TABLE ") {
            table = rest.split_whitespace().next();
        }
        if line.starts_with(')')
            && let (Some(name), Some(id)) = (table.take(), scan)
        {
            let n = rows(name).unwrap_or(0);
            let _ = writeln!(out, "-- {} rows in scan {id}", thousands(n));
        }
    }
    if scan.is_none() {
        out.push_str(
            "-- no saved scan holds the path: run duscope scan --save to count the rows\n",
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::ENVELOPES;

    #[test]
    fn p2_qry_08_the_envelopes_are_the_schemas_files() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../schemas/envelope");
        let mut files: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".schema.json"))
            .collect();
        files.sort();
        let mut names: Vec<String> = ENVELOPES
            .iter()
            .map(|(name, _)| format!("{name}.schema.json"))
            .collect();
        names.sort();
        assert_eq!(names, files, "every schemas/envelope file, and no other");
        for (name, text) in ENVELOPES {
            let file = fs::read_to_string(dir.join(format!("{name}.schema.json"))).unwrap();
            // The copy, laid out, is the file byte for byte.
            assert_eq!(
                super::pretty(text).unwrap(),
                file,
                "duscope/src/verbs/schema/{name}.schema.json differs from schemas/envelope/: \
                 copy it compact"
            );
            assert_eq!(text.lines().count(), 1, "{name}: the copy is compact");
        }
    }
}
