//! P2-BENCH-01: `bench/run.sh index` against the binary this test run built,
//! on a scaled T3, with `bench/testdata/hyperfine_stub.py` for hyperfine (CI's
//! test job does not install it), `true` for the cache drop, and stand-ins for
//! dua and fdu that answer as their pins and keep no snapshot of their own:
//! nothing touches the network. The numbers it prints are a smoke tree's and
//! mean nothing; the nightly measures T2 on the bench runners.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

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

#[test]
fn p2_bench_01_the_index_measurements_run_end_to_end() {
    let env = TestEnv::new();
    let scratch = env.tmp_dir();
    let dua = scratch.join("dua");
    // `i --once --export FILE TREE` writes the snapshot; `aggregate --import`
    // reads it.
    script(
        &dua,
        "case \"$1\" in\n\
         --version) echo 'dua 2.46.0' ;;\n\
         i) while [ $# -gt 0 ]; do [ \"$1\" = --export ] && echo snapshot > \"$2\"; shift; done ;;\n\
         aggregate) [ \"$2\" = --import ] && [ -s \"$3\" ] || exit 3 ;;\n\
         *) exit 2 ;;\n\
         esac\n",
    );
    let fdu = scratch.join("fdu");
    script(&fdu, "[ \"$1\" = --version ] && echo 'fdu 0.5.0'\nexit 0\n");
    let duscope = env.cmd().get_program().to_owned();
    let stub = repo_root().join("bench/testdata/hyperfine_stub.py");
    let hyperfine = format!("{} {}", python(), stub.display());
    let results = scratch.join("results");
    let out = env
        .command(repo_root().join("bench/run.sh"))
        .arg("index")
        .arg("--duscope")
        .arg(&duscope)
        .args(["--trees", "T3", "--scale", "0.001", "--jobs", "1"])
        .args(["--mode", "both", "--cold-cmd", "true", "--runs", "2", "--warmup", "1"])
        .arg("--tool-path")
        .arg(format!("dua={}", dua.display()))
        .arg("--tool-path")
        .arg(format!("fdu={}", fdu.display()))
        .arg("--hyperfine")
        .arg(&hyperfine)
        .arg("--trees-dir")
        .arg(scratch.join("trees"))
        .arg("--tools-dir")
        .arg(scratch.join("tools"))
        .arg("--results-dir")
        .arg(&results)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "exit {:?}: {stderr}", out.status.code());
    let file = PathBuf::from(String::from_utf8(out.stdout).unwrap().trim());
    assert!(file.starts_with(&results), "{}", file.display());
    assert!(
        file.file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .ends_with("-index-x0.001.json")
    );
    let result: Value = serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(result["kind"], "index");
    assert_eq!(result["failures"], serde_json::json!([]));
    // Every gate was measured, and says whether it holds.
    let gates = result["gates"].as_array().unwrap();
    assert_eq!(gates.len(), 15);
    for gate in gates {
        assert!(gate["value"].is_number(), "{gate}");
        assert!(gate["holds"].is_boolean(), "{gate}");
    }
    let tree = &result["trees"][0];
    assert_eq!(tree["name"], "T3");
    for kind in ["basic", "full"] {
        assert!(tree["index_size"][kind]["bytes_per_entry"].as_f64().unwrap() > 0.0);
    }
    assert_eq!(tree["write"]["warm"]["runs"].as_array().unwrap().len(), 2);
    assert!(tree["write"]["cold"]["share_of_scan"]["p50"].is_number());
    // The rows: five warm queries, the first after a cache drop, and a pair
    // with each competitor.
    let mut rows: Vec<(String, String, String)> = result["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["kind"].as_str().unwrap().to_owned(),
                r["mode"].as_str().unwrap().to_owned(),
                r["tool"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    rows.sort();
    let want: Vec<(String, String, String)> = [
        ("d34", "warm", "dua"),
        ("d34", "warm", "fdu"),
        ("d34", "warm", "top"),
        ("d34", "warm", "top"),
        ("first_query", "cold", "top"),
        ("query", "warm", "by_type"),
        ("query", "warm", "by_type_subtree"),
        ("query", "warm", "explain"),
        ("query", "warm", "stale"),
        ("query", "warm", "top"),
    ]
    .iter()
    .map(|(a, b, c)| ((*a).to_owned(), (*b).to_owned(), (*c).to_owned()))
    .collect();
    assert_eq!(rows, want);
    // The saved scans, beside the trees, are gone.
    let left: Vec<String> = fs::read_dir(scratch.join("trees"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.starts_with(".duscope-index-"))
        .collect();
    assert!(left.is_empty(), "{left:?}");
    assert!(stderr.contains("gate dua_speedup_p50 >= 20"), "{stderr}");
}
