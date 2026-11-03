//! `agent`: the briefing an AI agent reads before it uses duscope, as
//! Markdown on stdout, or with `--skill` as a skill an agent's client loads
//! by itself (P3-AGT-02). The text is `briefing.md.tmpl`; when it runs, the
//! binary fills in its own verbs from the command line, its exit codes, and
//! the envelope's error codes (the conventions' `ERROR_CODES`) with the exit
//! code of each, so those lists cannot drift from the code. The MCP server's `instructions` are the briefing's first
//! 4,096 characters (P3-MCP-03), which is why the template opens with the
//! words that should bring duscope to mind, the invariants and the handle
//! flow.
//!
//! The skill is `skill.md.tmpl` around the same briefing: a frontmatter
//! whose `description` and `when_to_use` carry the words that should bring
//! duscope to mind (the client lists at most 1,536 characters of the two),
//! the briefing up to its SQL, and a pointer to `references/sql.md`, which
//! holds the briefing's SQL section. `--skill` writes both files to
//! `.agents/skills/duscope/` and `.claude/skills/duscope/`, the two folders
//! agents' clients read skills from, under the working directory or
//! `--dir`, and replaces a skill that is there only under `--force`.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use clap::CommandFactory;
use duscope_core::envelope::{ERROR_CODES, SCHEMA_VERSION};

use crate::cli::{AgentArgs, Cli, visible_verbs};
use crate::out::emit_bytes;
use crate::verbs::{Exit, Failure};

/// The briefing, with `{{verbs}}`, `{{exit_codes}}`, `{{error_codes}}` and
/// `{{schema_version}}` to fill in.
const TEMPLATE: &str = include_str!("briefing.md.tmpl");

/// The skill, with `{{briefing}}` (the briefing up to its SQL) and
/// `{{sql_count}}` to fill in.
const SKILL_TEMPLATE: &str = include_str!("skill.md.tmpl");

/// The heading of the briefing's SQL section, which the skill keeps in
/// `references/sql.md`.
const SQL_HEADING: &str = "## SQL over a saved scan";

/// The folders the skill goes to, under the working directory or `--dir`:
/// where agents' clients look for a project's skills.
pub const SKILL_DIRS: [&str; 2] = [".agents/skills/duscope", ".claude/skills/duscope"];

/// Every exit code, in the order the briefing lists them.
const EXITS: [Exit; 7] = [
    Exit::Success,
    Exit::Runtime,
    Exit::Usage,
    Exit::NotFound,
    Exit::Permission,
    Exit::Index,
    Exit::Interrupted,
];

/// Prints the briefing, or writes the skill.
pub fn run(args: &AgentArgs) -> Result<(), Failure> {
    if !args.skill {
        return emit_bytes(briefing().as_bytes());
    }
    let base = match &args.dir {
        Some(dir) => dir.clone(),
        None => std::env::current_dir().map_err(|e| {
            Failure::new(
                Exit::Runtime,
                "io_error",
                format!("cannot read the working directory: {e}"),
            )
        })?,
    };
    let written = write_skill(&base, args.force)?;
    let mut listing = String::new();
    for path in written {
        let _ = writeln!(listing, "{}", path.display());
    }
    emit_bytes(listing.as_bytes())
}

/// The skill's files, by their path in a skill folder: `SKILL.md` and
/// `references/sql.md`.
pub fn skill() -> [(&'static str, String); 2] {
    let briefing = briefing();
    let (body, sql) = briefing
        .split_once(SQL_HEADING)
        .unwrap_or((briefing.as_str(), ""));
    let skill = SKILL_TEMPLATE
        .replace("{{briefing}}", body.trim_end())
        .replace("{{sql_count}}", &sql.matches("```sql").count().to_string());
    let references = format!("# SQL over a saved scan{sql}");
    [("SKILL.md", skill), ("references/sql.md", references)]
}

/// Writes the skill to both folders under `base`: the files written. A
/// skill that is there is replaced only when `force`; otherwise nothing is
/// written. Each file goes to a temporary name beside it first and is
/// renamed over the old one, so a reader never sees half a file.
fn write_skill(base: &Path, force: bool) -> Result<Vec<PathBuf>, Failure> {
    let files = skill();
    let targets: Vec<(PathBuf, &str)> = SKILL_DIRS
        .iter()
        .flat_map(|dir| {
            files
                .iter()
                .map(move |(name, text)| (base.join(dir).join(name), text.as_str()))
        })
        .collect();
    if !force && let Some((there, _)) = targets.iter().find(|(path, _)| path.exists()) {
        return Err(Failure::new(
            Exit::Usage,
            "usage",
            format!("{} is already there", there.display()),
        )
        .hint("Run duscope agent --skill --force to replace the skill"));
    }
    let failed = |path: &Path, e: std::io::Error| {
        Failure::new(
            Exit::Runtime,
            "io_error",
            format!("cannot write {}: {e}", path.display()),
        )
    };
    for (path, text) in &targets {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| failed(parent, e))?;
        }
        let mut temporary = path.clone().into_os_string();
        temporary.push(format!(".tmp-{}", std::process::id()));
        let temporary = PathBuf::from(temporary);
        fs::write(&temporary, text)
            .and_then(|()| fs::rename(&temporary, path))
            .map_err(|e| {
                let _ = fs::remove_file(&temporary);
                failed(path, e)
            })?;
    }
    Ok(targets.into_iter().map(|(path, _)| path).collect())
}

