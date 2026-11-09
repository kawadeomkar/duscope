//! P3-MCP-06: the client smoke scripts under `tests/smoke/`, offline. Each
//! registers `duscope mcp` in its client and makes three tool calls through
//! it, which only CI's `mcp-smoke` job runs, with the vendors' keys; here
//! each runs its dry run, which installs and runs no client and calls no
//! vendor, but plays the client against the binary and checks its log the
//! way a real run is checked; and each, without its key, stops before it
//! installs anything.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use duscope_testkit::{TestEnv, duscope_bin};

/// Each script, the client it names and the key it reads.
const SCRIPTS: [(&str, &str, &str); 3] = [
    ("claude-code", "Claude Code", "ANTHROPIC_API_KEY"),
    ("codex", "Codex CLI", "OPENAI_API_KEY"),
    ("gemini", "Gemini CLI", "GEMINI_API_KEY"),
];

fn script(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/smoke/{name}.sh"))
}

/// A directory of stubs that stand in for every program a script could
/// reach the network or a vendor with, each failing and leaving a mark.
fn stubs(env: &TestEnv) -> PathBuf {
    let bin = env.root().join("stubs");
    fs::create_dir_all(&bin).unwrap();
    for program in ["npm", "npx", "claude", "codex", "gemini", "curl"] {
        let path = bin.join(program);
        let mark = env.root().join(format!("ran-{program}"));
        fs::write(
            &path,
            format!("#!/bin/sh\ntouch '{}'\nexit 97\n", mark.display()),
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    bin
}

/// No stub ran.
fn assert_nothing_ran(env: &TestEnv) {
    let ran: Vec<String> = fs::read_dir(env.root())
        .unwrap()
        .filter_map(|entry| entry.unwrap().file_name().into_string().ok())
        .filter(|name| name.starts_with("ran-"))
        .collect();
    assert!(ran.is_empty(), "{ran:?}");
}

/// PATH with the stubs first.
fn path(stubs: &Path) -> std::ffi::OsString {
    let mut dirs = vec![stubs.to_owned()];
    dirs.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    std::env::join_paths(dirs).unwrap()
}

#[test]
fn p3_mcp_06_each_smoke_script_passes_its_dry_run() {
    let env = TestEnv::new();
    let stubs = stubs(&env);
    let logs = env.work_dir();
    for (name, client, _) in SCRIPTS {
        let out = env
            .command("bash")
            .arg(script(name))
            .arg("--dry-run")
            .arg("--duscope")
            .arg(duscope_bin())
            .arg("--log-dir")
            .arg(&logs)
            .env("PATH", path(&stubs))
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        let log = fs::read_to_string(logs.join(format!("{name}.dry-run.log")))
            .unwrap_or_else(|e| panic!("{name}: {e}: {stderr}"));
        assert!(out.status.success(), "{name}: {stderr}\n{log}");
        assert!(
            log.starts_with(&format!("# duscope MCP smoke test: {client}\n")),
            "{log}"
        );
        assert!(
            log.contains("DRY RUN: no client was installed or run"),
            "{log}"
        );
        assert!(
            log.contains(&format!("\nPASS: {client} started duscope mcp")),
            "{log}"
        );
        // The registration names the binary as the documentation does.
        let registered = log
            .lines()
            .find(|line| line.contains(" mcp add "))
            .unwrap_or_else(|| panic!("{log}"));
        assert!(registered.contains(" duscope mcp "), "{registered}");
        // The binary was started with the registered command line, and the
        // three calls ran their verbs.
        assert!(log.contains("\nargv: mcp --roots "), "{log}");
        for verb in ["scan", "top", "summary"] {
            assert!(log.contains(&format!("running {verb} for a tool")), "{log}");
        }
        // The real log's name stays free for a real run.
        assert!(!logs.join(format!("{name}.log")).exists());
    }
    assert_nothing_ran(&env);
}

#[test]
fn p3_mcp_06_a_smoke_script_without_its_key_stops_before_installing_anything() {
    let env = TestEnv::new();
    let stubs = stubs(&env);
    let logs = env.work_dir();
    for (name, client, key) in SCRIPTS {
        // The test environment holds no API key.
        let out = env
            .command("bash")
            .arg(script(name))
            .arg("--duscope")
            .arg(duscope_bin())
            .arg("--log-dir")
            .arg(&logs)
            .env("PATH", path(&stubs))
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{name}: {stderr}");
        assert!(stderr.contains(&format!("{key} is not set")), "{stderr}");
        assert!(stderr.contains(client), "{stderr}");
        let log = fs::read_to_string(logs.join(format!("{name}.log"))).unwrap();
        assert!(log.contains(&format!("\nFAIL: {key} is not set")), "{log}");
        assert!(!log.contains("## Install"), "{log}");
    }
    assert_nothing_ran(&env);
}

#[test]
fn p3_mcp_06_a_smoke_script_keeps_its_key_out_of_its_log() {
    let env = TestEnv::new();
    let stubs = stubs(&env);
    let logs = env.work_dir();
    // npm is a stub that fails after printing what it was given and the
    // key, as a client that echoed its environment would.
    let secret = "sk-not-a-real-key-0123456789";
    fs::write(
        stubs.join("npm"),
        "#!/bin/sh\necho \"npm $* $ANTHROPIC_API_KEY\"\nexit 97\n",
    )
    .unwrap();
    let out = env
        .command("bash")
        .arg(script("claude-code"))
        .arg("--duscope")
        .arg(duscope_bin())
        .arg("--log-dir")
        .arg(&logs)
        .env("PATH", path(&stubs))
        .env("ANTHROPIC_API_KEY", secret)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let log = fs::read_to_string(logs.join("claude-code.log")).unwrap();
    assert!(log.contains("[ANTHROPIC_API_KEY]"), "{log}");
    assert!(!log.contains(secret), "{log}");
    assert!(log.contains("\nFAIL: npm could not install"), "{log}");
}
