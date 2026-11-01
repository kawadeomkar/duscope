//! `query --sql`: one read-only statement over a saved scan's tables, in
//! the sandbox of `duscope_core::query::sql`. The scan is the newest saved
//! one that holds the path, or one run for the verb; `:root_id` and
//! `:root_last` name the path's entry and the last id below it.

use std::path::Path;
use std::time::Duration;

use duscope_core::envelope::{ObjectEnvelope, SCHEMA_VERSION, SqlQueryBlock, SqlResult, Tool};
use duscope_core::index::Index;
use duscope_core::query::{self, SqlQuery, abs_path, sql};

use super::source::{self, core_failure};
use super::{Failure, scan};
use crate::cli::SqlArgs;
use crate::out::{Output, thousands};

/// `query`.
pub fn run(args: &SqlArgs, index_dir: Option<&Path>, output: &Output) -> Result<(), Failure> {
    let max_rows = args.max_rows.map_or(sql::MAX_ROWS, |n| {
        usize::try_from(n).unwrap_or(sql::MAX_ROWS)
    });
    let timeout = args
        .timeout
        .map_or(sql::DEFAULT_TIMEOUT, Duration::from_secs);
    let source = source::source(&args.walk, &args.index, index_dir)?;
    let size = scan::size_kind(&args.walk);
    let block = source.scan_block(&args.walk, size);
    let warnings = source.warnings(&args.walk);
    // A statement reads an index file: a scan run for the verb is saved
    // first, where it is kept, or to a directory removed afterwards.
    let indexed = source.indexed("query", &args.walk, &[], output)?;
    let (index, root_id) = (&*indexed.index, indexed.base);
    let q = SqlQuery {
        sql: &args.sql,
        max_rows,
        timeout,
        root_id,
    };
    let answer = query::sql(index, &q);
    let path = String::from_utf8_lossy(&abs_path(index, root_id)).into_owned();
    let checked = check(index);
    let result = answer.map_err(core_failure)?;
    checked?;
    let footer = footer(&result, max_rows, &indexed.label);
    let envelope = ObjectEnvelope {
        ok: true,
        schema_version: SCHEMA_VERSION,
        tool: Tool::CURRENT,
        scan: Some(block),
        query: SqlQueryBlock {
            command: "query",
            path,
            sql: args.sql.clone(),
            limit: result.limit,
            max_rows: max_rows as u64,
            timeout_ms: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
        },
        result,
        warnings,
        next: Vec::new(),
    };
    output.sql(&envelope, &footer)
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

/// The one line for people: the rows, whether the limit cut them and what
/// gets more, and the scan.
fn footer(result: &SqlResult, max_rows: usize, scan: &str) -> String {
    let rows = match result.row_count {
        1 => "1 row".to_owned(),
        n => format!("{} rows", thousands(n)),
    };
    if !result.truncated {
        return format!("{rows} · {scan}");
    }
    let more = if result.limited && max_rows < sql::MAX_ROWS {
        "raise --max-rows for more"
    } else if result.limited {
        "narrow the statement: 10,000 rows is the most query returns"
    } else if result.limit < max_rows as u64 {
        "add a LIMIT for more"
    } else {
        "raise --max-rows for more"
    };
    format!(
        "{rows}, the limit of {}: {more} · {scan}",
        thousands(result.limit)
    )
}
