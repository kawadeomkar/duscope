//! P5-BENCH-04: the main path in the nightly (DEV-213): T4, the generated
//! scan with log-normal file sizes; `scan --save` end to end and the write per
//! entry (`bench/run.sh save`); the rescan by phase at shares of the
//! directories changed (`bench/run.sh rescan`); and the judge holding duscope
//! to diskus at its best, which `bench/test_acceptance.py` tests.
//!
//! `save` and `rescan` run whole against the binary this test run built, on
//! scaled trees, with `true` for the cache drop: nothing touches the network,
//! and their numbers are a smoke tree's. The nightly's two new steps run for
//! real under bash, against stand-ins for `bench/run.sh` and `caffeinate`
//! that record their arguments. The lines the harness reads the phases from
//! are taken from the binary's source, filled in as the binary fills them,
//! and matched by the harness's own patterns. The Python tests run under the
//! interpreter `bench/run.sh` picks.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use duscope_testkit::TestEnv;
use serde_json::Value;

const NIGHTLY: &str = ".github/workflows/nightly-bench.yml";
const SCALE: &str = "0.0002";
/// T4 at `SCALE`: its digest, the first 16 bytes of the export's SHA-256,
/// and its apparent bytes, which `bench/test_bench.py` pins too. A change to
/// the generator that changes them bumps `EXPORT_VERSION` in `bench/trees.py`.
const T4_SMALL: (&str, u64) = ("57095a3efa018889f8593d2855b86c5a", 5_999_273);

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn read(relative: &str) -> String {
    let path = root().join(relative);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
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

fn platform() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    }
}

fn succeeded(out: &Output, what: &str) -> String {
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "{what} failed ({}):\nstdout: {stdout}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    stdout
}

