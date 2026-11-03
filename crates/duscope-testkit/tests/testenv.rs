//! P0-TEST-01: the test environment pins what output may depend on and
//! nothing of the caller's environment leaks into the commands it builds.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use duscope_testkit::TestEnv;

/// The variables a command sees, besides the optional pass-throughs.
const PINNED: &[&str] = &[
    "COLUMNS",
    "DUSCOPE_INDEX_DIR",
    "DUSCOPE_TEST_NOW",
    "DUSCOPE_TEST_TRASH",
    "HOME",
    "LC_ALL",
    "LINES",
    "PATH",
    "TMPDIR",
    "TZ",
    "XDG_CACHE_HOME",
    "XDG_CONFIG_DIRS",
    "XDG_CONFIG_HOME",
    "XDG_DATA_DIRS",
    "XDG_DATA_HOME",
    "XDG_RUNTIME_DIR",
    "XDG_STATE_HOME",
];

const PASS_THROUGH: &[&str] = &["LLVM_PROFILE_FILE", "RUST_BACKTRACE", "RUST_LIB_BACKTRACE"];

/// Variables that would change duscope's output and must never leak in.
const NEVER: &[&str] = &[
    "NO_COLOR",
    "CLICOLOR",
    "CLICOLOR_FORCE",
    "FORCE_COLOR",
    "COLORTERM",
    "COLORFGBG",
    "TERM",
    "TERM_PROGRAM",
    "LS_COLORS",
    "LANG",
    "LC_CTYPE",
    "CI",
    "CLAUDECODE",
    "CODEX_THREAD_ID",
    "CODEX_VERSION",
    "GEMINI_CLI",
    "DUSCOPE_FORMAT",
    "DUSCOPE_CONFIG",
    "DUSCOPE_THEME",
    "DUSCOPE_THREADS",
    "DUSCOPE_LOG",
    "RUST_LOG",
];

/// The environment `env(1)` reports when run under the test environment.
fn seen(env: &TestEnv) -> BTreeMap<String, String> {
    let out = env.command("env").output().unwrap();
    assert!(out.status.success());
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect()
}

#[test]
fn p0_test_01_testenv_pins_the_environment() {
    let env = TestEnv::new();
    let vars = seen(&env);
    let names: Vec<&str> = vars
        .keys()
        .map(String::as_str)
        .filter(|k| !PASS_THROUGH.contains(k))
        .collect();
    assert_eq!(names, PINNED, "exactly the pinned variables");
    for name in NEVER {
        assert!(
            !vars.contains_key(*name),
            "{name} leaked into the test environment"
        );
    }
    assert_eq!(vars["TZ"], "UTC");
    assert_eq!(vars["LC_ALL"], "C.UTF-8");
    assert_eq!(vars["COLUMNS"], "80");
    assert_eq!(vars["LINES"], "24");
    assert_eq!(vars["DUSCOPE_TEST_NOW"], "2037-01-01T00:00:00Z");
    assert_eq!(vars["DUSCOPE_TEST_NOW"], env.test_now());

    let real_home = std::env::var_os("HOME").map(PathBuf::from);
    for name in PINNED
        .iter()
        .filter(|n| ["HOME", "TMPDIR", "DUSCOPE_INDEX_DIR"].contains(n) || n.starts_with("XDG_"))
    {
        let path = PathBuf::from(&vars[*name]);
        assert!(
            path.starts_with(env.root()),
            "{name}={} is outside the test's directory",
            path.display()
        );
        assert!(path.is_dir(), "{name} does not exist");
        assert_ne!(
            Some(&path),
            real_home.as_ref(),
            "{name} is the real home directory"
        );
    }
    let runtime = fs::metadata(env.xdg_runtime_dir())
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(runtime, 0o700);

    let pwd = env.command("pwd").output().unwrap();
    assert_eq!(
        String::from_utf8(pwd.stdout).unwrap().trim_end(),
        env.work_dir().to_str().unwrap()
    );
}

#[test]
fn p0_test_01_cmd_runs_the_duscope_binary_under_the_environment() {
    let env = TestEnv::new();
    let cmd = env.cmd();
    let program = PathBuf::from(cmd.get_program());
    assert_eq!(program, duscope_testkit::duscope_bin());
    assert_eq!(program.file_name().unwrap(), "duscope");
    let exe = std::env::current_exe().unwrap();
    assert_eq!(
        program.parent(),
        exe.parent().unwrap().parent(),
        "beside deps/"
    );
    let envs: Vec<(OsString, OsString)> = cmd
        .get_envs()
        .map(|(k, v)| (k.to_owned(), v.unwrap().to_owned()))
        .collect();
    let mut expected = env.vars();
    expected.sort();
    let mut got = envs;
    got.sort();
    assert_eq!(got, expected);
    assert_eq!(cmd.get_current_dir(), Some(env.work_dir().as_path()));
}

#[test]
fn p0_test_01_testenv_removes_its_directory() {
    let env = TestEnv::new();
    let root = env.root().to_owned();
    // A test may leave an unreadable directory behind; clean-up restores
    // permissions before removing.
    let locked = env.home().join("locked");
    fs::create_dir_all(locked.join("inner")).unwrap();
    fs::write(locked.join("inner/file"), b"x").unwrap();
    fs::set_permissions(locked.join("inner"), fs::Permissions::from_mode(0o000)).unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    drop(env);
    assert!(
        !root.exists(),
        "{} survived the test environment",
        root.display()
    );
}

#[test]
fn p0_test_01_test_now_is_the_manifest_instant() {
    let plan = mktree::Plan::embedded().unwrap();
    assert_eq!(duscope_testkit::test_now(), plan.instant_text());
    assert_eq!(duscope_testkit::test_now(), "2037-01-01T00:00:00Z");
}
