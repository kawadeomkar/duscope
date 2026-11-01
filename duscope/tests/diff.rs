//! P2-QRY-06: `diff` of two saved scans: a golden for a scripted change set
//! (create, delete, rename, grow, hard-link), an unchanged tree diffing to
//! nothing, roots that do not match, and T2 against itself in time.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fs;
use std::path::Path;
use std::process::Output;
use std::time::Instant;

use duscope_testkit::{TestEnv, golden};
use serde_json::Value;

fn run(env: &TestEnv, args: &[&str]) -> Output {
    env.cmd().args(args).output().unwrap()
}

fn ok(out: &Output) -> Value {
    assert!(
        out.status.success(),
        "exit {:?}: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

/// Saves a scan of `root` and returns its id.
fn save(env: &TestEnv, root: &Path) -> String {
    let saved = ok(&run(
        env,
        &[
            "scan",
            root.to_str().unwrap(),
            "--save",
            "--json",
            "--threads",
            "4",
        ],
    ));
    saved["scan"]["id"].as_str().unwrap().to_owned()
}

const MIB: usize = 1 << 20;

/// A tree of files of known sizes under `root`.
fn tree(root: &Path) {
    for (path, size) in [
        ("docs/a.bin", 3 * MIB),
        ("docs/b.bin", 5 * MIB),
        ("docs/small.txt", 1000),
        ("old/x/1.bin", MIB),
        ("old/x/2.bin", MIB),
        ("photos/p.jpg", 4 * MIB),
    ] {
        let file = root.join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, vec![1u8; size]).unwrap();
    }
}

#[test]
fn p2_qry_06_golden_for_a_scripted_change_set() {
    let env = TestEnv::new();
    let root = env.tmp_dir().join("tree");
    tree(&root);
    let root = fs::canonicalize(&root).unwrap();
    let before = save(&env, &root);
    // Create, delete, rename, grow, hard-link.
    fs::write(root.join("docs/new.bin"), vec![2u8; 7 * MIB]).unwrap();
    fs::remove_dir_all(root.join("old")).unwrap();
    fs::rename(root.join("photos"), root.join("pictures")).unwrap();
    fs::write(root.join("docs/a.bin"), vec![3u8; 5 * MIB]).unwrap();
    // A later link of b.bin (it sorts after it): it counts nothing.
    fs::hard_link(root.join("docs/b.bin"), root.join("docs/b2.bin")).unwrap();
    let after = save(&env, &root);
    let out = run(&env, &["diff", &before, &after, "--json"]);
    let listed = ok(&out);
    let os = std::env::consts::OS;
    let text = duscope_testkit::normalize(&String::from_utf8(out.stdout).unwrap(), &root)
        .replace(&before, "<old>")
        .replace(&after, "<new>");
    golden(&format!("diff_json_changes@{os}.snap"), &text);
    let rows: Vec<(String, String)> = listed["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            let path = e["path"].as_str().unwrap();
            let rel = path
                .strip_prefix(root.to_str().unwrap())
                .unwrap_or(path)
                .trim_start_matches('/');
            (rel.to_owned(), e["change"].as_str().unwrap().to_owned())
        })
        .collect();
    let has = |path: &str, change: &str| {
        assert!(
            rows.iter().any(|(p, c)| p == path && c == change),
            "{path} {change}: {rows:?}"
        );
    };
    has("docs/new.bin", "added");
    has("old", "removed");
    has("photos", "removed");
    has("pictures", "added");
    has("docs/a.bin", "grown");
    has("docs/b2.bin", "added");
    let link = listed["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["path"].as_str().unwrap().ends_with("docs/b2.bin"))
        .unwrap();
    assert_eq!(link["bytes_delta"], 0);
    let old = listed["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["path"].as_str().unwrap().ends_with("/old"))
        .unwrap();
    // old/, old/x/ and two files.
    assert_eq!(old["collapsed_count"], 4);
    assert_eq!(listed["diff"]["old_scan"], before.as_str());
    assert_eq!(listed["diff"]["new_scan"], after.as_str());
    // Sorted by the size of the change.
    let deltas: Vec<u64> = listed["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["bytes_delta"].as_i64().unwrap().unsigned_abs())
        .collect();
    assert!(deltas.windows(2).all(|w| w[0] >= w[1]), "{deltas:?}");
}

#[test]
fn p2_qry_06_an_unchanged_tree_diffs_to_nothing() {
    let env = TestEnv::new();
    let home = fs::canonicalize(env.fixtures().path("home")).unwrap();
    let first = save(&env, &home);
    let second = save(&env, &home);
    assert_ne!(first, second);
    let listed = ok(&run(&env, &["diff", &first, &second, "--json"]));
    assert_eq!(listed["entries"], serde_json::json!([]));
    assert_eq!(listed["diff"]["net_bytes"], 0);
    assert_eq!(listed["diff"]["changed_count"], 0);
}

#[test]
fn p2_qry_06_scans_of_other_roots_are_incompatible_unless_under() {
    let env = TestEnv::new();
    let home = fs::canonicalize(env.fixtures().path("home")).unwrap();
    let library = home.join("Library");
    let whole = save(&env, &home);
    let part = save(&env, &library);
    let out = run(&env, &["diff", &whole, &part, "--json"]);
    assert_eq!(out.status.code(), Some(2));
    let error: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(error["error"]["code"], "incompatible_scans");
    // Both hold Library.
    let listed = ok(&run(
        &env,
        &[
            "diff",
            &whole,
            &part,
            "--under",
            library.to_str().unwrap(),
            "--json",
        ],
    ));
    assert_eq!(listed["entries"], serde_json::json!([]));
    assert_eq!(listed["query"]["path"], library.to_str().unwrap());
    // A path one scan does not hold.
    let out = run(
        &env,
        &[
            "diff",
            &whole,
            &part,
            "--under",
            home.join("Movies").to_str().unwrap(),
            "--json",
        ],
    );
    assert_eq!(out.status.code(), Some(2));
}

/// T2 against itself, under 2 s: `DUSCOPE_BENCH_INDEX` names a saved scan
/// of T2, and the build is a release one (`cargo test --release -p duscope
/// --test diff -- --ignored`).
#[test]
#[ignore = "times diff on T2; set DUSCOPE_BENCH_INDEX and run in release"]
fn p2_qry_06_t2_against_itself_under_two_seconds() {
    let Some(index) = std::env::var_os("DUSCOPE_BENCH_INDEX") else {
        return duscope_testkit::skip("DUSCOPE_BENCH_INDEX names no index");
    };
    let env = TestEnv::new();
    let index = index.to_str().unwrap().to_owned();
    let started = Instant::now();
    let listed = ok(&run(&env, &["diff", &index, &index, "--json"]));
    let secs = started.elapsed().as_secs_f64();
    assert_eq!(listed["entries"], serde_json::json!([]));
    eprintln!("diff of T2 against itself: {secs:.2} s");
    assert!(secs < 2.0, "{secs:.2} s");
}

#[test]
fn p2_qry_06_a_link_that_takes_the_charge_leaves_its_original_unchanged() {
    let env = TestEnv::new();
    let root = env.tmp_dir().join("tree");
    tree(&root);
    let root = fs::canonicalize(&root).unwrap();
    let before = save(&env, &root);
    // a0.bin sorts before b.bin, so the new link is charged with the bytes.
    fs::hard_link(root.join("docs/b.bin"), root.join("docs/a0.bin")).unwrap();
    let after = save(&env, &root);
    let listed = ok(&run(&env, &["diff", &before, &after, "--json"]));
    let rows = listed["entries"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(rows[0]["path"].as_str().unwrap().ends_with("docs/a0.bin"));
    assert_eq!(rows[0]["change"], "added");
    assert_eq!(listed["diff"]["net_bytes"], 0);
    assert_eq!(listed["diff"]["changed_count"], 0);
}
