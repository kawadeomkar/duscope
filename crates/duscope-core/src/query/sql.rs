//! `query --sql`: one read-only statement over a saved scan, in a sandbox.
//!
//! Read-only is the connection's to enforce, not the statement's to grant:
//! `PRAGMA query_only` is one a statement could flip. The index is opened
//! read-only on its immutable URI with `SQLITE_DBCONFIG_DEFENSIVE`, an
//! untrusted schema, no triggers, views or attached databases, and
//! `SQLite`'s limits lowered; sorts and temporary tables stay
//! in memory, of which `SQLite` may hold 256 MiB. An authorizer allows a
//! `SELECT`, reads of the index's own tables ([`TABLES`]) and the functions
//! of [`FUNCTIONS`], and denies the rest: writes, `ATTACH`, `PRAGMA`,
//! transactions, other tables, views and triggers. The statement must be
//! one, and read-only (`sqlite3_stmt_readonly`). It runs wrapped as
//! `SELECT * FROM (…) LIMIT n + 1`, n being its rows' limit: 25 for a
//! statement without a `LIMIT` of its own, `--max-rows` (10,000 at most)
//! for one with, so that a row past n says the answer was truncated. A
//! progress handler stops it at the deadline.
//!
//! Beside `SQLite`'s functions, `path(id)` is an entry's absolute path,
//! `human_size(bytes)` a size as tables show it (`412.3 GiB`), and
//! `under(id, root_id)` whether an entry is `root_id` or below it.
//! `:root_id` and `:root_last` name the entry the verb was asked about and
//! the last id below it.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use rusqlite::config::DbConfig;
use rusqlite::functions::{Context, FunctionFlags};
use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use rusqlite::limits::Limit;
use rusqlite::types::{Value, ValueRef};
use rusqlite::{Connection, ErrorCode, OpenFlags};

use std::os::unix::ffi::OsStrExt;

use super::{TreeView, scaled};
use crate::CoreError;
use crate::envelope::{SqlResult, SqlValue};
use crate::index::Index;
use crate::index::reader::uri;

/// The rows a statement without a `LIMIT` of its own returns at most.
pub const DEFAULT_LIMIT: usize = 25;

/// `--max-rows`' default, and its ceiling.
pub const MAX_ROWS: usize = 10_000;

/// `--timeout`'s default.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

/// `--timeout`'s ceiling.
pub const MAX_TIMEOUT: Duration = Duration::from_secs(60);

/// The memory `SQLite` may hold while the statement runs, sorts and
/// temporary tables included.
const HEAP_LIMIT: i64 = 256 << 20;

/// The bytes of values an answer may hold: rows times columns times
/// [`Limit::SQLITE_LIMIT_LENGTH`] could pass any memory there is.
const ANSWER_BYTES: usize = 256 << 20;

/// Virtual-machine steps between looks at the clock.
const PROGRESS_OPS: i32 = 1_000;

/// The tables of the index a statement may read.
pub const TABLES: [&str; 9] = [
    "scan_meta",
    "scan_volumes",
    "dirs",
    "files",
    "hardlinks",
    "clones",
    "shared_members",
    "scan_errors",
    "type_stats",
];

/// The prefixes of `SQLite`'s own tables and of the table-valued functions
/// it registers when a statement first names them (`json_each`), which no
/// module list shows before then.
const SQLITE_NAMESPACES: [&str; 4] = ["sqlite_", "pragma_", "json_", "jsonb_"];