/// The briefing, its placeholders filled in.
pub fn briefing() -> String {
    TEMPLATE
        .replace("{{verbs}}", &verbs())
        .replace("{{exit_codes}}", &exit_codes())
        .replace("{{error_codes}}", &error_codes())
        .replace("{{schema_version}}", SCHEMA_VERSION)
}

/// What an exit code means to an agent deciding what to do next.
fn meaning(exit: Exit) -> &'static str {
    match exit {
        Exit::Success => "success, partial reads included (`partial_read` in `warnings`)",
        Exit::Runtime => "a runtime error: read `error.code` and `error.hint`",
        Exit::Usage => "a usage error, an unknown flag or a bad value: fix the command",
        Exit::NotFound => "the path, or the saved scan asked for, does not exist",
        Exit::Permission => {
            "permission denied on the root, or unreadable directories under `--strict`"
        }
        Exit::Index => {
            "a saved scan is damaged, or of an index version this duscope cannot read: \
             scan again"
        }
        Exit::Interrupted => "interrupted (SIGINT)",
    }
}

/// The verbs and utilities, as a table of `duscope <name>` and the line
/// `--help` gives each.
fn verbs() -> String {
    let mut command = Cli::command();
    command.build();
    let mut out = String::from("| Command | What it does |\n| --- | --- |\n");
    for verb in visible_verbs(&command) {
        let about = verb
            .get_about()
            .map(|about| about.to_string().replace('|', "\\|"))
            .unwrap_or_default();
        let _ = writeln!(out, "| `duscope {}` | {about} |", verb.get_name());
    }
    out.truncate(out.trim_end().len());
    out
}

/// The exit codes, as a table.
fn exit_codes() -> String {
    let mut out = String::from("| Exit | Meaning |\n| --- | --- |\n");
    for exit in EXITS {
        let _ = writeln!(out, "| {} | {} |", exit.code(), meaning(exit));
    }
    out.truncate(out.trim_end().len());
    out
}

/// The error codes, a line for each exit code they carry.
fn error_codes() -> String {
    let mut out = String::new();
    for exit in EXITS {
        let codes: Vec<String> = ERROR_CODES
            .iter()
            .filter(|&&(_, code)| code == exit.code())
            .map(|&(name, _)| format!("`{name}`"))
            .collect();
        if !codes.is_empty() {
            let _ = writeln!(out, "- exit {}: {}", exit.code(), codes.join(", "));
        }
    }
    out.truncate(out.trim_end().len());
    out
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};
    use duscope_core::envelope::ERROR_CODES;

    use super::{EXITS, TEMPLATE, briefing};
    use crate::cli::{Cli, visible_verbs};

    /// The `duscope …` commands the template's prose and tables name, split
    /// as a shell splits them (double quotes kept together), with PATH,
    /// HANDLE, the scan ids and the elided statement filled in as an agent
    /// would fill them.
    fn template_commands() -> Vec<Vec<String>> {
        let prose: String = TEMPLATE
            .split("```")
            .step_by(2)
            .collect::<Vec<_>>()
            .join("\n");
        let mut commands = Vec::new();
        for span in prose.split('`').skip(1).step_by(2) {
            if span != "duscope" && !span.starts_with("duscope ") {
                continue;
            }
            let span = span.replace("\"SELECT …\"", "\"SELECT 1\"");
            let mut words = Vec::new();
            let mut word = String::new();
            let mut quoted = false;
            for c in span.chars() {
                match c {
                    '"' => quoted = !quoted,
                    c if c.is_whitespace() && !quoted => {
                        if !word.is_empty() {
                            words.push(std::mem::take(&mut word));
                        }
                    }
                    c => word.push(c),
                }
            }
            words.push(word);
            let filled = words
                .into_iter()
                .map(|w| match w.as_str() {
                    "PATH" => "/tmp".to_owned(),
                    "HANDLE" => "aGFuZGxl".to_owned(),
                    "OLD" | "ID" => "1fd3".to_owned(),
                    "NEW" => "7a2c".to_owned(),
                    _ => w,
                })
                .collect();
            commands.push(filled);
        }
        commands
    }

    #[test]
    fn p3_agt_01_every_command_in_the_text_parses() {
        let commands = template_commands();
        assert!(commands.len() > 15, "{commands:?}");
        for words in commands {
            if let Err(e) = Cli::try_parse_from(&words) {
                panic!("{}: {e}", words.join(" "));
            }
        }
    }

    #[test]
    fn p3_agt_01_every_placeholder_is_filled() {
        assert!(!briefing().contains("{{"), "{}", briefing());
    }

    #[test]
    fn p3_agt_01_the_lists_are_the_binarys_own() {
        let text = briefing();
        let mut command = Cli::command();
        command.build();
        for verb in visible_verbs(&command) {
            assert!(
                text.contains(&format!("| `duscope {}` |", verb.get_name())),
                "{}",
                verb.get_name()
            );
        }
        for (name, exit) in ERROR_CODES {
            assert!(
                EXITS.iter().any(|e| e.code() == exit),
                "{name} carries exit {exit}, which the briefing does not list"
            );
            assert!(text.contains(&format!("`{name}`")), "{name}");
        }
    }
}
