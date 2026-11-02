//! P2-BENCH-01: `bench/run.sh index` against the binary this test run built,
//! on a scaled T3, with `bench/testdata/hyperfine_stub.py` for hyperfine (CI's
//! test job does not install it), `true` for the cache drop, and stand-ins for
//! dua and fdu that answer as their pins: nothing touches the network. The
//! numbers it prints are a smoke tree's and mean nothing; the nightly measures
//! T2 on the bench runners.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Output;

use duscope_testkit::TestEnv;
use serde_json::Value;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

/// The interpreter `bench/run.sh` picks: `$PYTHON`, else `/usr/bin/python3`,
/// else `python3`.
fn python() -> String {
    std::env::var("PYTHON")
        .ok()
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| {
            if Path::new("/usr/bin/python3").is_file() {
                "/usr/bin/python3".to_owned()
            } else {
                "python3".to_owned()
            }
        })
}

fn script(path: &Path, body: &str) {
    fs::write(path, format!("#!/bin/sh\n{body}")).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// A dua that writes its snapshot with `i --once --export FILE TREE` and reads
/// it with `i --import FILE --once`, and an fdu that keeps nothing and exits
/// `fdu_status` when it answers.
fn stand_ins(scratch: &Path, fdu_status: u8) -> (PathBuf, PathBuf) {
    let dua = scratch.join("dua");
    script(
        &dua,
        "case \"$1\" in\n\
         --version) echo 'dua 2.46.0' ;;\n\
         i) while [ $# -gt 0 ]; do\n\
              case \"$1\" in\n\
                --export) echo snapshot > \"$2\" ;;\n\
                --import) [ -s \"$2\" ] || exit 3 ;;\n\
              esac\n\
              shift\n\
            done ;;\n\
         *) exit 2 ;;\n\
         esac\n",
    );
    let fdu = scratch.join("fdu");
    script(
        &fdu,
        &format!(
            "[ \"$1\" = --version ] && echo 'fdu 0.5.0' && exit 0\n\
             [ \"$2\" = --stale-ok ] && exit {fdu_status}\n\
             exit 0\n"
        ),
    );
    (dua, fdu)
}

/// `bench/run.sh index` on a scaled T3 under `scratch`, with `extra`.
fn index(env: &TestEnv, scratch: &Path, dua: &Path, fdu: &Path, extra: &[&str]) -> Output {
    let stub = repo_root().join("bench/testdata/hyperfine_stub.py");
    env.command(repo_root().join("bench/run.sh"))
        .arg("index")
        .arg("--duscope")
        .arg(env.cmd().get_program())
        .args(["--trees", "T3", "--scale", "0.001", "--jobs", "1"])
        .args(["--mode", "both", "--cold-cmd", "true"])
        .args(["--runs", "2", "--warmup", "1"])
        .arg("--tool-path")
        .arg(format!("dua={}", dua.display()))
        .arg("--tool-path")
        .arg(format!("fdu={}", fdu.display()))
        .arg("--hyperfine")
        .arg(format!("{} {}", python(), stub.display()))
        .arg("--trees-dir")
        .arg(scratch.join("warm"))
        .arg("--cold-trees-dir")
        .arg(scratch.join("cold"))
        .arg("--tools-dir")
        .arg(scratch.join("tools"))
        .arg("--results-dir")
        .arg(scratch.join("results"))
        .args(extra)
        .output()
        .unwrap()
}

fn result(out: &Output, scratch: &Path) -> Value {
    let file = PathBuf::from(String::from_utf8(out.stdout.clone()).unwrap().trim());
    assert!(
        file.starts_with(scratch.join("results")),
        "{}",
        file.display()
    );
    assert!(
        file.file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .ends_with("-index-x0.001.json")
    );
    serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap()
}

/// The saved scans each mode's trees directory held during the run.
fn left(dir: &Path) -> Vec<String> {
    fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.starts_with(".duscope-index-"))
        .collect()
}