/// The functions a statement may call: `SQLite`'s core scalars and
/// aggregates (and their other names), its date and time functions, its
/// JSON functions, and duscope's.
pub const FUNCTIONS: &[&str] = &[
    "abs",
    "avg",
    "coalesce",
    "count",
    "format",
    "glob",
    "group_concat",
    "hex",
    "if",
    "ifnull",
    "iif",
    "instr",
    "length",
    "like",
    "lower",
    "ltrim",
    "max",
    "min",
    "nullif",
    "printf",
    "quote",
    "replace",
    "round",
    "rtrim",
    "string_agg",
    "substr",
    "substring",
    "sum",
    "total",
    "trim",
    "typeof",
    "unicode",
    "upper",
    // Dates and times.
    "current_date",
    "current_time",
    "current_timestamp",
    "date",
    "datetime",
    "julianday",
    "strftime",
    "time",
    "timediff",
    "unixepoch",
    // JSON, its operators included; json_each and json_tree are tables.
    "->",
    "->>",
    "json",
    "json_array",
    "json_array_length",
    "json_error_position",
    "json_extract",
    "json_group_array",
    "json_group_object",
    "json_insert",
    "json_object",
    "json_patch",
    "json_pretty",
    "json_quote",
    "json_remove",
    "json_replace",
    "json_set",
    "json_type",
    "json_valid",
    "jsonb",
    "jsonb_array",
    "jsonb_extract",
    "jsonb_group_array",
    "jsonb_group_object",
    "jsonb_insert",
    "jsonb_object",
    "jsonb_patch",
    "jsonb_remove",
    "jsonb_replace",
    "jsonb_set",
    // duscope's.
    "human_size",
    "path",
    "under",
];

/// `SQLite`'s limits for the statement: a value, a statement, its columns,
/// its expressions' depth, its compound terms, its program, a function's
/// arguments, attached databases, a LIKE pattern, parameters, triggers,
/// worker threads.
const LIMITS: [(Limit, i32); 12] = [
    (Limit::SQLITE_LIMIT_LENGTH, 1_000_000),
    (Limit::SQLITE_LIMIT_SQL_LENGTH, 100_000),
    (Limit::SQLITE_LIMIT_COLUMN, 100),
    (Limit::SQLITE_LIMIT_EXPR_DEPTH, 100),
    (Limit::SQLITE_LIMIT_COMPOUND_SELECT, 20),
    (Limit::SQLITE_LIMIT_VDBE_OP, 50_000),
    (Limit::SQLITE_LIMIT_FUNCTION_ARG, 32),
    (Limit::SQLITE_LIMIT_ATTACHED, 0),
    (Limit::SQLITE_LIMIT_LIKE_PATTERN_LENGTH, 1_000),
    (Limit::SQLITE_LIMIT_VARIABLE_NUMBER, 10),
    (Limit::SQLITE_LIMIT_TRIGGER_DEPTH, 1),
    (Limit::SQLITE_LIMIT_WORKER_THREADS, 0),
];

/// What `query --sql` was asked.
#[derive(Debug, Clone)]
pub struct SqlQuery<'a> {
    /// The statement.
    pub sql: &'a str,
    /// `--max-rows`: the rows a statement with a `LIMIT` returns at most.
    pub max_rows: usize,
    /// `--timeout`.
    pub timeout: Duration,
    /// The entry `:root_id` names.
    pub root_id: u32,
}

