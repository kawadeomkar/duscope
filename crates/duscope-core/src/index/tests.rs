use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::Connection;

use super::{INDEX_VERSION, SCAN_META_KEYS, SCHEMA, schema_parts};

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn applied(sql: &[&str]) -> Connection {
    let db = Connection::open_in_memory().unwrap();
    for part in sql {
        db.execute_batch(part).unwrap();
    }
    db
}

fn strings(db: &Connection, sql: &str, param: &str) -> Vec<String> {
    let mut stmt = db.prepare(sql).unwrap();
    stmt.query_map([param], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// Each table's `PRAGMA table_info` (`cid|name|type|notnull|dflt_value|pk`)
/// under its name, marked when it is WITHOUT ROWID, then each index with its
/// columns, all in the order the DDL creates them.
fn layout(db: &Connection) -> String {
    let mut out = String::new();
    let tables = strings(
        db,
        "SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE ?1 ORDER BY rowid",
        "sqlite_%",
    );
    for table in &tables {
        let without_rowid: bool = db
            .query_row(
                "SELECT wr FROM pragma_table_list WHERE schema = 'main' AND name = ?1",
                [table],
                |row| row.get(0),
            )
            .unwrap();
        let mark = if without_rowid { " WITHOUT ROWID" } else { "" };
        writeln!(out, "{table}{mark}").unwrap();
        let mut stmt = db
            .prepare(
                "SELECT cid, name, type, \"notnull\", dflt_value, pk FROM pragma_table_info(?1)",
            )
            .unwrap();
        let rows = stmt
            .query_map([table], |row| {
                Ok(format!(
                    "  {}|{}|{}|{}|{}|{}",
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, Option<String>>(4)?.unwrap_or_default(),
                    row.get::<_, i64>(5)?,
                ))
            })
            .unwrap();
        for row in rows {
            writeln!(out, "{}", row.unwrap()).unwrap();
        }
    }
    let indexes = strings(
        db,
        "SELECT name FROM sqlite_schema WHERE type = 'index' AND sql IS NOT NULL AND name NOT LIKE ?1 ORDER BY rowid",
        "sqlite_%",
    );
    for index in &indexes {
        let table: String = db
            .query_row(
                "SELECT tbl_name FROM sqlite_schema WHERE name = ?1",
                [index],
                |row| row.get(0),
            )
            .unwrap();
        let mut stmt = db
            .prepare(
                "SELECT name, \"desc\" FROM pragma_index_xinfo(?1) WHERE key = 1 ORDER BY seqno",
            )
            .unwrap();
        let columns: Vec<String> = stmt
            .query_map([index], |row| {
                let name: String = row.get(0)?;
                let desc: bool = row.get(1)?;
                Ok(if desc { format!("{name} DESC") } else { name })
            })
            .unwrap()
            .map(Result::unwrap)
            .collect();
        writeln!(out, "index {index} ON {table} ({})", columns.join(", ")).unwrap();
    }
    out
}

#[test]
fn p2_idx_02_the_default_index_dir_is_xdg_then_the_platform_cache() {
    let vars = |pairs: &'static [(&'static str, &'static str)]| {
        move |name: &str| {
            pairs
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| std::ffi::OsString::from(v))
        }
    };
    assert_eq!(
        super::default_dir(vars(&[("XDG_CACHE_HOME", "/xdg"), ("HOME", "/h")])),
        Some(PathBuf::from("/xdg/duscope"))
    );
    let platform = duscope_walk::cache_dir(Path::new("/h")).join("duscope");
    for xdg in ["", "relative"] {
        let pairs: &'static [(&str, &str)] = if xdg.is_empty() {
            &[("XDG_CACHE_HOME", ""), ("HOME", "/h")]
        } else {
            &[("XDG_CACHE_HOME", "relative"), ("HOME", "/h")]
        };
        assert_eq!(
            super::default_dir(vars(pairs)),
            Some(platform.clone()),
            "{xdg:?}"
        );
    }
    assert_eq!(super::default_dir(vars(&[])), None);
}

