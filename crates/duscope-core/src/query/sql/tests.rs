//! P2-QRY-07: the parts of the sandbox that run no SQL.

use super::{DEFAULT_LIMIT, TABLES, human_size, limit, statement};
use crate::index::SCHEMA;
use crate::query::scaled;

#[test]
fn p2_qry_07_the_tables_are_the_index_layouts() {
    let created: Vec<&str> = SCHEMA
        .lines()
        .filter_map(|line| line.strip_prefix("CREATE TABLE "))
        .filter_map(|rest| rest.split_whitespace().next())
        .collect();
    let mut tables = TABLES.to_vec();
    let mut created_sorted = created.clone();
    tables.sort_unstable();
    created_sorted.sort_unstable();
    assert_eq!(tables, created_sorted);
}

#[test]
fn p2_qry_07_a_limit_counts_only_outside_parentheses_quotes_and_comments() {
    for (sql, limited) in [
        ("SELECT * FROM files", false),
        ("SELECT * FROM files LIMIT 3", true),
        ("select * from files limit 3 offset 2", true),
        ("SELECT * FROM files\nLIMIT\n3", true),
        ("SELECT * FROM (SELECT * FROM files LIMIT 3)", false),
        ("WITH t AS (SELECT 1 LIMIT 1) SELECT * FROM t", false),
        ("SELECT 'LIMIT 3' FROM files", false),
        ("SELECT \"limit\" FROM files", false),
        ("SELECT `limit`, [limit] FROM files", false),
        ("SELECT 'it''s' FROM files LIMIT 1", true),
        ("SELECT 1 -- LIMIT 3", false),
        ("SELECT 1 /* LIMIT 3 */", false),
        ("SELECT :limit, @limit, $limit", false),
        ("SELECT 1 UNION SELECT 2 LIMIT 1", true),
        ("SELECT 1 LIMIT 2; -- LIMIT", true),
        ("SELECT unlimited FROM files", false),
    ] {
        assert_eq!(statement(sql).1, limited, "{sql}");
    }
    assert_eq!(limit(false, 10_000), DEFAULT_LIMIT);
    assert_eq!(limit(false, 10), 10);
    assert_eq!(limit(true, 10_000), 10_000);
}

#[test]
fn p2_qry_07_the_statement_ends_at_its_first_semicolon() {
    assert_eq!(statement("SELECT 1; -- done").0, "SELECT 1");
    assert_eq!(statement("SELECT ';'; ").0, "SELECT ';'");
    assert_eq!(statement("SELECT 1 /* ; */").0, "SELECT 1 /* ; */");
    assert_eq!(statement("SELECT 1").0, "SELECT 1");
    // An unterminated quote runs to the end; an unterminated comment,
    // which would swallow the wrapper's parenthesis, is cut off.
    assert_eq!(statement("SELECT 'a;").0, "SELECT 'a;");
    assert_eq!(statement("SELECT 1 /* ;").0, "SELECT 1 ");
    assert!(statement("SELECT 1 LIMIT 2 /* x").1);
}

#[test]
fn p2_qry_07_human_size_is_the_tables_iec_with_one_decimal() {
    for (bytes, shown) in [
        (0, "0 B"),
        (1023, "1023 B"),
        (1024, "1.0 KiB"),
        (1536, "1.5 KiB"),
        (1_048_575, "1.0 MiB"),
        (442_708_615_168, "412.3 GiB"),
        (u64::MAX, "16.0 EiB"),
    ] {
        assert_eq!(human_size(bytes), shown, "{bytes}");
    }
    assert_eq!(scaled(999, 1000, &["B", "kB"]), "999 B");
    assert_eq!(scaled(999_950, 1000, &["B", "kB", "MB"]), "1.0 MB");
}
