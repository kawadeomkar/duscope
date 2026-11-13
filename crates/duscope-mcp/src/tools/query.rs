//! `duscope_query`: one read-only SQL statement over a saved scan's tables
//! (`query --sql`), in the verb's sandbox unchanged.
//!
//! A statement reads the whole scan, not the subtree at a path, so the tool
//! reads only a scan whose root is a root or lies under one: one whose root
//! lies above a root holds what is outside the roots, and is not found, as
//! one the roots hide. With no `scan_id` (or `latest`) it reads the newest
//! such scan that holds the path, and names it to the verb by its id.

use std::path::{Path, PathBuf};

use duscope_core::index::Index;
use duscope_core::query::{TreeView, index_files};
use serde_json::{Map, Value};

use super::{
    Annotations, CommandLine, Context, Invocation, READ_ONLY, ScanArgument, Tool, path_argument,
};
use crate::roots::{Refusal, Visibility};

/// `query --no-scan --scan ID --sql SQL PATH`.
pub struct Query;

impl Tool for Query {
    fn name(&self) -> &'static str {
        "duscope_query"
    }

    fn description(&self) -> &'static str {
        "One read-only SQL SELECT over a saved scan's tables, which duscope_schema lists, in the \
         command line's sandbox: 25 rows without a LIMIT, 10000 at most, 5 seconds. Reads a scan \
         of a root or of a directory under one."
    }

    fn input_schema(&self) -> &'static str {
        include_str!("schemas/duscope_query.input.json")
    }

    fn verb(&self) -> Option<&'static str> {
        Some("query")
    }

    fn annotations(&self) -> Annotations {
        READ_ONLY
    }

    fn invocation(
        &self,
        arguments: &Map<String, Value>,
        context: &Context<'_>,
    ) -> Result<Invocation, Refusal> {
        let (id, path) = whole_scan(self.name(), arguments, context)?;
        let mut line = CommandLine::new(arguments);
        line.push("--no-scan")
            .push("--scan")
            .push(id)
            .value("sql", "--sql")
            .push(path);
        Ok(line.invocation("query"))
    }

    fn summary(&self, envelope: &Value) -> String {
        let result = &envelope["result"];
        let columns = result["columns"].as_array().map_or(0, Vec::len);
        let rows = format!("{} rows of {columns} columns", result["row_count"]);
        if result["truncated"] == true {
            format!("{rows}; more rows match: add a LIMIT, or narrow the statement")
        } else {
            rows
        }
    }
}

/// The saved scan `tool`, which reads a whole scan, reads, and the path it
/// reads it at: the scan `scan_id` names, whose root must be a root or lie
/// under one; else the newest saved scan of a directory under the roots
/// that holds the path.
pub(super) fn whole_scan(
    tool: &str,
    arguments: &Map<String, Value>,
    context: &Context<'_>,
) -> Result<(String, PathBuf), Refusal> {
    let named = match arguments.get("scan_id").and_then(Value::as_str) {
        Some(selector) => Some(context.roots.scan_selector(selector)?),
        None => None,
    };
    if let Some(prefix) = named.filter(|selector| *selector != "latest") {
        let saved = context.roots.saved_scan(context.index_dir, prefix)?;
        if saved.visibility != Visibility::Inside {
            return Err(Refusal {
                code: "scan_not_found",
                message: format!(
                    "no saved scan {prefix} of a directory under the roots: {tool} reads a \
                     whole scan, and scan {prefix} is of a directory above them"
                ),
                hint: "Call duscope_scan for a scan of a root, or duscope_scans for the saved \
                       scans"
                    .to_owned(),
            });
        }
        let scan = ScanArgument {
            selector: saved.id,
            root: Some(saved.root.clone()),
            default_path: Some(saved.root),
        };
        let path = path_argument(arguments, "path", Some(&scan), context.roots)?;
        return Ok((scan.selector, path));
    }
    let path = path_argument(arguments, "path", None, context.roots)?;
    let id = newest_inside(context, &path).ok_or_else(|| Refusal {
        code: "scan_not_found",
        message: format!(
            "no saved scan of a directory under the roots holds {}",
            path.display()
        ),
        hint: "Call duscope_scan to scan it, or duscope_scans for the saved scans".to_owned(),
    })?;
    Ok((id, path))
}

/// The id of the newest saved scan, an import aside, whose root is a root
/// or lies under one and holds `path`.
fn newest_inside(context: &Context<'_>, path: &Path) -> Option<String> {
    index_files(context.index_dir)
        .into_iter()
        .filter_map(|(id, file)| {
            let index = Index::open(&file).ok()?;
            let root = index.root_path();
            let wanted = index.meta().source == "scan"
                && path.starts_with(root)
                && context.roots.visibility(root) == Visibility::Inside;
            wanted.then(|| (index.meta().started_at, id.to_string()))
        })
        .max()
        .map(|(_, id)| id)
}
