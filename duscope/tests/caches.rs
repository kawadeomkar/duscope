//! P2-QRY-05: `caches` on the fixture tree: goldens on the fixture
//! projects (npm, cargo, python, gradle, pods, composer, Xcode), each cache
//! and by project; names without their marker, absent unless `--loose`;
//! `CACHEDIR.TAG` detection; the default `--min-size`; a saved scan read
//! below its root.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fs;
use std::path::Path;
use std::process::Output;

use duscope_testkit::{TestEnv, golden};

fn run(env: &TestEnv, args: &[&str]) -> Output {
    env.cmd().args(args).output().unwrap()
}

fn ok(out: &Output) -> serde_json::Value {
    assert!(
        out.status.success(),
        "exit {:?}: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

/// Each row's path relative to `root`.
fn paths(rows: &serde_json::Value, root: &Path) -> Vec<String> {
    let root = format!("{}/", root.to_str().unwrap());
    rows["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            let path = r["path"].as_str().unwrap();
            path.strip_prefix(&root).unwrap_or(path).to_owned()
        })
        .collect()
}

#[test]
fn p2_qry_05_goldens_on_the_fixture_projects() {
    let env = TestEnv::new();
    let home = fs::canonicalize(env.fixtures().path("home")).unwrap();
    let os = std::env::consts::OS;
    let base = [
        "caches",
        home.to_str().unwrap(),
        "--min-size",
        "0",
        "--limit",
        "100",
        "--threads",
        "4",
        "--no-save",
    ];
    let cases: [(&str, &[&str]); 3] = [
        ("json_projects", &["--json"]),
        ("json_by_project", &["--json", "--by-project"]),
        ("plain_projects", &["--plain", "--header"]),
    ];
    for (name, extra) in cases {
        let mut args = base.to_vec();
        args.extend(extra);
        let out = run(&env, &args);
        assert!(out.status.success(), "{args:?}");
        let text = String::from_utf8(out.stdout).unwrap();
        golden(
            &format!("caches_{name}@{os}.snap"),
            &duscope_testkit::normalize(&text, &home),
        );
    }
    let mut args = base.to_vec();
    args.push("--json");
    let listed = ok(&run(&env, &args));
    let rows = paths(&listed, &home);
    // One cache per ecosystem the package names, each with its command.
    for (path, by) in [
        ("Projects/npm-app/node_modules", "npm install"),
        ("Projects/rust-cli/target", "cargo build"),
        ("Projects/py-pyproject/.venv", "pip install"),
        ("Projects/gradle-app/build", "gradle build"),
        ("Projects/cmake-app/build", "cmake --build"),
        ("Projects/ios-app/Pods", "pod install"),
        ("Projects/php-app/vendor", "composer install"),
        ("Library/Developer/Xcode/DerivedData", "xcodebuild"),
    ] {
        let at = rows
            .iter()
            .position(|r| r == path)
            .unwrap_or_else(|| panic!("{path} is not listed: {rows:?}"));
        assert_eq!(listed["entries"][at]["regenerable_by"], by, "{path}");
    }
    // A cache inside a cache is part of it; a vendor without composer.json
    // is not one.
    assert!(
        !rows
            .iter()
            .any(|r| r.contains("node_modules/outer/node_modules"))
    );
    assert!(!rows.iter().any(|r| r == "Projects/go-mod/vendor"));
    // cargo's target holds a valid tag; the farther marker names the
    // project.
    let at = |path: &str| &listed["entries"][rows.iter().position(|r| r == path).unwrap()];
    assert_eq!(at("Projects/rust-cli/target")["cachedir_tag"], true);
    assert_eq!(
        at("Projects/monorepo/tools/scratch/node_modules")["project_root"],
        home.join("Projects/monorepo").to_str().unwrap()
    );
    assert!(at("Library/Developer/Xcode/DerivedData")["project_root"].is_null());
}

#[test]
fn p2_qry_05_a_target_without_cargo_toml_is_absent_unless_loose() {
    let env = TestEnv::new();
    let root = fs::canonicalize(env.fixtures().case_root("cache-names-without-markers")).unwrap();
    let args = [
        "caches",
        root.to_str().unwrap(),
        "--min-size",
        "0",
        "--no-save",
        "--json",
    ];
    // The tree lies inside the checkout's target directory, whose
    // Cargo.toml confirms that target and not the ones inside it.
    let strict = ok(&run(&env, &args));
    assert_eq!(strict["entries"], serde_json::json!([]));
    let mut loose_args = args.to_vec();
    loose_args.push("--loose");
    let out = run(&env, &loose_args);
    let loose = ok(&out);
    let rows = paths(&loose, &root);
    for name in [
        "target",
        "node_modules",
        "build",
        "vendor",
        "Pods",
        ".venv",
        "__pycache__",
        ".gradle",
        "DerivedData",
    ] {
        assert!(rows.iter().any(|r| r == name), "{name}: {rows:?}");
    }
    let entries = loose["entries"].as_array().unwrap();
    assert!(entries.iter().all(|r| r["confirmed"] == false));
    assert!(entries.iter().all(|r| r["project_root"].is_null()));
    assert!(
        loose["query"]["filters"]
            .as_array()
            .unwrap()
            .contains(&"--loose".into())
    );
    let os = std::env::consts::OS;
    golden(
        &format!("caches_json_loose@{os}.snap"),
        &duscope_testkit::normalize(&String::from_utf8(out.stdout).unwrap(), &root),
    );
}

#[test]
fn p2_qry_05_cachedir_tag_is_a_regular_file_with_the_signature() {
    let env = TestEnv::new();
    let root = fs::canonicalize(env.fixtures().case_root("cachedir-tag")).unwrap();
    let listed = ok(&run(
        &env,
        &[
            "caches",
            root.to_str().unwrap(),
            "--min-size",
            "0",
            "--sort",
            "path",
            "--no-save",
            "--json",
        ],
    ));
    // Not a wrong signature, a truncated tag, a directory named
    // CACHEDIR.TAG or a symbolic link to a valid tag.
    assert_eq!(paths(&listed, &root), ["exact", "valid"]);
    for row in listed["entries"].as_array().unwrap() {
        assert_eq!(row["rule"], "CACHEDIR.TAG");
        assert_eq!(row["regenerable_by"], "unknown");
        assert_eq!(row["cachedir_tag"], true);
        assert!(row["project_root"].is_null());
    }
}

#[test]
fn p2_qry_05_min_size_defaults_to_10_mib() {
    let env = TestEnv::new();
    let home = fs::canonicalize(env.fixtures().path("home")).unwrap();
    let listed = ok(&run(
        &env,
        &["caches", home.to_str().unwrap(), "--no-save", "--json"],
    ));
    assert_eq!(listed["query"]["filters"][0], "--min-size=10485760");
    let entries = listed["entries"].as_array().unwrap();
    assert!(!entries.is_empty());
    assert!(
        entries
            .iter()
            .all(|r| r["allocated_bytes"].as_u64().unwrap() >= 10 << 20)
    );
    // By project, the threshold is a project's total.
    let projects = ok(&run(
        &env,
        &[
            "caches",
            home.to_str().unwrap(),
            "--by-project",
            "--min-size",
            "1M",
            "--no-save",
            "--json",
        ],
    ));
    assert_eq!(projects["query"]["command"], "caches --by-project");
    for row in projects["entries"].as_array().unwrap() {
        assert!(row["allocated_bytes"].as_u64().unwrap() >= 1 << 20, "{row}");
        assert_eq!(
            row["cache_count"].as_u64().unwrap(),
            row["caches"].as_array().unwrap().len() as u64
        );
    }
}

#[test]
fn p2_qry_05_a_saved_scan_read_below_its_root_finds_the_marker_above() {
    let env = TestEnv::new();
    let home = fs::canonicalize(env.fixtures().path("home")).unwrap();
    ok(&run(
        &env,
        &["scan", home.to_str().unwrap(), "--save", "--json"],
    ));
    let target = home.join("Projects/rust-cli/target");
    let listed = ok(&run(
        &env,
        &[
            "caches",
            target.to_str().unwrap(),
            "--min-size",
            "0",
            "--no-scan",
            "--json",
        ],
    ));
    assert!(listed["scan"]["id"].is_string(), "read from the saved scan");
    let entries = listed["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_eq!(entries[0]["path"], target.to_str().unwrap());
    assert_eq!(
        entries[0]["project_root"],
        home.join("Projects/rust-cli").to_str().unwrap()
    );
}

#[test]
fn p2_qry_05_exclude_caches_is_refused() {
    let env = TestEnv::new();
    let home = fs::canonicalize(env.fixtures().path("home")).unwrap();
    let out = run(
        &env,
        &[
            "caches",
            home.to_str().unwrap(),
            "--exclude-caches",
            "--json",
        ],
    );
    assert_eq!(out.status.code(), Some(2));
    let envelope: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(envelope["error"]["code"], "usage");
}
