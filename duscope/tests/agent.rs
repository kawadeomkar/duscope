//! P3-AGT-01: `duscope agent` through the binary: the briefing's golden, every
//! SQL example in it run against a saved scan of the fixture tree, and the
//! words its first 4,096 characters (the MCP server's `instructions`) must
//! carry. P3-AGT-02: `duscope agent --skill`: the skill's golden, both
//! folders written, the client's limits, and a skill that is there kept
//! unless `--force`.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::path::Path;
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

/// The folders `--skill` writes to, under the base directory.
const SKILL_DIRS: [&str; 2] = [".agents/skills/duscope", ".claude/skills/duscope"];

/// What Claude Code lists of a skill's `description` and `when_to_use`.
const LISTED_CHARS: usize = 1536;

/// `duscope agent --skill ARGS` run in `cwd`.
fn skill(env: &TestEnv, cwd: &Path, args: &[&str]) -> Output {
    env.cmd()
        .current_dir(cwd)
        .args(["agent", "--skill"])
        .args(args)
        .output()
        .unwrap()
}

/// `SKILL.md`'s frontmatter fields, as `key: "value"` lines, and its body.
fn frontmatter(text: &str) -> (Vec<(String, String)>, String) {
    let rest = text
        .strip_prefix("---\n")
        .expect("the frontmatter opens the file");
    let (head, body) = rest.split_once("\n---\n").expect("the frontmatter closes");
    let fields = head
        .lines()
        .map(|line| {
            let (key, value) = line.split_once(": ").unwrap();
            let value: String = if value.starts_with('"') {
                serde_json::from_str(value).unwrap()
            } else {
                value.to_owned()
            };
            (key.to_owned(), value)
        })
        .collect();
    (fields, body.to_owned())
}

#[test]
fn p3_agt_02_the_skill_matches_its_golden() {
    let env = TestEnv::new();
    let work = env.work_dir();
    let out = skill(&env, &work, &[]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let dir = work.join(SKILL_DIRS[1]);
    golden(
        "agent_skill.snap",
        &std::fs::read_to_string(dir.join("SKILL.md")).unwrap(),
    );
    golden(
        "agent_skill_sql.snap",
        &std::fs::read_to_string(dir.join("references/sql.md")).unwrap(),
    );
}

#[test]
fn p3_agt_02_both_paths_are_written() {
    let env = TestEnv::new();
    let work = env.work_dir();
    let out = skill(&env, &work, &[]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let listed = String::from_utf8(out.stdout).unwrap();
    let mut expected = Vec::new();
    for dir in SKILL_DIRS {
        for file in ["SKILL.md", "references/sql.md"] {
            expected.push(work.join(dir).join(file).display().to_string());
        }
    }
    assert_eq!(listed.lines().collect::<Vec<_>>(), expected);
    for file in ["SKILL.md", "references/sql.md"] {
        let agents = std::fs::read(work.join(SKILL_DIRS[0]).join(file)).unwrap();
        let claude = std::fs::read(work.join(SKILL_DIRS[1]).join(file)).unwrap();
        assert_eq!(agents, claude, "{file}");
    }
    // --dir puts them elsewhere, and nothing goes to the working directory.
    let other = work.join("project");
    std::fs::create_dir_all(&other).unwrap();
    let elsewhere = work.join("elsewhere");
    let out = skill(&env, &other, &["--dir", elsewhere.to_str().unwrap()]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(elsewhere.join(SKILL_DIRS[0]).join("SKILL.md").is_file());
    assert!(elsewhere.join(SKILL_DIRS[1]).join("SKILL.md").is_file());
    assert!(!other.join(".claude").exists());
}

#[test]
fn p3_agt_02_a_skill_that_is_there_is_kept_without_force() {
    let env = TestEnv::new();
    let work = env.work_dir();
    let mine = work.join(SKILL_DIRS[1]).join("SKILL.md");
    std::fs::create_dir_all(mine.parent().unwrap()).unwrap();
    std::fs::write(&mine, "my own skill").unwrap();
    let out = skill(&env, &work, &[]);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("already there"), "{stderr}");
    assert!(stderr.contains("--force"), "{stderr}");
    assert_eq!(std::fs::read_to_string(&mine).unwrap(), "my own skill");
    // Nothing was written, the other folder included.
    assert!(!work.join(SKILL_DIRS[0]).exists());
    let out = skill(&env, &work, &["--force"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        std::fs::read_to_string(&mine)
            .unwrap()
            .starts_with("---\nname: duscope\n")
    );
    // --dir and --force belong to --skill.
    for args in [&["agent", "--force"][..], &["agent", "--dir", "x"][..]] {
        assert_eq!(run(&env, args).status.code(), Some(2), "{args:?}");
    }
}

#[test]
fn p3_agt_02_the_skill_fits_the_clients_limits() {
    let env = TestEnv::new();
    let work = env.work_dir();
    assert!(skill(&env, &work, &[]).status.success());
    let text = std::fs::read_to_string(work.join(SKILL_DIRS[1]).join("SKILL.md")).unwrap();
    assert!(text.lines().count() < 500, "{} lines", text.lines().count());
    let (fields, body) = frontmatter(&text);
    let field = |name: &str| {
        fields
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
            .unwrap_or_else(|| panic!("no {name} in the frontmatter"))
    };
    assert_eq!(field("name"), "duscope");
    let listed = format!("{} {}", field("description"), field("when_to_use"));
    assert!(
        listed.chars().count() <= LISTED_CHARS,
        "{} characters",
        listed.chars().count()
    );
    for trigger in [
        "disk space",
        "what is eating my disk",
        "large files",
        "node_modules",
        "stale",
        "clean up",
        "compare scans",
    ] {
        assert!(listed.contains(trigger), "the listing lacks {trigger:?}");
    }
    // The invariants travel with it; the SQL is in references/.
    for needed in [
        "Sizes are bytes",
        "dry run unless `--yes`",
        "references/sql.md",
    ] {
        assert!(body.contains(needed), "SKILL.md lacks {needed:?}");
    }
    assert!(
        !body.contains("```sql"),
        "the SQL belongs in references/sql.md"
    );
    let references =
        std::fs::read_to_string(work.join(SKILL_DIRS[1]).join("references/sql.md")).unwrap();
    assert_eq!(
        sql_examples(&references),
        sql_examples(&briefing(&env)),
        "references/sql.md holds the briefing's SQL, which p3_agt_01 runs"
    );
}