fn script(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// `bench/run.sh VERB ARGS --trees-dir DIR` under the test's environment.
fn bench(env: &TestEnv, verb: &str, args: &[&str], trees_dir: &Path) -> Output {
    env.command(root().join("bench/run.sh"))
        .arg(verb)
        .args(args)
        .arg("--trees-dir")
        .arg(trees_dir)
        .output()
        .unwrap()
}

fn result(out: &Output, suffix: &str) -> Value {
    let file = PathBuf::from(String::from_utf8(out.stdout.clone()).unwrap().trim());
    assert!(
        file.to_str().unwrap().ends_with(suffix),
        "{}: {}",
        file.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap()
}

/// P5-BENCH-04: the harness's unit tests of T4, the saves, the rescan's
/// phases and levels, and the judge.
#[test]
fn p5_bench_04_python_unit_tests() {
    let out = Command::new(python())
        .args([
            "-B", "-E", "-s", "-m", "unittest", "discover", "-s", "bench",
        ])
        .args(["-p", "test_*.py", "-k", "p5_bench_04"])
        .current_dir(root())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "bench's p5_bench_04 tests:\n{stderr}");
    assert!(
        !stderr.contains("Ran 0 tests"),
        "no p5_bench_04 test ran:\n{stderr}"
    );
}

/// P5-BENCH-04: T4 is byte-identical across two generations from one seed,
/// with one process or several, and its stamp holds the file's digest,
/// which `verify` reads again.
#[test]
fn p5_bench_04_t4_is_byte_identical_across_two_generations_from_one_seed() {
    let env = TestEnv::new();
    let scratch = env.tmp_dir();
    let mut exports = Vec::new();
    for (dir, jobs) in [("a", "1"), ("b", "1"), ("c", "3")] {
        let trees = scratch.join(dir);
        let args = ["--trees", "T4", "--scale", SCALE, "--jobs", jobs];
        let out = bench(&env, "build", &args, &trees);
        succeeded(&out, "bench/run.sh build --trees T4");
        let file = trees.join(format!("T4-x{SCALE}.ncdu.json"));
        exports.push(fs::read(&file).unwrap());
        let stamp: Value = serde_json::from_str(
            &fs::read_to_string(trees.join(format!("T4-x{SCALE}.json"))).unwrap(),
        )
        .unwrap();
        assert_eq!(stamp["digest"], T4_SMALL.0, "{stamp}");
        assert_eq!(stamp["apparent_bytes"], T4_SMALL.1, "{stamp}");
        let out = bench(&env, "verify", &args[..4], &trees);
        succeeded(&out, "bench/run.sh verify --trees T4");
    }
    assert_eq!(exports[0], exports[1]);
    assert_eq!(exports[0], exports[2]);
    // An ncdu export, naming no path of the machine that made it.
    let text = String::from_utf8(exports[0].clone()).unwrap();
    assert!(
        text.starts_with("[1,2,{\"progname\":\"duscope-bench\""),
        "{}",
        &text[..80]
    );
    assert!(!text.contains(scratch.to_str().unwrap()));
}

/// P5-BENCH-04: `save` times `scan --save` end to end on T1 to T3 and an
/// `import` of T4, warm and cold, reads the write per entry from `-v` in each
/// run, and leaves no saved scan behind in either trees directory.
#[test]
fn p5_bench_04_save_times_scan_save_end_to_end_and_the_write_on_t4() {
    let env = TestEnv::new();
    let scratch = env.tmp_dir();
    let bin = env.cmd().get_program().to_owned();
    let out = env
        .command(root().join("bench/run.sh"))
        .arg("save")
        .arg("--duscope")
        .arg(&bin)
        .args(["--scale", SCALE, "--jobs", "1", "--mode", "both"])
        .args(["--cold-cmd", "true", "--runs", "2", "--warmup", "1"])
        .arg("--trees-dir")
        .arg(scratch.join("warm"))
        .arg("--cold-trees-dir")
        .arg(scratch.join("cold"))
        .arg("--results-dir")
        .arg(scratch.join("results"))
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "{stderr}");
    let result = result(&out, &format!("-{}-save-x{SCALE}.json", platform()));
    assert_eq!(result["kind"], "save");
    assert_eq!(result["failures"], Value::Array(vec![]));
    let rows = result["results"].as_array().unwrap();
    let got: BTreeSet<String> = rows
        .iter()
        .map(|r| format!("{} {} {}", r["tree"], r["mode"], r["tool"]).replace('"', ""))
        .collect();
    let mut want = BTreeSet::new();
    for mode in ["cold", "warm"] {
        for tree in ["T1", "T2", "T3"] {
            want.insert(format!("{tree} {mode} scan --save"));
        }
        want.insert(format!("T4 {mode} import"));
    }
    assert_eq!(got, want);
    for row in rows {
        assert_eq!(row["run_count"], 2, "{row}");
        assert!(row["p50_s"].as_f64().unwrap() > 0.0, "{row}");
        assert!(
            row["write_us_per_entry_p50"].as_f64().unwrap() > 0.0,
            "{row}"
        );
        let scan = row["tool"] == "scan --save";
        assert_eq!(row["walk_s_p50"].is_number(), scan, "{row}");
    }
    // T4's 25 directories, 175 files and root reach the writer through import.
    let t4 = rows.iter().find(|r| r["tree"] == "T4").unwrap();
    assert_eq!(t4["entries"], 201, "{t4}");
    let trees = result["trees"].as_array().unwrap();
    assert_eq!(trees[3]["source"], "ncdu export");
    assert_eq!(trees[3]["digest"], T4_SMALL.0);
    let figures = &result["figures"];
    assert!(figures["T2"]["save_p50_s"]["cold"].is_number(), "{figures}");
    assert!(figures["T4"]["save_p50_s"]["warm"].is_null(), "{figures}");
    assert!(
        figures["T4"]["write_us_per_entry_p50"]["warm"].is_number(),
        "{figures}"
    );
    // Each saved scan was removed, and the work directories with them.
    for dir in ["warm", "cold"] {
        let left: Vec<String> = fs::read_dir(scratch.join(dir))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(".duscope-save-") || n == "scans")
            .collect();
        assert!(left.is_empty(), "{dir}: {left:?}");
    }
}

