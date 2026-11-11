//! P4-TEST-01: the redraw budget's measurement. `bench/run.sh redraw`, whole,
//! on a pseudo-terminal, against the binary this test run built on a scaled
//! T2 (the numbers are a smoke tree's and mean nothing; the nightly job
//! measures T2); the first-time check against a frame that came late; and
//! the nightly workflow running it on both bench runners.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use duscope_testkit::TestEnv;
use serde_json::Value;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

/// The interpreter `bench/run.sh` runs: `$PYTHON`, else the system's.
fn python() -> OsString {
    if let Some(python) = std::env::var_os("PYTHON").filter(|p| !p.is_empty()) {
        return python;
    }
    let system = Path::new("/usr/bin/python3");
    if system.is_file() {
        system.into()
    } else {
        "python3".into()
    }
}

/// Whether the views' first-time frames in `rows` are the three tables',
/// each once: as many first-time frames as tables, and the keys that drew
/// them those of the tables. The keys are compared as a set, not in order:
/// a frame that comes after `--frame-wait` is read by the next press, so one
/// slow frame moves a table's first frame to a later round (`3`, `4`, `2`),
/// which tells nothing about the classes.
fn views_first_time(rows: &[Value]) -> Result<(), String> {
    let row = rows
        .iter()
        .find(|row| row["kind"] == "views" && row["class"] == "recompute")
        .ok_or("no first-time row of the views")?;
    if row["first"] != 3 {
        return Err(format!("{} first-time frames, not 3: {row}", row["first"]));
    }
    let keys: Vec<&str> = row["keys"]
        .as_array()
        .ok_or_else(|| format!("no keys: {row}"))?
        .iter()
        .filter_map(Value::as_str)
        .collect();
    if keys.len() != 3
        || keys.into_iter().collect::<BTreeSet<_>>() != BTreeSet::from(["2", "3", "4"])
    {
        return Err(format!("not the three tables' first frames: {row}"));
    }
    Ok(())
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
    views_first_time(rows).unwrap();
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

/// `bench/redraw.py`'s rows of the views at `--rounds 2`, its twelve
/// presses answered by a stand-in viewer whose frame for press `late` comes
/// after the frame wait: that press reads nothing, and each later one the
/// frame of the press before it. One list of rows for each `late` from 0
/// (none late) to 12. Nothing waits.
const LATE_FRAME: &str = r#"
import json, sys
sys.path.insert(0, "bench")
import redraw

class Late:
    def __init__(self, late):
        self.late, self.count, self.unread = late, 0, []

    def press(self, key):
        self.unread.append((0.02 if key in redraw.TABLES else 0.002, 100))
        self.count += 1
        return (None, 0) if self.count == self.late else self.unread.pop(0)

keys = dict((kind, keys) for kind, _, keys in redraw.KEYS)["views"]
runs = [redraw.measure_keys(Late(late), "views", (), keys, 2) for late in range(13)]
print(json.dumps([redraw.rows_of("T2", "views", presses) for presses in runs]))
"#;

fn views_with_a_late_frame() -> Vec<Vec<Value>> {
    let out = Command::new(python())
        .args(["-B", "-I", "-c", LATE_FRAME])
        .current_dir(repo_root())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

#[test]
fn p4_test_01_one_late_frame_keeps_the_first_time_check() {
    let runs = views_with_a_late_frame();
    let recompute = |rows: &[Value]| -> Value {
        rows.iter()
            .find(|r| r["class"] == "recompute")
            .unwrap()
            .clone()
    };
    // On time, the tables' first frames in the order they were pressed.
    views_first_time(&runs[0]).unwrap();
    assert_eq!(
        recompute(&runs[0])["keys"],
        serde_json::json!(["2", "3", "4"])
    );
    // One frame late, wherever it falls among the twelve presses: a table
    // whose first press drew nothing in time has its first frame in the next
    // round (the first `2` late: `3`, `4`, `2`), the same three, once each.
    for (late, rows) in runs.iter().enumerate() {
        views_first_time(rows).unwrap_or_else(|e| panic!("press {late} late: {e}"));
    }
    assert_eq!(
        recompute(&runs[1])["keys"],
        serde_json::json!(["3", "4", "2"])
    );
    // What the check refuses: a table with no first frame, or one counted
    // twice.
    let with = |first: u64, keys: Value| {
        let mut row = recompute(&runs[0]);
        row["first"] = first.into();
        row["keys"] = keys;
        views_first_time(&[row])
    };
    assert!(with(3, serde_json::json!(["2", "3", "4"])).is_ok());
    assert!(with(2, serde_json::json!(["2", "3"])).is_err());
    assert!(with(3, serde_json::json!(["2", "3", "3"])).is_err());
    assert!(with(4, serde_json::json!(["2", "3", "4", "4"])).is_err());
    assert!(with(3, serde_json::json!(["2", "3", "4", "4"])).is_err());
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
