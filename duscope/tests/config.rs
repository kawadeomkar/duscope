//! P2-CORE-02: the configuration file, the environment, and what goes to
//! stderr (`-v`, `-vv`, `--debug`, `-q`).

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use duscope_testkit::{TestEnv, golden};

/// Writes `text` as the configuration file the environment's
/// `XDG_CONFIG_HOME` points at, and returns its path.
fn config(env: &TestEnv, text: &str) -> PathBuf {
    let dir = env.xdg_config_home().join("duscope");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.toml");
    fs::write(&path, text).unwrap();
    path
}

/// A small tree of its own: two files in `a/`, one in `b/`.
fn tree(env: &TestEnv) -> PathBuf {
    let root = env.work_dir().join("tree");
    fs::create_dir_all(root.join("a")).unwrap();
    fs::create_dir_all(root.join("b")).unwrap();
    fs::write(root.join("a/one.bin"), vec![1u8; 40_000]).unwrap();
    fs::write(root.join("a/two.txt"), b"two").unwrap();
    fs::write(root.join("b/three.bin"), vec![3u8; 9_000]).unwrap();
    root
}

fn run(env: &TestEnv, args: &[&str], vars: &[(&str, &str)]) -> Output {
    env.cmd()
        .args(args)
        .envs(vars.iter().copied())
        .output()
        .unwrap()
}

fn json(out: &Output) -> serde_json::Value {
    assert!(
        out.status.success(),
        "exit {:?}: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!("{e}: {}", String::from_utf8_lossy(&out.stdout));
    })
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn p2_core_02_the_precedence_matrix() {
    // Flag > environment > file > built-in, for a value each of the three
    // can set (--threads, DUSCOPE_THREADS, [defaults] threads), in every
    // combination; thread_count reports the winner.
    let env = TestEnv::new();
    let root = tree(&env);
    let root = root.to_str().unwrap();
    let builtin = json(&run(
        &env,
        &["top", root, "--json", "--config", "none"],
        &[],
    ));
    let builtin = builtin["scan"]["thread_count"].as_u64().unwrap();
    for flag in [false, true] {
        for var in [false, true] {
            for file in [false, true] {
                config(
                    &env,
                    if file {
                        "[defaults]\nthreads = 3\n"
                    } else {
                        ""
                    },
                );
                let mut args = vec!["top", root, "--json"];
                if flag {
                    args.extend(["--threads", "7"]);
                }
                let vars: &[(&str, &str)] = if var {
                    &[("DUSCOPE_THREADS", "5")]
                } else {
                    &[]
                };
                let expected = if flag {
                    7
                } else if var {
                    5
                } else if file {
                    3
                } else {
                    builtin
                };
                let got = json(&run(&env, &args, vars))["scan"]["thread_count"].clone();
                assert_eq!(got, expected, "flag {flag}, environment {var}, file {file}");
            }
        }
    }
    // The format: --plain > DUSCOPE_FORMAT > an agent's variable > the file
    // > plain off a terminal.
    config(&env, "[defaults]\nformat = \"json\"\nsize = \"apparent\"\n");
    let from_file = json(&run(&env, &["top", root], &[]));
    assert_eq!(from_file["scan"]["size_kind"], "apparent");
    let out = run(&env, &["top", root], &[("DUSCOPE_FORMAT", "plain")]);
    assert!(serde_json::from_slice::<serde_json::Value>(&out.stdout).is_err());
    let out = run(
        &env,
        &["top", root, "--plain"],
        &[("DUSCOPE_FORMAT", "json")],
    );
    assert!(serde_json::from_slice::<serde_json::Value>(&out.stdout).is_err());
    config(&env, "[defaults]\nformat = \"plain\"\n");
    json(&run(&env, &["top", root], &[("CLAUDECODE", "1")]));
    // --size and --apparent-size beat the file's size.
    config(&env, "[defaults]\nsize = \"apparent\"\n");
    let flag = json(&run(
        &env,
        &["top", root, "--json", "--size", "allocated"],
        &[],
    ));
    assert_eq!(flag["scan"]["size_kind"], "allocated");
}

#[test]
fn p2_core_02_a_duscope_toml_inside_the_tree_is_ignored() {
    let env = TestEnv::new();
    let root = tree(&env);
    fs::write(
        root.join(".duscope.toml"),
        "[defaults]\nsize = \"apparent\"\nlimit = 1\n\n[excludes]\npatterns = [\"a\"]\n",
    )
    .unwrap();
    fs::write(
        root.join("a/.duscope.toml"),
        "[defaults]\nsize = \"apparent\"\n",
    )
    .unwrap();
    let envelope = json(&run(&env, &["top", root.to_str().unwrap(), "--json"], &[]));
    assert_eq!(envelope["scan"]["size_kind"], "allocated");
    assert_eq!(envelope["scan"]["excluded_count"], 0);
    assert!(envelope["entries"].as_array().unwrap().len() > 1);
}

#[test]
fn p2_core_02_debug_output_shape() {
    let env = TestEnv::new();
    let fixtures = env.fixtures();
    if !fixtures.unreadable_effective() {
        duscope_testkit::skip("running as root, the mode-000 directory is readable");
        return;
    }
    let root = fixtures.case_root("unreadable");
    let out = run(
        &env,
        &["scan", root.to_str().unwrap(), "--debug", "--json"],
        &[],
    );
    json(&out);
    let real = fs::canonicalize(&root).unwrap();
    let shape: String = stderr(&out)
        .lines()
        .filter(|line| line.starts_with("duscope: "))
        .map(|line| shape(line, &real, &root) + "\n")
        .collect();
    golden("scan_stderr_debug.snap", &shape);
}