/// The rows `q.sql` returns from `index`, in the sandbox.
pub fn sql(index: &Index, q: &SqlQuery<'_>) -> Result<SqlResult, CoreError> {
    let deadline = Instant::now() + q.timeout;
    let (body, limited) = statement(q.sql);
    let limit = limit(limited, q.max_rows);
    let (db, reader) = sandbox(index)?;
    // Why the authorizer denied what it denied first: SQLite says only
    // "not authorized".
    let refused: Arc<Mutex<Option<String>>> = Arc::default();
    let seen = Arc::clone(&refused);
    let schema = schema(index, &db)?;
    db.authorizer(Some(move |ctx: AuthContext<'_>| {
        authorize(&ctx, &schema, &seen)
    }))
    .map_err(|e| setup(index, &e))?;
    let fail = |e: rusqlite::Error| failure(&e, &refused, q.timeout);
    // The statement alone first: one, and read-only.
    let alone = db.prepare(q.sql).map_err(fail)?;
    if !alone.readonly() {
        return Err(CoreError::SqlDenied(
            "query runs a statement that reads, and this one writes".to_owned(),
        ));
    }
    // A SELECT has a column at least; what reads none is empty.
    if alone.column_count() == 0 {
        return Err(CoreError::SqlInvalid(
            "the statement is empty: give query one SELECT".to_owned(),
        ));
    }
    // The statement's own names: the wrapper's `*` would rename a second
    // `id` to `id:1`.
    let columns: Vec<String> = alone
        .column_names()
        .into_iter()
        .map(str::to_owned)
        .collect();
    drop(alone);
    // A line of its own each side, so that a comment at the statement's
    // end ends before the wrapper's parenthesis.
    let wrapped = format!("SELECT * FROM (\n{body}\n) LIMIT {}", limit + 1);
    let mut stmt = db.prepare(&wrapped).map_err(fail)?;
    let root_last = index.node(q.root_id).last_id;
    for (name, value) in [(":root_id", q.root_id), (":root_last", root_last)] {
        if let Some(at) = stmt.parameter_index(name).map_err(fail)? {
            stmt.raw_bind_parameter(at, value).map_err(fail)?;
        }
    }
    db.progress_handler(PROGRESS_OPS, Some(move || Instant::now() >= deadline))
        .map_err(|e| setup(index, &e))?;
    let mut rows: Vec<Vec<SqlValue>> = Vec::new();
    let mut held = 0;
    let mut truncated = false;
    let mut cursor = stmt.raw_query();
    while let Some(row) = cursor.next().map_err(fail)? {
        if rows.len() == limit {
            truncated = true;
            break;
        }
        let mut values = Vec::with_capacity(columns.len());
        for at in 0..columns.len() {
            let value = value(row.get_ref(at).map_err(fail)?);
            held += match &value {
                SqlValue::Text(text) => text.len(),
                SqlValue::Bytes(bytes) => bytes.len(),
                _ => 8,
            };
            values.push(value);
        }
        if held > ANSWER_BYTES {
            return Err(CoreError::SqlDenied(format!(
                "the answer passed the {} MiB query holds: select fewer rows or columns",
                ANSWER_BYTES >> 20
            )));
        }
        rows.push(values);
    }
    drop(cursor);
    // What `path` and `under` could not read, they answered wrongly.
    let failed = reader
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .failure();
    if let Some(message) = failed {
        return Err(if message.contains("out of memory") {
            memory()
        } else {
            CoreError::IndexCorrupt {
                file: index.file().display().to_string(),
                message,
            }
        });
    }
    Ok(SqlResult {
        columns,
        row_count: rows.len() as u64,
        rows,
        truncated,
        limit: limit as u64,
        limited,
    })
}

/// The rows a statement returns at most under `--max-rows`: `max_rows`
/// when it has a `LIMIT` of its own (`limited`), 25 (or `max_rows`, if
/// lower) when it has not.
fn limit(limited: bool, max_rows: usize) -> usize {
    if limited {
        max_rows
    } else {
        DEFAULT_LIMIT.min(max_rows)
    }
}

/// `sql_denied` for a statement that outgrew the heap limit.
fn memory() -> CoreError {
    CoreError::SqlDenied(format!(
        "the statement needed more than the {} MiB of memory query allows",
        HEAP_LIMIT >> 20
    ))
}

/// The index the functions read through, shared by them.
type Reader = Arc<Mutex<Index>>;

/// The sandbox's connection to `index`'s file, duscope's functions
/// registered, with neither the authorizer nor the deadline yet; and the
/// index the functions read through.
fn sandbox(index: &Index) -> Result<(Connection, Reader), CoreError> {
    let db = Connection::open_with_flags(
        uri(index.file()),
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| setup(index, &e))?;
    for (setting, on) in [
        (DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true),
        (DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA, false),
        (DbConfig::SQLITE_DBCONFIG_ENABLE_TRIGGER, false),
        (DbConfig::SQLITE_DBCONFIG_ENABLE_VIEW, false),
        (DbConfig::SQLITE_DBCONFIG_ENABLE_ATTACH_CREATE, false),
        (DbConfig::SQLITE_DBCONFIG_ENABLE_ATTACH_WRITE, false),
    ] {
        db.set_db_config(setting, on)
            .map_err(|e| setup(index, &e))?;
    }
    for (limit, value) in LIMITS {
        db.set_limit(limit, value).map_err(|e| setup(index, &e))?;
    }
    // Before the authorizer, which denies every PRAGMA. The heap limit is
    // the process's: a pragma lowers it and never raises it.
    db.execute_batch(&format!(
        "PRAGMA temp_store = MEMORY; PRAGMA hard_heap_limit = {HEAP_LIMIT};"
    ))
    .map_err(|e| setup(index, &e))?;
    // The functions read the index through a connection of their own,
    // without the sandbox's limits: what they read is duscope's to decide.
    let reader: Reader = Arc::new(Mutex::new(Index::open(index.file())?));
    let flags = FunctionFlags::SQLITE_UTF8
        | FunctionFlags::SQLITE_DETERMINISTIC
        | FunctionFlags::SQLITE_DIRECTONLY;
    let paths = Arc::clone(&reader);
    db.create_scalar_function("path", 1, flags, move |ctx| {
        let index = paths.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(id_arg(ctx, 0, &index).and_then(|id| path(&index, id)))
    })
    .map_err(|e| setup(index, &e))?;
    let ranges = Arc::clone(&reader);
    db.create_scalar_function("under", 2, flags, move |ctx| {
        let index = ranges.lock().unwrap_or_else(PoisonError::into_inner);
        let (Some(id), Some(root)) = (int_arg(ctx, 0), id_arg(ctx, 1, &index)) else {
            return Ok(None);
        };
        let last = index.node(root).last_id;
        Ok(Some(id >= i64::from(root) && id <= i64::from(last)))
    })
    .map_err(|e| setup(index, &e))?;
    db.create_scalar_function("human_size", 1, flags, |ctx| {
        let bytes = match ctx.get_raw(0) {
            ValueRef::Integer(n) => n,
            // An average or a total: the nearest whole byte.
            ValueRef::Real(x) if x.is_finite() => whole(x),
            _ => return Ok(None),
        };
        let size = human_size(bytes.unsigned_abs());
        Ok(Some(if bytes < 0 { format!("-{size}") } else { size }))
    })
    .map_err(|e| setup(index, &e))?;
    Ok((db, reader))
}