#[test]
fn p2_bench_01_the_index_measurements_run_end_to_end() {
    let env = TestEnv::new();
    let scratch = env.tmp_dir();
    let (dua, fdu) = stand_ins(&scratch, 0);
    let out = index(&env, &scratch, &dua, &fdu, &[]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "exit {:?}: {stderr}",
        out.status.code()
    );
    let result = result(&out, &scratch);
    assert_eq!(result["kind"], "index");
    assert_eq!(result["failures"], serde_json::json!([]));
    assert_eq!(result["gates_tree"], "T3");
    // Every gate was measured, and says whether it holds, but the warm write
    // on macOS, which the target leaves to Linux.
    let gates = result["gates"].as_array().unwrap();
    assert_eq!(gates.len(), 15);
    for gate in gates {
        assert!(gate["value"].is_number(), "{gate}");
        let macos_write =
            gate["name"] == "write_us_per_entry_warm_p50" && result["platform"] == "macos";
        assert_eq!(gate["holds"].is_boolean(), !macos_write, "{gate}");
    }
    let tree = &result["trees"][0];
    assert_eq!(tree["name"], "T3");
    assert_ne!(tree["filesystem"], Value::Null);
    for kind in ["basic", "full"] {
        assert!(
            tree["index_size"][kind]["bytes_per_entry"]
                .as_f64()
                .unwrap()
                > 0.0
        );
    }
    assert_eq!(tree["write"]["warm"]["runs"].as_array().unwrap().len(), 2);
    assert!(tree["write"]["cold"]["share_of_scan"]["p50"].is_number());
    // The rows: five warm queries, the first after a cache drop, and a pair
    // with each competitor.
    let mut rows: Vec<String> = result["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| format!("{} {} {}", r["kind"], r["mode"], r["tool"]).replace('"', ""))
        .collect();
    rows.sort();
    assert_eq!(
        rows,
        [
            "d34 warm dua",
            "d34 warm fdu",
            "d34 warm top",
            "d34 warm top",
            "first_query cold top",
            "query warm by_type",
            "query warm by_type_subtree",
            "query warm explain",
            "query warm stale",
            "query warm top",
        ]
    );
    // dua answers from its snapshot under a pseudo-terminal.
    let dua_row = result["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["tool"] == "dua")
        .unwrap();
    let command = dua_row["command"].as_str().unwrap();
    assert!(
        command.starts_with("script ") && command.contains(" i --import "),
        "{command}"
    );
    // The saved scans and fdu's home, beside each mode's trees, are gone.
    for mode in ["warm", "cold"] {
        assert!(left(&scratch.join(mode)).is_empty(), "{mode}");
    }
    assert!(stderr.contains("gate dua_speedup_p50 >= 20"), "{stderr}");
}

#[test]
fn p2_bench_01_a_part_that_fails_leaves_the_others_measured() {
    let env = TestEnv::new();
    let scratch = env.tmp_dir();
    let (dua, fdu) = stand_ins(&scratch, 1);
    let out = index(&env, &scratch, &dua, &fdu, &[]);
    assert_eq!(out.status.code(), Some(1));
    let result = result(&out, &scratch);
    let failures: Vec<&str> = result["failures"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f.as_str().unwrap())
        .collect();
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(failures[0].starts_with("T3 fdu: "), "{failures:?}");
    let measured = |name: &str| {
        result["gates"]
            .as_array()
            .unwrap()
            .iter()
            .find(|g| g["name"] == name)
            .unwrap()["value"]
            .is_number()
    };
    assert!(!measured("fdu_speedup_p50"));
    assert!(measured("dua_speedup_p50"));
    assert!(measured("top_p95_ms"));
    // Only dua and fdu can be named: duscope is --duscope.
    let refused = index(
        &env,
        &scratch,
        &dua,
        &fdu,
        &["--tool-path", "duscope=/bin/true"],
    );
    assert_eq!(refused.status.code(), Some(2));
}