#[test]
fn p2_idx_01_ddl_matches_the_table_info_fixture() {
    let actual = layout(&applied(&[SCHEMA]));
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/index/table_info.txt");
    if std::env::var_os(duscope_testkit::UPDATE_GOLDEN).is_some_and(|v| v == "1") {
        fs::write(&path, &actual).unwrap();
        return;
    }
    let expected = fs::read_to_string(&path).unwrap();
    assert_eq!(
        actual,
        expected,
        "the DDL's tables differ from {}; rerun with {}=1 to regenerate it, then review the diff",
        path.display(),
        duscope_testkit::UPDATE_GOLDEN
    );
}

#[test]
fn p2_idx_01_index_version_is_present() {
    let db = applied(&[SCHEMA]);
    let rows: Vec<(String, String)> = db
        .prepare("SELECT key, value FROM scan_meta")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        rows,
        [("index_version".to_owned(), INDEX_VERSION.to_owned())]
    );
}

#[test]
fn p2_idx_01_the_crate_carries_the_published_ddl() {
    let published = fs::read_to_string(workspace().join("schemas/index/v2.sql")).unwrap();
    assert!(
        published == SCHEMA,
        "crates/duscope-core/src/index/schema.sql differs from schemas/index/v2.sql; copy the \
         published file over it"
    );
}

#[test]
fn p2_idx_01_the_indexes_follow_the_tables() {
    let (tables, indexes) = schema_parts();
    assert!(
        !tables.contains("CREATE INDEX"),
        "an index comes before the inserts"
    );
    // The proposal's eight, and shared_members (DEV-68).
    assert_eq!(tables.matches("CREATE TABLE").count(), 9);
    let statements: Vec<&str> = indexes.lines().filter(|l| !l.trim().is_empty()).collect();
    // The proposal's four, and the partial `dirs_errors`, `dirs_excluded`
    // and `files_dataless` (DEV-68).
    assert_eq!(statements.len(), 7, "{statements:?}");
    assert!(
        statements.iter().all(|s| s.starts_with("CREATE INDEX ")),
        "{statements:?}"
    );
    assert_eq!(
        layout(&applied(&[tables, indexes])),
        layout(&applied(&[SCHEMA])),
        "the two parts build the same index as the whole"
    );
}

/// `docs/index-format.md`'s section under `heading`, up to the next heading
/// of the same level or higher.
fn section<'a>(doc: &'a str, heading: &str) -> &'a str {
    let level = heading.bytes().take_while(|&b| b == b'#').count();
    let start = doc
        .find(&format!("\n{heading}\n"))
        .unwrap_or_else(|| panic!("docs/index-format.md has no `{heading}`"));
    let body = &doc[start + heading.len() + 2..];
    let end = body
        .match_indices("\n#")
        .find(|(i, _)| body[i + 1..].bytes().take_while(|&b| b == b'#').count() <= level)
        .map_or(body.len(), |(i, _)| i);
    &body[..end]
}

/// The first cell of each row of the markdown tables in `text`, without its
/// backquotes.
fn first_cells(text: &str) -> Vec<&str> {
    text.lines()
        .filter_map(|l| l.strip_prefix("| `"))
        .filter_map(|l| l.split_once("` |").map(|(cell, _)| cell))
        .collect()
}

#[test]
fn p2_idx_01_index_format_doc_describes_every_column() {
    let doc = fs::read_to_string(workspace().join("docs/index-format.md")).unwrap();
    let db = applied(&[SCHEMA]);
    let tables = strings(
        &db,
        "SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE ?1 ORDER BY rowid",
        "sqlite_%",
    );
    for table in &tables {
        let columns = strings(&db, "SELECT name FROM pragma_table_info(?1)", table);
        let documented = first_cells(section(&doc, &format!("### `{table}`")));
        assert_eq!(documented, columns, "docs/index-format.md, `{table}`");
    }
    let keys = first_cells(section(&doc, "## `scan_meta` keys"));
    assert_eq!(
        keys, SCAN_META_KEYS,
        "docs/index-format.md, the `scan_meta` keys"
    );
    let changelog = section(&doc, "## Changelog");
    assert!(
        changelog.contains(&format!("\n### {INDEX_VERSION}\n")),
        "docs/index-format.md has no changelog entry for index_version {INDEX_VERSION}"
    );
}
