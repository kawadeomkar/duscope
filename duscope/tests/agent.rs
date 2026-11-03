//! P3-AGT-01: `duscope agent` through the binary: the briefing's golden, every
//! SQL example in it run against a saved scan of the fixture tree, and the
//! words its first 4,096 characters (the MCP server's `instructions`) must
//! carry.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::process::Output;

use duscope_testkit::{TestEnv, golden};
use serde_json::Value;

/// What the MCP server's `instructions` hold of the briefing.
const INSTRUCTIONS_CHARS: usize = 4096;

fn run(env: &TestEnv, args: &[&str]) -> Output {
    env.cmd().args(args).output().unwrap()
}

fn briefing(env: &TestEnv) -> String {
    let out = run(env, &["agent"]);
    assert!(
        out.status.success(),
        "exit {:?}: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// The statements of the briefing's fenced `sql` blocks.
fn sql_examples(text: &str) -> Vec<String> {
    let mut examples = Vec::new();
    let mut block: Option<String> = None;
    for line in text.lines() {
        match (&mut block, line) {
            (None, "```sql") => block = Some(String::new()),
            (Some(sql), "```") => {
                examples.push(sql.trim().to_owned());
                block = None;
            }
            (Some(sql), line) => {
                sql.push_str(line);
                sql.push('\n');
            }
            (None, _) => {}
        }
    }
    assert!(block.is_none(), "an unclosed ```sql block");
    examples
}

#[test]
fn p3_agt_01_the_briefing_matches_its_golden() {
    let env = TestEnv::new();
    golden("agent_briefing.snap", &briefing(&env));
}

#[test]
fn p3_agt_01_every_sql_example_runs_on_the_fixture_tree() {
    let env = TestEnv::new();
    let examples = sql_examples(&briefing(&env));
    assert!(
        (5..=10).contains(&examples.len()),
        "{} SQL examples",
        examples.len()
    );
    let home = std::fs::canonicalize(env.fixtures().path("home")).unwrap();
    let home = home.to_str().unwrap();
    let saved = run(&env, &["scan", home, "--save", "--json"]);
    assert!(
        saved.status.success(),
        "{}",
        String::from_utf8_lossy(&saved.stderr)
    );
    for sql in &examples {
        let out = run(&env, &["query", home, "--sql", sql, "--json"]);
        let envelope: Value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("{sql}: {e}: {}", String::from_utf8_lossy(&out.stderr)));
        assert_eq!(envelope["ok"], true, "{sql}: {envelope}");
        assert!(out.status.success(), "{sql}");
        assert!(
            envelope["result"]["columns"].is_array(),
            "{sql}: {envelope}"
        );
    }
}

#[test]
fn p3_agt_01_the_first_4096_characters_carry_the_trigger_vocabulary() {
    let env = TestEnv::new();
    let text = briefing(&env);
    let instructions: String = text.chars().take(INSTRUCTIONS_CHARS).collect();
    // The words that should bring duscope to mind (the skill's trigger
    // keywords), the invariants `--help` cannot state, and the handle flow.
    for needed in [
        "disk space",
        "what is eating my disk",
        "large files",
        "node_modules",
        "stale",
        "clean up",
        "compare scans",
        "Sizes are bytes",
        "size_kind",
        "dry run unless `--yes`",
        "Never pass\n  `--yes` unless a person has told you",
        "confirmation_required",
        "duscope explain PATH --json",
        "result.handle",
        "duscope clean --from HANDLE --json",
    ] {
        assert!(
            instructions.contains(needed),
            "the first {INSTRUCTIONS_CHARS} characters lack {needed:?}"
        );
    }
}