/// `x` rounded to the nearest integer, saturating past `i64`'s range.
#[allow(clippy::cast_possible_truncation)]
fn whole(x: f64) -> i64 {
    x.round() as i64
}

/// What `index`'s file holds, and the virtual-table modules a statement
/// could name as tables (`json_each`), read before the authorizer, which
/// denies reads of `sqlite_schema` and every `PRAGMA`.
#[derive(Debug, Default)]
struct Schema {
    /// Every table, index, view and trigger, and every module.
    names: Vec<String>,
    /// The views and triggers: what reads through them is theirs, not the
    /// statement's.
    actors: Vec<String>,
}

fn schema(index: &Index, db: &Connection) -> Result<Schema, CoreError> {
    let mut stmt = db
        .prepare("SELECT name, type IN ('view', 'trigger') FROM sqlite_schema")
        .map_err(|e| setup(index, &e))?;
    let rows: Vec<(String, bool)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .and_then(Iterator::collect)
        .map_err(|e| setup(index, &e))?;
    let mut schema = Schema::default();
    for (name, actor) in rows {
        if actor {
            schema.actors.push(name.clone());
        }
        schema.names.push(name);
    }
    let mut modules = db
        .prepare("SELECT name FROM pragma_module_list")
        .map_err(|e| setup(index, &e))?;
    let modules: Vec<String> = modules
        .query_map([], |row| row.get(0))
        .and_then(Iterator::collect)
        .map_err(|e| setup(index, &e))?;
    schema.names.extend(modules);
    Ok(schema)
}

