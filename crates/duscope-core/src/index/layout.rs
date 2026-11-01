//! The index's layout, as `schema` prints it: each table of [`SCHEMA`]
//! with its columns (declared type, nullability, the platforms that fill
//! it) and, from a saved scan, its rows.

use rusqlite::Connection;

use super::{Index, SCHEMA};
use crate::CoreError;
use crate::envelope::{ColumnEntry, TableEntry, values};

/// Both platforms.
const BOTH: &[&str] = &values::PLATFORMS;

/// macOS alone.
const MACOS: &[&str] = &[values::PLATFORMS[0]];

/// The columns only macOS fills, by table; `*` for every column: the
/// `FSEvents` state of a volume, and the clone groups, which only APFS
/// reports at scan time (docs/index-format.md).
const MACOS_ONLY: [(&str, &str); 5] = [
    ("scan_volumes", "fsevents_uuid"),
    ("scan_volumes", "fsevents_event_id"),
    ("scan_volumes", "os_build"),
    ("clones", "*"),
    ("shared_members", "clone_first"),
];

/// The platforms that fill column `column` of `table`.
pub fn platforms(table: &str, column: &str) -> &'static [&'static str] {
    let macos = MACOS_ONLY
        .iter()
        .any(|&(t, c)| t == table && (c == "*" || c == column));
    if macos { MACOS } else { BOTH }
}

/// Every table of [`SCHEMA`], in its order, with its columns; each one's
/// rows in `index`, when one is given.
pub fn tables(index: Option<&Index>) -> Result<Vec<TableEntry>, CoreError> {
    let layout = |e: rusqlite::Error| CoreError::IndexCorrupt {
        file: "the built-in layout".to_owned(),
        message: e.to_string(),
    };
    let db = Connection::open_in_memory().map_err(layout)?;
    db.execute_batch(SCHEMA).map_err(layout)?;
    let names: Vec<String> = db
        .prepare("SELECT name FROM sqlite_schema WHERE type = 'table' ORDER BY rowid")
        .and_then(|mut stmt| {
            stmt.query_map([], |row| row.get(0))
                .and_then(Iterator::collect)
        })
        .map_err(layout)?;
    let mut tables = Vec::with_capacity(names.len());
    for name in names {
        let columns: Vec<ColumnEntry> = db
            .prepare("SELECT name, type, \"notnull\", pk FROM pragma_table_info(?1)")
            .and_then(|mut stmt| {
                stmt.query_map([&name], |row| {
                    let column: String = row.get(0)?;
                    let not_null: bool = row.get(2)?;
                    let key: i64 = row.get(3)?;
                    let declared: String = row.get(1)?;
                    Ok(ColumnEntry {
                        platforms: platforms(&name, &column).to_vec(),
                        // An INTEGER PRIMARY KEY is the rowid: never null.
                        nullable: !(not_null || (key > 0 && declared == "INTEGER")),
                        r#type: declared,
                        name: column,
                    })
                })
                .and_then(Iterator::collect)
            })
            .map_err(layout)?;
        let row_count = match index {
            Some(index) => index.rows(&name)?,
            None => None,
        };
        tables.push(TableEntry {
            name,
            columns,
            row_count,
        });
    }
    Ok(tables)
}

#[cfg(test)]
mod tests {
    use super::{platforms, tables};

    #[test]
    fn p2_qry_08_the_platforms_are_the_proposals() {
        // The proposal's table of index columns: the FSEvents state and the
        // clone groups are macOS's; a time Linux keeps only on some
        // filesystems is still both platforms'.
        for (table, column) in [
            ("scan_volumes", "fsevents_uuid"),
            ("scan_volumes", "fsevents_event_id"),
            ("scan_volumes", "os_build"),
            ("clones", "dev"),
            ("clones", "clone_id"),
            ("clones", "first_id"),
            ("shared_members", "clone_first"),
        ] {
            assert_eq!(platforms(table, column), ["macos"], "{table}.{column}");
        }
        for (table, column) in [
            ("files", "btime"),
            ("files", "atime"),
            ("files", "flags"),
            ("scan_volumes", "case_sensitive"),
            ("scan_volumes", "normalization"),
            ("shared_members", "link_first"),
            ("hardlinks", "first_id"),
        ] {
            assert_eq!(
                platforms(table, column),
                ["macos", "linux"],
                "{table}.{column}"
            );
        }
        // Every macOS-only column names one the layout has.
        let layout = tables(None).unwrap();
        for (table, column) in super::MACOS_ONLY {
            let t = layout.iter().find(|t| t.name == table).unwrap();
            assert!(
                column == "*" || t.columns.iter().any(|c| c.name == column),
                "{table}.{column}"
            );
        }
        assert_eq!(layout.len(), 9);
        let dirs = layout.iter().find(|t| t.name == "dirs").unwrap();
        let id = dirs.columns.iter().find(|c| c.name == "id").unwrap();
        assert!(!id.nullable && id.r#type == "INTEGER");
        let errors = dirs.columns.iter().find(|c| c.name == "errors").unwrap();
        assert!(errors.nullable);
        assert!(layout.iter().all(|t| t.row_count.is_none()));
    }
}
