//! P4-TEST-01: the redraw budget's measurement. `bench/run.sh redraw`, whole,
//! on a pseudo-terminal, against the binary this test run built on a scaled
//! T2 (the numbers are a smoke tree's and mean nothing; the nightly job
//! measures T2); and the nightly workflow running it on both bench runners.

use std::fs;
use std::path::{Path, PathBuf};

use duscope_testkit::TestEnv;
use serde_json::Value;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

#[test]
fn p4_test_01_the_redraw_measurement_runs_end_to_end() {
    let env = TestEnv::new();
    let scratch = env.tmp_dir();
    let out = env
        .command(repo_root().join("bench/run.sh"))
        .arg("redraw")
        .arg("--duscope")
        .arg(env.cmd().get_program())
        .args(["--trees", "T2", "--scale", "0.002", "--jobs", "1"])
        .args(["--rounds", "2", "--frame-wait", "1"])
        .arg("--trees-dir")
        .arg(scratch.join("trees"))
        .arg("--tools-dir")
        .arg(scratch.join("tools"))
        .arg("--results-dir")
        .arg(scratch.join("results"))
        .args(["--note", "smoke"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "exit {:?}: {stderr}",
        out.status.code()
    );
    let file = PathBuf::from(String::from_utf8(out.stdout.clone()).unwrap().trim());
    assert!(
        file.file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .ends_with("-redraw-x0.002.json"),
        "{}",
        file.display()
    );
    let result: Value = serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(result["kind"], "redraw");
    assert_eq!(result["failures"], serde_json::json!([]), "{stderr}");
    assert_eq!(result["harness"]["note"], "smoke");
    assert_eq!(result["gates_tree"], "T2");
    let gates = result["gates"].as_array().unwrap();
    let lines: Vec<(&str, f64)> = gates
        .iter()
        .map(|gate| {
            assert!(gate["value"].is_number(), "{gate}");
            assert!(gate["holds"].is_boolean(), "{gate}");
            (
                gate["name"].as_str().unwrap(),
                gate["limit"].as_f64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        lines,
        [
            ("redraw_p95_ms_at_120x40", 16.0),
            ("redraw_recompute_max_ms", 1000.0)
        ]
    );
    let rows = result["results"].as_array().unwrap();
    let kinds: Vec<(&str, &str)> = rows
        .iter()
        .map(|row| {
            (
                row["kind"].as_str().unwrap(),
                row["class"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        kinds,
        [
            ("move", "navigation"),
            ("descend", "navigation"),
            ("zoom", "navigation"),
            ("zoom", "recompute"),
            ("repaint", "recompute"),
            ("focus", "navigation"),
            ("views", "navigation"),
            ("views", "recompute"),
            ("resize", "recompute"),
        ]
    );
    for row in rows {
        assert!(row["samples"].as_u64().unwrap() > 0, "{row}");
        assert!(row["p95_ms"].is_number(), "{row}");
        assert!(row["max_ms"].is_number(), "{row}");
        assert!(row["over_limit"].is_u64(), "{row}");
    }
    // The first-time frames stand in rows of their own: each table once, and
    // every first zoom.
    let row = |kind: &str| {
        rows.iter()
            .find(|row| row["kind"] == kind && row["class"] == "recompute")
            .unwrap()
    };
    assert_eq!(row("views")["first"], 3);
    assert_eq!(row("views")["keys"], serde_json::json!(["2", "3", "4"]));
    assert!(row("zoom")["first"].as_u64().unwrap() >= 1);
    assert_eq!(row("repaint")["first"], 0);
    // Every press is recorded in order, those that drew nothing too.
    let sessions = result["sessions"].as_array().unwrap();
    let played: Vec<(&str, usize)> = sessions
        .iter()
        .map(|session| {
            (
                session["kind"].as_str().unwrap(),
                session["presses"].as_array().unwrap().len(),
            )
        })
        .collect();
    assert_eq!(
        played,
        [
            ("move", 8),
            ("descend", 4),
            ("zoom", 6),
            ("repaint", 4),
            ("focus", 2),
            ("views", 12),
            ("resize", 4),
        ]
    );
}

#[test]
fn p4_test_01_the_nightly_job_measures_the_redraw_on_both_runners() {
    let workflow =
        fs::read_to_string(repo_root().join(".github/workflows/nightly-bench.yml")).unwrap();
    assert_eq!(
        workflow.matches("bench/run.sh redraw --duscope").count(),
        2,
        "the redraw is measured on each bench runner"
    );
    for platform in ["macos", "linux"] {
        let artifact = format!("name: bench-{platform}-redraw");
        assert!(workflow.contains(&artifact), "{artifact}");
    }
    assert!(workflow.contains("caffeinate -is bench/run.sh redraw"));
    // The publish job takes every bench-* artifact to bench-results.
    assert!(workflow.contains("pattern: bench-*"));
}