/// The authorizer: a `SELECT`, reads of the index's tables (not through a
/// view or trigger, nor of an attached or temporary database), and the
/// allowed functions. `schema` names what the file holds. What it denies
/// first, it says why in `refused`.
fn authorize(
    ctx: &AuthContext<'_>,
    schema: &Schema,
    refused: &Mutex<Option<String>>,
) -> Authorization {
    let why = match ctx.action {
        AuthAction::Select | AuthAction::Recursive => return Authorization::Allow,
        AuthAction::Read {
            table_name,
            column_name,
        } => {
            // A common table expression is the accessor of what it reads,
            // as a view or trigger is.
            let direct = ctx
                .accessor
                .is_none_or(|by| !schema.actors.iter().any(|a| a.eq_ignore_ascii_case(by)));
            let allowed = if column_name.is_empty() {
                // A table the statement reads none of the columns of
                // (`count(*)`), by the name it gives: one of the index's,
                // or a name neither the file nor a module holds anything
                // under (a common table expression's). It reads no values
                // either way.
                let named = |name: &str| name.eq_ignore_ascii_case(table_name);
                direct
                    && ctx
                        .database_name
                        .is_none_or(|db| db.eq_ignore_ascii_case("main"))
                    && (TABLES.iter().any(|t| named(t))
                        || !(SQLITE_NAMESPACES.iter().any(|prefix| {
                            table_name
                                .get(..prefix.len())
                                .is_some_and(|p| p.eq_ignore_ascii_case(prefix))
                        }) || schema.names.iter().any(|s| named(s))))
            } else {
                direct && ctx.database_name == Some("main") && TABLES.contains(&table_name)
            };
            if allowed {
                return Authorization::Allow;
            }
            format!("query reads the index's tables only, and {table_name} is not one of them")
        }
        AuthAction::Function { function_name } => {
            if FUNCTIONS
                .iter()
                .any(|f| f.eq_ignore_ascii_case(function_name))
            {
                return Authorization::Allow;
            }
            format!("{function_name} is not one of the functions query allows")
        }
        AuthAction::Pragma { pragma_name, .. } => {
            format!("PRAGMA {pragma_name} is not allowed: query runs a SELECT and nothing else")
        }
        AuthAction::Attach { .. } | AuthAction::Detach { .. } => {
            "ATTACH is not allowed: query reads the one saved scan".to_owned()
        }
        AuthAction::Insert { .. } | AuthAction::Update { .. } | AuthAction::Delete { .. } => {
            "query runs a statement that reads, and this one writes".to_owned()
        }
        _ => "query runs a SELECT and nothing else".to_owned(),
    };
    refused
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get_or_insert(why);
    Authorization::Deny
}

/// The statement before its first `;`, and whether it has a `LIMIT` of its
/// own: one outside parentheses, quotes and comments. `SQLite` has prepared
/// the whole by now, so only a statement it reads as one comes here.
fn statement(sql: &str) -> (&str, bool) {
    let bytes = sql.as_bytes();
    let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'$' || b >= 0x80;
    let mut depth = 0i32;
    let mut limited = false;
    let mut at = 0;
    while at < bytes.len() {
        let rest = &bytes[at..];
        match bytes[at] {
            b'-' if rest.starts_with(b"--") => {
                at += rest.iter().position(|&b| b == b'\n').unwrap_or(rest.len());
            }
            b'/' if rest.starts_with(b"/*") => {
                match rest[2..].windows(2).position(|w| w == b"*/") {
                    Some(end) => at += end + 4,
                    // A comment `SQLite` ends at the end of the input
                    // would swallow the wrapper's parenthesis: the
                    // statement ends where it starts.
                    None => return (&sql[..at], limited),
                }
            }
            quote @ (b'\'' | b'"' | b'`' | b'[') => {
                let close = if quote == b'[' { b']' } else { quote };
                at += 1;
                while at < bytes.len() {
                    if bytes[at] == close {
                        // A doubled quote is one inside the quotes.
                        if close != b']' && bytes.get(at + 1) == Some(&close) {
                            at += 2;
                            continue;
                        }
                        break;
                    }
                    at += 1;
                }
                at += 1;
            }
            b'(' => {
                depth += 1;
                at += 1;
            }
            b')' => {
                depth -= 1;
                at += 1;
            }
            b';' => return (&sql[..at], limited),
            // A parameter's name is not a keyword.
            b':' | b'@' | b'$' | b'?' => {
                at += 1;
                while at < bytes.len() && word(bytes[at]) {
                    at += 1;
                }
            }
            b if word(b) => {
                let start = at;
                while at < bytes.len() && word(bytes[at]) {
                    at += 1;
                }
                if depth == 0 && bytes[start..at].eq_ignore_ascii_case(b"limit") {
                    limited = true;
                }
            }
            _ => at += 1,
        }
    }
    (sql, limited)
}

/// A value of a row, as the envelope writes it.
fn value(value: ValueRef<'_>) -> SqlValue {
    match value {
        ValueRef::Null => SqlValue::Null,
        ValueRef::Integer(n) => SqlValue::Integer(n),
        ValueRef::Real(x) => SqlValue::Real(x),
        ValueRef::Text(text) => match std::str::from_utf8(text) {
            Ok(text) => SqlValue::Text(text.to_owned()),
            Err(_) => SqlValue::Bytes(text.to_vec()),
        },
        ValueRef::Blob(bytes) => SqlValue::Bytes(bytes.to_vec()),
    }
}