/// `line` with the root as `<root>`, the volume's mount point and type as
/// `<mount>` and `<type>`, and every number as `N`.
fn shape(line: &str, real: &Path, given: &Path) -> String {
    let mut line = line
        .replace(real.to_str().unwrap(), "<root>")
        .replace(given.to_str().unwrap(), "<root>");
    if let Some(rest) = line.strip_prefix("duscope: volume ")
        && let Some((_, after)) = rest.split_once(": ")
        && let Some((_, after)) = after.split_once(", ")
    {
        line = format!("duscope: volume <mount>: <type>, {after}");
    }
    let mut shaped = String::new();
    let mut in_number = false;
    for c in line.chars() {
        let digit = c.is_ascii_digit() || (in_number && matches!(c, ',' | '.'));
        if digit && !in_number {
            shaped.push('N');
        } else if !digit {
            shaped.push(c);
        }
        in_number = digit;
    }
    shaped
}

#[test]
fn p2_core_02_v_prints_phase_timings_and_stdout_stays_the_envelope() {
    let env = TestEnv::new();
    let root = tree(&env);
    let root = root.to_str().unwrap();
    let out = run(&env, &["scan", root, "-v", "--json"], &[]);
    json(&out);
    let err = stderr(&out);
    assert!(
        err.contains("duscope: walk: 6 entries, 3 directories"),
        "{err}"
    );
    assert!(err.contains("duscope: roll-up: "), "{err}");
    assert!(
        !err.contains("duscope: workers: "),
        "-v leaves -vv's figures out: {err}"
    );
    let out = run(&env, &["scan", root, "-vv", "--json"], &[]);
    json(&out);
    let err = stderr(&out);
    assert!(err.contains("duscope: workers: "), "{err}");
    assert!(err.contains("duscope: volume "), "{err}");
    // The environment's filter, when no flag is given.
    let out = run(&env, &["scan", root, "--json"], &[("DUSCOPE_LOG", "info")]);
    assert!(stderr(&out).contains("duscope: walk: "));
    let out = run(&env, &["scan", root, "--json"], &[("RUST_LOG", "info")]);
    assert!(stderr(&out).contains("duscope: walk: "));
    let out = run(&env, &["scan", root, "--json"], &[]);
    assert!(!stderr(&out).contains("duscope: walk: "));
}

#[test]
fn p2_core_02_q_keeps_warnings_off_stderr() {
    let env = TestEnv::new();
    let fixtures = env.fixtures();
    if !fixtures.unreadable_effective() {
        duscope_testkit::skip("running as root, the mode-000 directory is readable");
        return;
    }
    let root = fixtures.case_root("unreadable");
    let root = root.to_str().unwrap();
    let out = run(&env, &["scan", root, "--plain"], &[]);
    assert!(stderr(&out).contains("warning: "), "{}", stderr(&out));
    let out = run(&env, &["scan", root, "--plain", "-q"], &[]);
    assert!(out.status.success());
    assert!(!stderr(&out).contains("warning: "), "{}", stderr(&out));
    // The envelope keeps them.
    let envelope = json(&run(&env, &["scan", root, "--json", "-q"], &[]));
    assert_eq!(envelope["warnings"][0]["code"], "partial_read");
}

#[test]
fn p2_core_02_a_bad_file_is_a_usage_error_naming_its_line() {
    let env = TestEnv::new();
    let root = tree(&env);
    let root = root.to_str().unwrap();
    let path = config(&env, "[defaults]\nthreads = 2\nsize = \"huge\"\n");
    let out = run(&env, &["top", root, "--json"], &[]);
    assert_eq!(out.status.code(), Some(2));
    let envelope: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(envelope["error"]["code"], "usage");
    let message = envelope["error"]["message"].as_str().unwrap();
    assert!(
        message.starts_with(&format!("{}:3: [defaults] size: ", path.display())),
        "{message}"
    );
    config(&env, "[defaults]\ncolour = \"age\"\n");
    let out = run(&env, &["top", root, "--json"], &[]);
    assert_eq!(out.status.code(), Some(2));
    // --config none and DUSCOPE_CONFIG=none read no file.
    json(&run(
        &env,
        &["top", root, "--json", "--config", "none"],
        &[],
    ));
    json(&run(
        &env,
        &["top", root, "--json"],
        &[("DUSCOPE_CONFIG", "none")],
    ));
    // A file the user names must be there.
    let out = run(
        &env,
        &[
            "top",
            root,
            "--json",
            "--config",
            "/nonexistent/duscope.toml",
        ],
        &[],
    );
    assert_eq!(out.status.code(), Some(2));
    let other = env.work_dir().join("other.toml");
    fs::write(&other, "[defaults]\nsize = \"apparent\"\n").unwrap();
    let named = json(&run(
        &env,
        &["top", root, "--json"],
        &[("DUSCOPE_CONFIG", other.to_str().unwrap())],
    ));
    assert_eq!(named["scan"]["size_kind"], "apparent");
}

#[test]
fn p2_core_02_excludes_and_extra_tables_apply_to_every_scan() {
    let env = TestEnv::new();
    let root = tree(&env);
    let root = root.to_str().unwrap();
    let dir = env.xdg_config_home().join("duscope");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("classes.toml"), "[ext]\nbin = \"media\"\n").unwrap();
    config(
        &env,
        "[excludes]\npatterns = [\"b\"]\n\n[classes]\nextra = \"classes.toml\"\n",
    );
    let top = json(&run(&env, &["top", root, "--json"], &[]));
    assert_eq!(top["scan"]["excluded_count"], 1);
    let by_type = json(&run(&env, &["by-type", root, "--json"], &[]));
    let media = by_type["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["class"] == "media")
        .unwrap_or_else(|| panic!("no media row: {by_type}"));
    assert_eq!(media["file_count"], 1);
}
