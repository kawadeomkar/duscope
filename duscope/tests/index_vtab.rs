//! P2-IDX-04: the index is loaded the way bench/results/index-vtab.json
//! decides: by virtual tables only when they win the whole write by more
//! than 10% on a measured platform, else by the prepared-statement writer,
//! with no vtab module and no vtab feature left behind.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fs;
use std::path::Path;

fn read(relative: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(relative);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[test]
fn p2_idx_04_the_writer_follows_the_measured_rule() {
    let result: serde_json::Value =
        serde_json::from_str(&read("bench/results/index-vtab.json")).unwrap();
    let mut vtab_wins = false;
    for m in result["measurements"].as_array().unwrap() {
        // The median repeat's figures, as the file records them.
        let median = |key: &str| {
            let mut values: Vec<f64> = m["repeats"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r[key].as_f64().unwrap())
                .collect();
            values.sort_by(f64::total_cmp);
            values[values.len() / 2]
        };
        let (prepared, vtab, writer) = (
            median("prepared_load_s"),
            median("vtab_load_s"),
            median("writer_s"),
        );
        let entries = m["entries"].as_f64().unwrap();
        let near = |key: &str, value: f64, within: f64| {
            let recorded = m[key].as_f64().unwrap();
            assert!(
                (recorded - value).abs() < within,
                "{key}: {recorded} against {value:.3}"
            );
        };
        near(
            "prepared_load_us_per_entry",
            prepared * 1e6 / entries,
            0.002,
        );
        near("vtab_load_us_per_entry", vtab * 1e6 / entries, 0.002);
        near("writer_us_per_entry", writer * 1e6 / entries, 0.002);
        near("load_gain_pct", (prepared - vtab) / prepared * 100.0, 0.1);
        let write_gain = (prepared - vtab) / writer * 100.0;
        near("write_gain_pct", write_gain, 0.1);
        vtab_wins |= write_gain > 10.0;
    }
    let decision = result["decision"].as_str().unwrap();
    assert_eq!(
        decision.starts_with("undecided, default kept"),
        !vtab_wins,
        "{decision}"
    );
    // The prepared-statement writer loads dirs and files, and no virtual
    // table, module or feature is beside it.
    let vtab =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../crates/duscope-core/src/index/vtab.rs");
    assert_eq!(vtab.exists(), vtab_wins);
    let writer = read("crates/duscope-core/src/index/writer.rs");
    for table in ["dirs", "files"] {
        assert_eq!(
            writer.contains(&format!(
                "tx.prepare(\n        \"INSERT INTO {table} VALUES"
            )),
            !vtab_wins,
            "the {table} rows' prepared INSERT"
        );
    }
    for manifest in [
        "Cargo.toml",
        "crates/duscope-core/Cargo.toml",
        "duscope/Cargo.toml",
    ] {
        let text = read(manifest);
        for feature in [
            "\"vtab\"",
            "rusqlite/vtab",
            "\"array\"",
            "\"series\"",
            "\"csvtab\"",
        ] {
            assert_eq!(
                text.contains(feature),
                vtab_wins && feature == "\"vtab\"",
                "{manifest}: {feature}"
            );
        }
    }
}
