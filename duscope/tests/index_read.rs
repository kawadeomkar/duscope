//! P2-IDX-03: the list verbs read the newest saved scan that holds their
//! path, or scan, save and print the id; `--scan` names a scan; `scans`
//! lists, prunes and checks the saved scans.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::{Duration, SystemTime};

use duscope_testkit::TestEnv;
use rusqlite::Connection;

fn run(env: &TestEnv, args: &[&str]) -> Output {
    env.cmd().args(args).output().unwrap()
}

fn envelope(out: &Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "{e}: stdout {} stderr {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

fn ok(out: &Output) -> serde_json::Value {
    assert!(
        out.status.success(),
        "exit {:?}: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    envelope(out)
}

/// The index files under the test's index directory, by name.
fn saved(env: &TestEnv) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(env.index_dir().join("scans"))
        .map(|entries| {
            entries
                .map(|e| e.unwrap().file_name().into_string().unwrap())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

fn home(env: &TestEnv) -> PathBuf {
    fs::canonicalize(env.fixtures().path("home")).unwrap()
}

#[test]
fn p2_idx_03_top_without_an_index_scans_saves_and_prints_the_id() {
    let env = TestEnv::new();
    let home = home(&env);
    let home = home.to_str().unwrap();
    assert!(saved(&env).is_empty());
    // For people, the summary line carries the id the scan is saved as.
    let out = run(&env, &["top", home, "--format", "table"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let shown = String::from_utf8_lossy(&out.stdout);
    let files = saved(&env);
    assert_eq!(files.len(), 1, "{files:?}");
    let id = files[0].strip_suffix(".db").unwrap();
    assert!(
        shown.contains(&format!("threads, saved as {}", &id[..4])),
        "{shown}"
    );
    // The next query reads it: the same id, no new file.
    let again = ok(&run(&env, &["top", home, "--json"]));
    assert_eq!(again["scan"]["id"], id);
    assert_eq!(saved(&env).len(), 1);
    let table = run(&env, &["top", home, "--format", "table"]);
    let table = String::from_utf8_lossy(&table.stdout);
    assert!(
        table.contains(&format!("saved scan {} from ", &id[..4])),
        "{table}"
    );
}

#[test]
fn p2_idx_03_json_carries_the_id_of_the_scan_saved() {
    let env = TestEnv::new();
    let home = home(&env);
    let first = ok(&run(&env, &["tree", home.to_str().unwrap(), "--json"]));
    let id = first["scan"]["id"].as_str().unwrap().to_owned();
    assert_eq!(saved(&env), [format!("{id}.db")]);
    // A path inside the saved scan's root is read from it, re-rooted.
    let dev = home.join("dev");
    let sub = ok(&run(&env, &["top", dev.to_str().unwrap(), "--json"]));
    assert_eq!(sub["scan"]["id"], id.as_str());
    assert_eq!(sub["scan"]["root"], home.to_str().unwrap());
    assert_eq!(sub["query"]["path"], dev.to_str().unwrap());
    let fresh = ok(&run(
        &env,
        &["top", dev.to_str().unwrap(), "--json", "--no-save"],
    ));
    assert_eq!(sub["entries"], fresh["entries"]);
    assert_eq!(saved(&env).len(), 1);
}

#[test]
fn p2_idx_03_no_save_and_no_scan() {
    let env = TestEnv::new();
    let home = home(&env);
    let home = home.to_str().unwrap();
    // --no-scan with nothing saved: scan_not_found, exit 3.
    let out = run(&env, &["top", home, "--json", "--no-scan"]);
    assert_eq!(out.status.code(), Some(3));
    assert_eq!(envelope(&out)["error"]["code"], "scan_not_found");
    // --no-save scans and saves nothing; its envelope has no id.
    let fresh = ok(&run(&env, &["top", home, "--json", "--no-save"]));
    assert!(fresh["scan"]["id"].is_null());
    assert!(saved(&env).is_empty());
    // Once saved, --no-scan reads it.
    ok(&run(&env, &["scan", home, "--save", "--json"]));
    let read = ok(&run(&env, &["top", home, "--json", "--no-scan"]));
    assert!(read["scan"]["id"].is_string());
    // The two refuse to go together.
    let out = run(&env, &["top", home, "--no-scan", "--no-save"]);
    assert_eq!(out.status.code(), Some(2));
}

/// The scan block without what differs between two scans of the same tree:
/// the id, the times and the volumes' free space.
fn comparable(block: &serde_json::Value) -> serde_json::Value {
    let mut block = block.clone();
    let map = block.as_object_mut().unwrap();
    for key in ["id", "started_at", "finished_at"] {
        map.remove(key);
    }
    for volume in map["volumes"].as_array_mut().unwrap() {
        volume.as_object_mut().unwrap().remove("free_bytes");
    }
    block
}

#[test]
fn p2_idx_03_a_saved_scan_answers_as_the_scan_it_was() {
    let env = TestEnv::new();
    let home = home(&env);
    let home = home.to_str().unwrap();
    // The configuration's excludes keep the scan out of dev/node_modules,
    // so the scan block lists an excluded directory.
    let config = env.xdg_config_home().join("duscope");
    fs::create_dir_all(&config).unwrap();
    fs::write(
        config.join("config.toml"),
        "[excludes]\npatterns = [\"node_modules\"]\n",
    )
    .unwrap();
    for verb in ["top", "tree", "by-type", "stale"] {
        let fresh = ok(&run(&env, &[verb, home, "--json", "--no-save"]));
        assert!(saved(&env).is_empty());
        let saving = ok(&run(&env, &[verb, home, "--json"]));
        let read = ok(&run(&env, &[verb, home, "--json"]));
        assert_eq!(read["scan"]["id"], saving["scan"]["id"], "{verb}");
        assert!(
            !read["scan"]["excluded"].as_array().unwrap().is_empty(),
            "{verb}"
        );
        assert_eq!(
            comparable(&read["scan"]),
            comparable(&fresh["scan"]),
            "{verb}"
        );
        assert_eq!(read["entries"], fresh["entries"], "{verb}");
        assert_eq!(read["warnings"], fresh["warnings"], "{verb}");
        fs::remove_dir_all(env.index_dir().join("scans")).unwrap();
    }
}

#[test]
fn p2_idx_03_a_scan_taken_with_other_options_is_not_used() {
    let env = TestEnv::new();
    let home = home(&env);
    let home = home.to_str().unwrap();
    let confined = ok(&run(&env, &["scan", home, "--save", "-x", "--json"]));
    let basic = ok(&run(&env, &["top", home, "--json"]));
    assert_ne!(basic["scan"]["id"], confined["scan"]["id"]);
    assert_eq!(saved(&env).len(), 2);
    // --attrs full is not answered by a basic scan, though a full one
    // answers a basic query.
    let full = ok(&run(&env, &["top", home, "--json", "--attrs", "full"]));
    assert_ne!(full["scan"]["id"], basic["scan"]["id"]);
    assert_eq!(full["scan"]["attrs"], "full");
    let again = ok(&run(&env, &["top", home, "--json", "--attrs", "full"]));
    assert_eq!(again["scan"]["id"], full["scan"]["id"]);
    // --scan names one outright, whatever its options.
    let id = confined["scan"]["id"].as_str().unwrap();
    let named = ok(&run(&env, &["top", home, "--json", "--scan", &id[..6]]));
    assert_eq!(named["scan"]["id"], id);
    assert_eq!(named["scan"]["device_confined"], true);
}

#[test]
fn p2_idx_03_each_selector_and_its_failure() {
    let env = TestEnv::new();
    let home = home(&env);
    let home_s = home.to_str().unwrap();
    let saved_scan = ok(&run(&env, &["scan", home_s, "--save", "--json"]));
    let id = saved_scan["scan"]["id"].as_str().unwrap();
    let file = env.index_dir().join("scans").join(format!("{id}.db"));
    // A prefix, the whole id, latest and the file itself.
    let elsewhere = env.tmp_dir();
    for selector in [&id[..4], id, "latest", file.to_str().unwrap()] {
        let mut cmd = env.cmd();
        cmd.args(["top", "--json", "--scan", selector])
            .current_dir(&elsewhere);
        // latest looks for the path; the others name a scan, read at its
        // root when no path is given.
        if selector == "latest" {
            cmd.arg(home_s);
        }
        let read = ok(&cmd.output().unwrap());
        assert_eq!(read["scan"]["id"], id, "{selector}");
        assert_eq!(read["query"]["path"], home_s, "{selector}");
    }
    // What is not a selector: a usage error.
    let fail = |args: &[&str], exit: i32, code: &str| {
        let out = run(&env, args);
        assert_eq!(out.status.code(), Some(exit), "{args:?}");
        assert_eq!(envelope(&out)["error"]["code"], code, "{args:?}");
    };
    fail(&["top", "--json", "--scan", "zz"], 2, "usage");
    fail(&["top", "--json", "--scan", "a3f"], 2, "usage");
    // A prefix no scan has, a file that is not there, a path outside the
    // scan's root.
    fail(&["top", "--json", "--scan", "cccc"], 3, "scan_not_found");
    fail(
        &["top", "--json", "--scan", "./nothing.db"],
        3,
        "scan_not_found",
    );
    let outside = env.tmp_dir();
    fail(
        &["top", outside.to_str().unwrap(), "--json", "--scan", id],
        3,
        "scan_not_found",
    );
    // A file that is not an index, and one of another major version: exit 5.
    let junk = env.tmp_dir().join("junk.db");
    fs::write(&junk, b"no database here, not one page of it at all").unwrap();
    fail(
        &["top", "--json", "--scan", junk.to_str().unwrap()],
        5,
        "index_corrupt",
    );
    let v3 = env.tmp_dir().join("v3.db");
    let db = Connection::open(&v3).unwrap();
    db.execute_batch(
        "CREATE TABLE scan_meta (key TEXT PRIMARY KEY, value); \
         INSERT INTO scan_meta VALUES ('index_version', '3.0');",
    )
    .unwrap();
    drop(db);
    let out = run(&env, &["top", "--json", "--scan", v3.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(5));
    let error = &envelope(&out)["error"];
    assert_eq!(error["code"], "index_version_mismatch");
    assert!(
        error["hint"]
            .as_str()
            .unwrap()
            .contains("duscope scan --save")
    );
}

#[test]
fn p2_idx_03_a_saved_scan_older_than_max_age_warns() {
    let env = TestEnv::new();
    let home = home(&env);
    let home = home.to_str().unwrap();
    ok(&run(
        &env,
        &[
            "scan",
            home,
            "--save",
            "--json",
            "--test-now",
            "2030-01-01T00:00:00Z",
        ],
    ));
    let codes = |args: &[&str]| -> Vec<String> {
        ok(&run(&env, args))["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w["code"].as_str().unwrap().to_owned())
            .collect()
    };
    let week = ["top", home, "--json", "--test-now", "2030-01-05T00:00:00Z"];
    assert!(!codes(&week).contains(&"stale_index".to_owned()));
    let month = ["top", home, "--json", "--test-now", "2030-02-01T00:00:00Z"];
    assert!(codes(&month).contains(&"stale_index".to_owned()));
    let longer = [
        "top",
        home,
        "--json",
        "--test-now",
        "2030-02-01T00:00:00Z",
        "--max-age",
        "60d",
    ];
    assert!(!codes(&longer).contains(&"stale_index".to_owned()));
    // Still read, not rescanned.
    assert_eq!(saved(&env).len(), 1);
}

/// Sets `file`'s modification time `age` in the past.
fn age(file: &Path, age: Duration) {
    File::options()
        .write(true)
        .open(file)
        .unwrap()
        .set_modified(SystemTime::now() - age)
        .unwrap();
}

#[test]
fn p2_idx_03_prune_keeps_five_and_removes_a_stale_temp_file() {
    let env = TestEnv::new();
    let home = home(&env);
    let home = home.to_str().unwrap();
    let mut ids = Vec::new();
    for day in 1..=7 {
        let now = format!("2030-01-0{day}T00:00:00Z");
        let out = ok(&run(
            &env,
            &["scan", home, "--save", "--json", "--test-now", &now],
        ));
        ids.push(out["scan"]["id"].as_str().unwrap().to_owned());
    }
    // A scan of another root, as old: alone, it is kept.
    let dev = env.fixtures().path("home").join("dev");
    ok(&run(
        &env,
        &[
            "scan",
            dev.to_str().unwrap(),
            "--save",
            "--json",
            "--test-now",
            "2030-01-01T00:00:00Z",
        ],
    ));
    let scans = env.index_dir().join("scans");
    // A temporary file whose writer is gone, one whose writer runs, and
    // one older than a day.
    let gone = std::process::Command::new("true").spawn().unwrap();
    let gone_pid = gone.id();
    let mut gone = gone;
    gone.wait().unwrap();
    let dead = scans.join(format!("aaaaaaaaaaaa.db.tmp-{gone_pid}"));
    let running = scans.join(format!("bbbbbbbbbbbb.db.tmp-{}", std::process::id()));
    let old = scans.join(format!("cccccccccccc.db.tmp-{}", std::process::id()));
    for file in [&dead, &running, &old] {
        fs::write(file, b"half an index").unwrap();
    }
    age(&old, Duration::from_secs(25 * 3600));
    // Spill files: a week old, and a day.
    let out_dir = env.index_dir().join("out");
    fs::create_dir_all(&out_dir).unwrap();
    let old_spill = out_dir.join("old.json");
    let new_spill = out_dir.join("new.json");
    fs::write(&old_spill, b"{}").unwrap();
    fs::write(&new_spill, b"{}").unwrap();
    age(&old_spill, Duration::from_secs(8 * 86_400));
    age(&new_spill, Duration::from_secs(86_400));
    let pruned = ok(&run(
        &env,
        &[
            "scans",
            "--prune",
            "--json",
            "--test-now",
            "2030-03-01T00:00:00Z",
        ],
    ));
    let removed: Vec<(String, String)> = pruned["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["kind"].as_str().unwrap().to_owned(),
                e["path"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    let index = |id: &str| scans.join(format!("{id}.db")).to_str().unwrap().to_owned();
    let path = |p: &Path| p.to_str().unwrap().to_owned();
    // The two oldest scans of home go; the five newest stay.
    let mut removed = removed;
    removed.sort();
    let mut expected = vec![
        ("index".to_owned(), index(&ids[0])),
        ("index".to_owned(), index(&ids[1])),
        ("temporary".to_owned(), path(&dead)),
        ("temporary".to_owned(), path(&old)),
        ("spill".to_owned(), path(&old_spill)),
    ];
    expected.sort();
    assert_eq!(removed, expected);
    for id in &ids[2..] {
        assert!(scans.join(format!("{id}.db")).exists(), "{id}");
    }
    assert!(running.exists() && new_spill.exists());
    let left: Vec<String> = saved(&env)
        .into_iter()
        .filter(|n| n.ends_with(".db"))
        .collect();
    assert_eq!(left.len(), 6, "{left:?}");
    // Under seven days old, everything stays.
    let again = ok(&run(
        &env,
        &[
            "scans",
            "--prune",
            "--json",
            "--test-now",
            "2030-01-08T00:00:00Z",
        ],
    ));
    assert_eq!(again["entries"].as_array().unwrap().len(), 0);
}

#[test]
fn p2_idx_03_scans_lists_and_checks_the_saved_scans() {
    let env = TestEnv::new();
    let home = home(&env);
    let home = home.to_str().unwrap();
    let first = ok(&run(
        &env,
        &[
            "scan",
            home,
            "--save",
            "--json",
            "--test-now",
            "2030-01-01T00:00:00Z",
        ],
    ));
    let second = ok(&run(
        &env,
        &[
            "scan",
            home,
            "--save",
            "--json",
            "--test-now",
            "2030-01-02T00:00:00Z",
        ],
    ));
    let listed = ok(&run(&env, &["scans", "--json"]));
    let ids: Vec<&str> = listed["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_str().unwrap())
        .collect();
    // Newest first.
    assert_eq!(
        ids,
        [
            second["scan"]["id"].as_str().unwrap(),
            first["scan"]["id"].as_str().unwrap()
        ]
    );
    let row = &listed["entries"][0];
    assert_eq!(row["root"], home);
    assert_eq!(row["index_version"], "2.0");
    assert_eq!(
        row["allocated_bytes"],
        second["scan"]["total_allocated_bytes"]
    );
    assert_eq!(listed["query"]["command"], "scans");
    // --check: sound, then a damaged file.
    let id = ids[0];
    let checked = ok(&run(&env, &["scans", "--check", id, "--json"]));
    assert_eq!(checked["entries"][0]["quick_check"], "ok");
    let file = env.index_dir().join("scans").join(format!("{id}.db"));
    let mut bytes = fs::read(&file).unwrap();
    // The second page onwards overwritten: the header still says SQLite.
    for b in &mut bytes[4096..] {
        *b = 0xa5;
    }
    fs::write(&file, &bytes).unwrap();
    let out = run(&env, &["scans", "--check", id, "--json"]);
    assert_eq!(out.status.code(), Some(5));
    assert_eq!(envelope(&out)["error"]["code"], "index_corrupt");
}
