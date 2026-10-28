//! P1-CLI-02: on a terminal, people get a table and an agent gets JSON.

use std::time::Duration;

use duscope_testkit::{TestEnv, duscope_bin};
use termlens::Terminal;

/// What the terminal shows when `top` runs on the empty tree in a
/// pseudo-terminal, with `extra` added to the test environment.
fn on_a_terminal(env: &TestEnv, extra: &[(&str, &str)]) -> String {
    let tree = env.fixtures().case_root("empty");
    let mut terminal = Terminal::builder()
        .size(1000, 10)
        .timeout(Duration::from_secs(60))
        .env_clear()
        .envs(env.vars())
        .envs(extra.iter().copied())
        .current_dir(env.work_dir())
        .arg("top")
        .arg(&tree)
        .spawn(duscope_bin())
        .unwrap();
    let status = terminal.wait_exit().unwrap();
    assert!(status.success(), "{}", terminal.screen().full_text());
    terminal.screen().full_text()
}

#[test]
fn p1_cli_02_claudecode_yields_json_on_a_pty() {
    let env = TestEnv::new();
    // On a terminal, people get a table.
    let shown = on_a_terminal(&env, &[]);
    assert!(shown.trim_start().starts_with("Size  "), "{shown}");
    // With CLAUDECODE=1 the same terminal gets the envelope, which the
    // terminal wraps at 1,000 columns; the footer follows it on stderr.
    let shown = on_a_terminal(&env, &[("CLAUDECODE", "1")]);
    let lines: Vec<&str> = shown.lines().filter(|l| !l.is_empty()).collect();
    let (footer, json) = lines.split_last().unwrap();
    assert!(footer.contains(" allocated · "), "{shown}");
    let envelope: serde_json::Value = serde_json::from_str(&json.concat()).unwrap();
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["query"]["command"], "top");
}
