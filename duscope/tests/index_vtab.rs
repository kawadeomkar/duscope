//! P2-IDX-04: the index is loaded the way bench/results/index-vtab.json
//! decides: by a virtual table only when it wins the whole write by more
//! than 10%, else by the prepared-statement writer, with no vtab module
//! and no vtab feature left behind.

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
    let t2 = result["measurements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["tree"] == "T2")
        .expect("a T2 measurement");
    // The gain on the whole write, from the repeats' medians.
    let median = |key: &str| {
        let mut values: Vec<f64> = t2["repeats"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r[key].as_f64().unwrap())
            .collect();
        values.sort_by(f64::total_cmp);
        values[values.len() / 2]
    };
    let saved = median("prepared_s") - median("vtab_s");
    let write_gain = saved / median("writer_s") * 100.0;
    assert!(
        (write_gain - t2["write_gain_pct"].as_f64().unwrap()).abs() < 0.5,
        "write_gain_pct: {write_gain:.1}"
    );
    let vtab_wins = write_gain > 10.0;
    let decision = result["decision"].as_str().unwrap();
    assert_eq!(
        decision.starts_with("undecided, default kept"),
        !vtab_wins,
        "{decision}"
    );
    // The prepared-statement writer, and no virtual table beside it.
    let vtab =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../crates/duscope-core/src/index/vtab.rs");
    assert_eq!(vtab.exists(), vtab_wins);
    let manifest = read("Cargo.toml");
    let rusqlite = manifest
        .lines()
        .find(|l| l.starts_with("rusqlite = "))
        .unwrap();
    assert_eq!(rusqlite.contains("\"vtab\""), vtab_wins, "{rusqlite}");
    assert!(read("crates/duscope-core/src/index/writer.rs").contains("tx.prepare("));
}