/// P5-BENCH-04: `rescan` times the rescan by phase at each level: the full
/// rescan's walk, roll-up and write, and loading the parent before them, on
/// both platforms; on Linux the incremental rescan is refused in the binary's
/// words, and on macOS it is measured, or refused with the reason (a tree
/// built moments before is all one chunk of FSEvents history). The tree
/// matches its stamp after.
#[test]
fn p5_bench_04_rescan_times_each_phase_at_each_level() {
    let env = TestEnv::new();
    let scratch = env.tmp_dir();
    let bin = env.cmd().get_program().to_owned();
    let trees_dir = scratch.join("trees");
    let out = env
        .command(root().join("bench/run.sh"))
        .arg("rescan")
        .arg("--duscope")
        .arg(&bin)
        .args(["--trees", "T2", "--scale", SCALE, "--jobs", "1"])
        .args([
            "--levels", "0,10", "--runs", "2", "--warmup", "1", "--settle", "1",
        ])
        .arg("--trees-dir")
        .arg(&trees_dir)
        .arg("--results-dir")
        .arg(scratch.join("results"))
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let result = result(&out, &format!("-{}-rescan-x{SCALE}.json", platform()));
    assert_eq!(result["kind"], "rescan");
    for failure in result["failures"].as_array().unwrap() {
        let failure = failure.as_str().unwrap();
        assert!(
            platform() == "macos" && failure.contains("raise --settle"),
            "{failure}\n{stderr}"
        );
    }
    assert_eq!(
        out.status.success(),
        result["failures"].as_array().unwrap().is_empty(),
        "{stderr}"
    );
    let tree = &result["trees"][0];
    assert_eq!(tree["name"], "T2");
    let levels = tree["levels"].as_array().unwrap();
    let shares: Vec<(f64, u64)> = levels
        .iter()
        .map(|l| {
            (
                l["percent"].as_f64().unwrap(),
                l["changed_dirs"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(shares, [(0.0, 0), (10.0, 2)]);
    for level in levels {
        let full = &level["rescan --full"];
        assert_eq!(full["status"], "measured", "{level}");
        assert_eq!(full["runs"].as_array().unwrap().len(), 2, "{level}");
        for phase in ["parent_s", "walk_s", "rollup_s", "write_s", "rest_s"] {
            assert!(full["phases_p50_s"][phase].is_number(), "{phase}: {full}");
        }
        for phase in ["replay_s", "read_back_s", "plan_s"] {
            assert!(full["phases_p50_s"][phase].is_null(), "{phase}: {full}");
        }
        let incremental = &level["rescan --incremental"];
        if incremental["status"] == "measured" {
            for phase in ["plan_s", "walk_s", "read_back_s", "post_check_s"] {
                assert!(
                    incremental["phases_p50_s"][phase].is_number(),
                    "{phase}: {incremental}"
                );
            }
        } else {
            let reason = incremental["reason"].as_str().unwrap();
            if platform() == "linux" {
                assert!(reason.contains("keeps no change history"), "{reason}");
            } else {
                assert!(reason.contains("not possible ("), "{reason}");
            }
        }
    }
    if platform() == "linux" {
        assert_eq!(tree["incremental"], "not measured");
    }
    let verified = bench(
        &env,
        "verify",
        &["--trees", "T2", "--scale", SCALE],
        &trees_dir,
    );
    succeeded(&verified, "bench/run.sh verify after the rescans");
}

/// A job of the nightly workflow: its lines, as `nightly_bench.rs` reads them.
struct Job {
    lines: Vec<String>,
}

fn job(workflow: &str, id: &str) -> Job {
    let head = format!("  {id}:");
    let lines = workflow
        .lines()
        .skip_while(|l| *l != head)
        .skip(1)
        .take_while(|l| l.is_empty() || l.starts_with("   ") || l.starts_with("  #"))
        .map(str::to_owned)
        .collect();
    Job { lines }
}

impl Job {
    /// The step that starts with `- name: NAME` or `- uses:` after it: its
    /// lines up to the next step.
    fn step(&self, start: &str) -> Vec<&str> {
        let at = self
            .lines
            .iter()
            .position(|l| l.trim() == start)
            .unwrap_or_else(|| panic!("no step `{start}`"));
        let mut out = vec![self.lines[at].as_str()];
        out.extend(
            self.lines[at + 1..]
                .iter()
                .take_while(|l| {
                    !l.trim_start().starts_with("- ") && !l.trim_start().starts_with('#')
                })
                .map(String::as_str),
        );
        out
    }

    /// The `run: |` script of the step named `name`, dedented.
    fn script(&self, name: &str) -> String {
        let step = self.step(&format!("- name: {name}"));
        let at = step.iter().position(|l| l.trim() == "run: |").unwrap();
        let indent = step[at].len() - step[at].trim_start().len() + 2;
        step[at + 1..]
            .iter()
            .take_while(|l| l.trim().is_empty() || l.len() - l.trim_start().len() >= indent)
            .map(|l| format!("{}\n", l.get(indent..).unwrap_or_default()))
            .collect()
    }

    /// The upload step whose `name:` is `artifact`.
    fn upload(&self, artifact: &str) -> Vec<String> {
        let at = self
            .lines
            .iter()
            .position(|l| l.trim() == format!("name: {artifact}"))
            .unwrap_or_else(|| panic!("no artifact `{artifact}`"));
        let start = self.lines[..at]
            .iter()
            .rposition(|l| {
                l.trim_start()
                    .starts_with("- uses: actions/upload-artifact@")
            })
            .unwrap();
        self.lines[start..]
            .iter()
            .take_while(|l| !l.trim_start().starts_with("- name:"))
            .map(|l| l.trim().to_owned())
            .collect()
    }
}

/// P5-BENCH-04: one nightly carries every new figure: each bench job runs
/// `save` on T1 to T4 and `rescan` on T1 and T2 after the index
/// measurements, on the trees the harness reads (bench-macos: warm on the
/// internal disk, cold on the image the cold command remounts), unless the
/// dispatch is a sweep; keeps each file as an artifact the publish job pushes
/// to bench-results (`bench-*`); and fails when one could not be measured.
#[test]
fn p5_bench_04_the_nightly_records_every_new_figure_on_both_runners() {
    let workflow = read(NIGHTLY);
    for platform in ["macos", "linux"] {
        let job = job(&workflow, &format!("bench-{platform}"));
        let order: Vec<usize> = [
            "- name: Index measurements",
            "- name: Main path measurements",
            "- name: Rescan measurements",
            "- name: Redraw measurements",
        ]
        .iter()
        .map(|s| job.lines.iter().position(|l| l.trim() == *s).unwrap())
        .collect();
        assert!(
            order.windows(2).all(|w| w[0] < w[1]),
            "{platform}: {order:?}"
        );
        for (name, id, artifact) in [
            ("Main path measurements", "save", "save"),
            ("Rescan measurements", "rescan", "rescan"),
        ] {
            let step = job.step(&format!("- name: {name}"));
            assert!(
                step.iter().any(|l| l.trim() == format!("id: {id}")),
                "{step:?}"
            );
            assert!(
                step.iter()
                    .any(|l| l.trim() == "if: ${{ !cancelled() && env.SWEEP != 'true' }}"),
                "{platform} {name}: {step:?}"
            );
            let upload = job.upload(&format!("bench-{platform}-{artifact}"));
            assert!(
                upload.contains(&format!(
                    "if: ${{{{ !cancelled() && steps.{id}.outputs.file != '' }}}}"
                )),
                "{upload:?}"
            );
            assert!(
                upload.contains(&format!("path: ${{{{ steps.{id}.outputs.file }}}}")),
                "{upload:?}"
            );
            let fail = job.script(&format!(
                "Fail when the {} measurements did",
                name.split(" measurements").next().unwrap().to_lowercase()
            ));
            assert!(fail.contains("exit 1"), "{fail}");
        }
    }
    // The publish job takes every bench-* artifact.
    let publish = job(&workflow, "publish");
    assert!(
        publish.lines.iter().any(|l| l.trim() == "pattern: bench-*"),
        "publish downloads the bench-* artifacts"
    );

    // The steps' scripts, run against stand-ins that record their arguments.
    let stand_in = tempfile::tempdir().unwrap();
    let dir = fs::canonicalize(stand_in.path()).unwrap();
    fs::create_dir_all(dir.join("bench")).unwrap();
    fs::create_dir_all(dir.join("bin")).unwrap();
    fs::create_dir_all(dir.join("runner-temp")).unwrap();
    script(
        &dir.join("bench/run.sh"),
        "#!/usr/bin/env bash\nprintf '%s\\n' \"$@\" > \"$STUB_DIR/args\"\nout=\"$STUB_DIR/result.json\"\necho '{}' > \"$out\"\necho \"$out\"\n",
    );
    script(
        &dir.join("bin/caffeinate"),
        "#!/usr/bin/env bash\n[ \"$1\" = -is ] && shift\nexec \"$@\"\n",
    );
    let home = dir.join("bench-home");
    let path = format!(
        "{}:{}",
        dir.join("bin").display(),
        std::env::var("PATH").unwrap()
    );
    let run = |platform: &str, name: &str| -> Vec<String> {
        let job = job(&workflow, &format!("bench-{platform}"));
        let file = dir.join("step.sh");
        fs::write(&file, job.script(name)).unwrap();
        let out = Command::new("bash")
            .args(["--noprofile", "--norc", "-eo", "pipefail"])
            .arg(&file)
            .current_dir(&dir)
            .env("PATH", &path)
            .env("STUB_DIR", &dir)
            .env("BENCH_HOME", &home)
            .env("GITHUB_OUTPUT", dir.join("output"))
            .env("RUNNER_TEMP", dir.join("runner-temp"))
            .env("DUSCOPE_BIN", "/usr/local/bin/duscope")
            .env("MODE", "both")
            .env("SCALE", "1")
            .output()
            .unwrap();
        succeeded(&out, name);
        let outputs = fs::read_to_string(dir.join("output")).unwrap();
        assert!(outputs.contains("status=0\n"), "{outputs}");
        assert!(
            outputs.contains(&format!("file={}\n", dir.join("result.json").display())),
            "{outputs}"
        );
        fs::remove_file(dir.join("output")).unwrap();
        fs::read_to_string(dir.join("args"))
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    };
    let after = |args: &[String], flag: &str| -> Option<String> {
        args.iter()
            .position(|a| a == flag)
            .map(|i| args[i + 1].clone())
    };
    let trees = home.join("trees").display().to_string();
    let volume = home.join("trees-volume");
    for platform in ["macos", "linux"] {
        let save = run(platform, "Main path measurements");
        assert_eq!(save[0], "save", "{save:?}");
        assert_eq!(after(&save, "--duscope").unwrap(), "/usr/local/bin/duscope");
        assert_eq!(after(&save, "--trees").unwrap(), "T1,T2,T3,T4");
        assert_eq!(after(&save, "--mode").unwrap(), "both");
        assert_eq!(after(&save, "--trees-dir").unwrap(), trees);
        assert_eq!(
            after(&save, "--results-dir").unwrap(),
            dir.join("runner-temp/save-results").display().to_string()
        );
        let rescan = run(platform, "Rescan measurements");
        assert_eq!(rescan[0], "rescan", "{rescan:?}");
        assert_eq!(after(&rescan, "--trees").unwrap(), "T1,T2");
        assert_eq!(after(&rescan, "--trees-dir").unwrap(), trees);
        assert_eq!(after(&rescan, "--cold-trees-dir"), None);
        if platform == "macos" {
            // Cold saves read and write on the image, as the harness's cold runs do.
            assert_eq!(
                after(&save, "--cold-trees-dir").unwrap(),
                volume.join("trees").display().to_string()
            );
            assert_eq!(
                after(&save, "--cold-cmd").unwrap(),
                format!(
                    "{}/bench/drop-caches.sh --remount {} {}",
                    dir.display(),
                    home.join("trees.sparsebundle").display(),
                    volume.display()
                )
            );
        } else {
            assert_eq!(after(&save, "--cold-trees-dir"), None);
        }
    }
}

/// The Rust string literal in `file` that starts with `prefix`, its line
/// continuations joined as the compiler joins them.
fn literal(file: &str, prefix: &str) -> String {
    let text = read(file);
    let at = text
        .find(&format!("\"{prefix}"))
        .unwrap_or_else(|| panic!("{file} has no literal starting \"{prefix}"));
    let mut out = String::new();
    let mut chars = text[at + 1..].chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => return out,
            '\\' => match chars.next() {
                Some('\n') => {
                    while chars.peek().is_some_and(|c| c.is_whitespace()) {
                        chars.next();
                    }
                }
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => break,
            },
            _ => out.push(c),
        }
    }
    panic!("{file}: the literal starting \"{prefix} does not end");
}

/// `format` as the binary fills it in: seconds for `{:.3}` and `{:.2}`, and
/// a number for anything else.
fn filled(format: &str) -> String {
    let mut out = String::new();
    let mut rest = format;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let close = rest[open..].find('}').unwrap() + open;
        let spec = &rest[open + 1..close];
        out.push_str(if spec.ends_with(":.3") {
            "0.123"
        } else if spec.ends_with(":.2") {
            "1.23"
        } else {
            "1234"
        });
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    out
}

/// P5-BENCH-04: the lines the harness reads the write and the rescan's
/// phases from are the binary's: each format string, taken from the source
/// and filled in, matches the pattern the harness reads it with; the debug
/// lines that bound the replay come from the module the rescans' log filter
/// names, at debug level.
#[test]
fn p5_bench_04_the_lines_the_harness_reads_are_the_binarys() {
    let fsevents = "crates/duscope-walk/src/platform/macos/fsevents.rs";
    let incremental = "crates/duscope-core/src/rescan/incremental.rs";
    let cases = [
        (
            "rescanbench",
            "REPLAY_START",
            fsevents,
            "cannot tell whether FSEvents logs ",
        ),
        ("rescanbench", "REPLAY_END", fsevents, "FSEvents replay of "),
        (
            "rescanbench",
            "PLAN_LINE",
            "duscope/src/verbs/rescan.rs",
            "incremental rescan: ",
        ),
        (
            "rescanbench",
            "INCREMENTAL_WALK",
            incremental,
            "incremental walk: ",
        ),
        (
            "rescanbench",
            "MERGE_LINE",
            incremental,
            "incremental merge: ",
        ),
        (
            "rescanbench",
            "POST_CHECK_LINE",
            incremental,
            "post-check: ",
        ),
        (
            "indexbench",
            "ROLLUP_LINE",
            "duscope/src/verbs/scan.rs",
            "roll-up: ",
        ),
        (
            "indexbench",
            "WRITE_LINE",
            "duscope/src/verbs/scan.rs",
            "write: ",
        ),
        (
            "indexbench",
            "WALK_LINE",
            "duscope/src/verbs/scan.rs",
            "walk: ",
        ),
        (
            "indexbench",
            "LISTING_LINE",
            "duscope/src/verbs/scan.rs",
            "listing: ",
        ),
        (
            "indexbench",
            "STAT_LINE",
            "duscope/src/verbs/scan.rs",
            "stat: ",
        ),
    ];
    let mut lines = String::new();
    for (module, pattern, file, prefix) in cases {
        let line = filled(&literal(file, prefix));
        assert!(!line.contains('\n'), "{line}");
        lines.push_str(&format!("{module}\t{pattern}\tduscope: {line}\n"));
    }
    let check = tempfile::NamedTempFile::new().unwrap();
    fs::write(check.path(), &lines).unwrap();
    let out = Command::new(python())
        .args(["-B", "-E", "-s", "-c"])
        .arg(
            "import sys\nsys.path.insert(0, 'bench')\nimport indexbench, rescanbench\n\
             bad = []\n\
             for line in open(sys.argv[1], encoding='utf-8'):\n\
             \x20   module, name, text = line.rstrip('\\n').split('\\t', 2)\n\
             \x20   if not getattr(sys.modules[module], name).search(text):\n\
             \x20       bad.append(f'{module}.{name}: {text}')\n\
             assert not bad, '\\n'.join(bad)\n",
        )
        .arg(check.path())
        .current_dir(root())
        .output()
        .unwrap();
    succeeded(&out, &format!("the harness's patterns against\n{lines}"));
    // The replay's bounds are debug lines of duscope_walk::platform::macos::fsevents,
    // the one target the rescans' filter takes at debug.
    let source = read(fsevents);
    for message in [
        "\"cannot tell whether FSEvents logs ",
        "\"FSEvents replay of ",
    ] {
        let at = source.find(message).unwrap();
        let before = &source[..at];
        let call = before.rfind("log::").unwrap();
        assert!(
            before[call..].starts_with("log::debug!("),
            "{message} is no debug line: {}",
            &before[call..]
        );
    }
    for (file, declares) in [
        ("crates/duscope-walk/src/lib.rs", "mod platform;"),
        ("crates/duscope-walk/src/platform/mod.rs", "mod macos;"),
        (
            "crates/duscope-walk/src/platform/macos/mod.rs",
            "mod fsevents;",
        ),
    ] {
        assert!(read(file).contains(declares), "{file} lacks `{declares}`");
    }
    assert!(
        read("bench/rescanbench.py")
            .contains("PHASE_LOG = \"info,duscope_walk::platform::macos::fsevents=debug\""),
        "the rescans' log filter names the module the replay's lines come from"
    );
}
