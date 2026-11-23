//! P5-BENCH-07: `bench/nfs.py`, which measures what a scan of an NFS mount asks
//! the server for, and the decision gate **stats on network filesystems** it
//! answers. Its measurement needs two VMs and root, so these tests run the parts
//! that need neither: the per-operation counts read from a mountstats file, the
//! gate's fixed rule, and the committed result held to that rule (a missing
//! file fails).

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use duscope_testkit::TestEnv;
use serde_json::{Value, json};

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

fn nfs_py(args: &[&str]) -> Output {
    Command::new(python())
        .args(["-B", "-E", "-s"])
        .arg(repo_root().join("bench/nfs.py"))
        .args(args)
        .output()
        .unwrap()
}

/// Two NFS mounts as `/proc/self/mountstats` lists them, the first an `NFSv4.2`
/// one (no READDIRPLUS operation), the second an `NFSv3` one; trimmed to the
/// lines the reader looks at and a few around them.
const MOUNTSTATS: &str = "\
device /dev/sda1 mounted on / with fstype ext4
device 192.0.2.10:/srv/nfs-bench/T1-x0.25 mounted on /mnt/nfs-bench with fstype nfs4 statvers=1.1
\topts:\tro,vers=4.2,rsize=1048576,wsize=1048576,namlen=255,acregmin=3,acregmax=60,acdirmin=30,acdirmax=60,hard,proto=tcp,timeo=600,retrans=2,sec=sys
\tage:\t12
\tevents:\t4 125004 0 0 25001 3 0 0 0 0 0 25001 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0
\tRPC iostats version: 1.1  p/v: 100003/4 (nfs)
\tper-op statistics
\t        NULL: 1 1 0 44 24 0 0 0 0
\t        READ: 0 0 0 0 0 0 0 0 0
\t     GETATTR: 31250 31250 0 5000000 3000000 10 900 1000 0
\t      LOOKUP: 4 4 0 700 900 0 1 1 0
\t      ACCESS: 25001 25001 0 4000000 3000000 5 800 900 0
\t     READDIR: 25010 25010 0 4100000 90000000 20 3000 3500 0
device 192.0.2.10:/srv/other mounted on /mnt/other with fstype nfs statvers=1.1
\topts:\trw,vers=3
\tper-op statistics
\t     GETATTR: 99 99 0 0 0 0 0 0 0
\t READDIRPLUS: 7 7 0 0 0 0 0 0 0
";

#[test]
fn p5_bench_07_the_counts_are_the_mounts_own() {
    let env = TestEnv::new();
    let file = env.tmp_dir().join("mountstats");
    fs::write(&file, MOUNTSTATS).unwrap();
    let out = nfs_py(&["counts", file.to_str().unwrap(), "/mnt/nfs-bench"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let counts: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(counts["fstype"], "nfs4");
    assert!(counts["opts"].as_str().unwrap().contains("vers=4.2"));
    assert_eq!(counts["ops"]["GETATTR"], 31_250);
    assert_eq!(counts["ops"]["READDIR"], 25_010);
    assert_eq!(counts["ops"]["LOOKUP"], 4);
    assert!(
        counts["ops"].get("READDIRPLUS").is_none(),
        "NFSv4 has no READDIRPLUS"
    );

    let out = nfs_py(&["counts", file.to_str().unwrap(), "/mnt/other"]);
    let counts: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(counts["ops"]["READDIRPLUS"], 7);
    assert_eq!(counts["ops"]["GETATTR"], 99);

    let out = nfs_py(&["counts", file.to_str().unwrap(), "/"]);
    assert!(!out.status.success(), "/ is no NFS mount");
}

/// A result file whose medians are these GETATTR counts on a 125,000-entry
/// tree, recording `answer`.
fn result(scan_getattr: u64, du_getattr: u64, answer: &str) -> Value {
    let command = |name: &str, getattr: u64| {
        json!({
            "name": name,
            "ops_p50": {"READDIR": 25_010, "READDIRPLUS": null, "GETATTR": getattr, "LOOKUP": 4, "ACCESS": 25_001},
        })
    };
    json!({
        "tree": {"entries": 125_000},
        "commands": [
            command("duscope", scan_getattr),
            command("du", du_getattr),
            command("duscope --threads 1", 0),
        ],
        "decision": {"answer": answer},
    })
}

fn judge(env: &TestEnv, file: &Value) -> (Output, Value) {
    let path = env.tmp_dir().join("result.json");
    fs::write(&path, file.to_string()).unwrap();
    let out = nfs_py(&["judge", path.to_str().unwrap()]);
    let decision = serde_json::from_slice(&out.stdout).unwrap();
    (out, decision)
}

#[test]
fn p5_bench_07_the_gate_answers_by_its_rule() {
    let env = TestEnv::new();
    // More than one GETATTR per ten entries where du issues none: changed.
    let (out, decision) = judge(&env, &result(12_501, 4, "changed"));
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(decision["answer"], "changed");
    // One per ten exactly is not more than one per ten.
    let (_, decision) = judge(&env, &result(12_500, 4, "kept"));
    assert_eq!(decision["answer"], "kept");
    // du's own GETATTRs above one per thousand entries are not "none".
    let (_, decision) = judge(&env, &result(60_000, 126, "kept"));
    assert_eq!(decision["answer"], "kept");
    assert_eq!(decision["getattr_per_entry"]["duscope"], 0.48);
    // A file whose recorded answer its own counts contradict fails.
    let (out, _) = judge(&env, &result(60_000, 4, "kept"));
    assert!(!out.status.success());
}

#[test]
fn p5_bench_07_the_committed_result_follows_the_rule() {
    let file = repo_root().join("bench/results/nfs-readdirplus.json");
    let text = fs::read_to_string(&file)
        .unwrap_or_else(|e| panic!("{}: {e}: no NFS measurement is committed", file.display()));
    let result: Value = serde_json::from_str(&text).unwrap();
    for name in ["duscope", "du", "duscope --threads 1"] {
        let command = result["commands"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == name)
            .unwrap_or_else(|| panic!("{} has no {name} row", file.display()));
        assert!(command["p50_s"].as_f64().unwrap() > 0.0);
        assert!(command["ops_p50"]["GETATTR"].as_u64().is_some());
        assert!(command["ops_p50"]["READDIR"].as_u64().is_some());
    }
    assert_eq!(
        result["tree"]["entries"], result["tree"]["expected_entries"],
        "the export held another tree than T1 at 0.25"
    );
    let out = nfs_py(&["judge", file.to_str().unwrap()]);
    assert!(
        out.status.success(),
        "{}: {}",
        file.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}