/// The integer argument `at`; `None` for a NULL or another type.
fn int_arg(ctx: &Context<'_>, at: usize) -> Option<i64> {
    match ctx.get_raw(at) {
        ValueRef::Integer(n) => Some(n),
        _ => None,
    }
}

/// The argument `at` as an id of `index`; `None` for one it does not have.
fn id_arg(ctx: &Context<'_>, at: usize, index: &Index) -> Option<u32> {
    int_arg(ctx, at)
        .and_then(|n| u32::try_from(n).ok())
        .filter(|&id| id < index.len())
}

/// `path(id)`: text where the path is UTF-8, a BLOB where it is not.
/// `None` where the parents do not reach the root within the scan's
/// entries, as in a damaged index.
fn path(index: &Index, id: u32) -> Option<Value> {
    let mut names: Vec<Vec<u8>> = Vec::new();
    let mut at = id;
    for _ in 0..index.len() {
        if at == 0 {
            let mut path = index.root_path().as_os_str().as_bytes().to_vec();
            for name in names.iter().rev() {
                if path.last() != Some(&b'/') {
                    path.push(b'/');
                }
                path.extend_from_slice(name);
            }
            return Some(match String::from_utf8(path) {
                Ok(text) => Value::Text(text),
                Err(e) => Value::Blob(e.into_bytes()),
            });
        }
        names.push(index.name(at).into_owned());
        at = index.node(at).parent;
    }
    None
}

/// `bytes` in IEC units with one decimal, as tables and footers show them
/// (`412.3 GiB`; `999 B` below a KiB). A value that rounds up to the next
/// unit is shown in it (`1.0 MiB`, not `1024.0 KiB`).
pub fn human_size(bytes: u64) -> String {
    scaled(
        bytes,
        1024,
        &["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"],
    )
}

/// A failure setting the sandbox up on `index`: its file is not one `SQLite`
/// opens as the reader did.
fn setup(index: &Index, e: &rusqlite::Error) -> CoreError {
    CoreError::IndexCorrupt {
        file: index.file().display().to_string(),
        message: e.to_string(),
    }
}

/// The error a statement's failure is: `sql_denied` for what the sandbox
/// refused (the authorizer, a limit, the memory, a function it does not
/// have, a second statement), `sql_timeout` past the deadline, and a usage
/// error for what `SQLite` cannot prepare.
fn failure(e: &rusqlite::Error, refused: &Mutex<Option<String>>, timeout: Duration) -> CoreError {
    let (code, message) = match e {
        rusqlite::Error::SqliteFailure(error, message) => (Some(error.code), message.clone()),
        rusqlite::Error::SqlInputError { error, msg, .. } => (Some(error.code), Some(msg.clone())),
        rusqlite::Error::MultipleStatement => {
            return CoreError::SqlDenied(
                "query runs one statement, and this is more than one".to_owned(),
            );
        }
        _ => (None, None),
    };
    let message = message.unwrap_or_else(|| e.to_string());
    // Whatever failed once the authorizer denied something failed for it,
    // whichever code SQLite gives (a function it denies is SQLITE_ERROR).
    if let Some(why) = refused
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take()
    {
        return CoreError::SqlDenied(why);
    }
    match code {
        Some(ErrorCode::AuthorizationForStatementDenied) => CoreError::SqlDenied(message),
        Some(ErrorCode::OperationInterrupted) => CoreError::SqlTimeout(timeout.as_secs()),
        Some(ErrorCode::OutOfMemory) => memory(),
        Some(ErrorCode::TooBig) => CoreError::SqlDenied(format!(
            "a value passed the {} bytes query allows",
            LIMITS[0].1
        )),
        _ if message.starts_with("no such function") => CoreError::SqlDenied(format!(
            "{message}: query allows SQLite's core, date and JSON functions and duscope's"
        )),
        // The limits' own words: "too many columns", "Expression tree is
        // too large", "too many terms in compound SELECT", the parser's
        // "Recursion limit".
        _ if ["too many", "too large", "recursion limit", "stack overflow"]
            .iter()
            .any(|words| message.to_ascii_lowercase().contains(words)) =>
        {
            CoreError::SqlDenied(message)
        }
        _ => CoreError::SqlInvalid(message),
    }
}

#[cfg(test)]
mod tests;
