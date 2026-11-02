//! `agent`: the briefing an AI agent reads before it uses duscope, as
//! Markdown on stdout. The text is `briefing.md.tmpl`; when it runs, the
//! binary fills in its own verbs from the command line, the exit codes, and
//! the error codes with the exit code of each, so the briefing names what
//! this binary does. The MCP server's `instructions` are the briefing's first
//! 4,096 characters (P3-MCP-03), which is why the template opens with the
//! words that should bring duscope to mind, the invariants and the handle
//! flow.

use std::fmt::Write as _;

use clap::CommandFactory;
use duscope_core::envelope::{ERROR_CODES, SCHEMA_VERSION};

use crate::cli::Cli;
use crate::out::emit_bytes;
use crate::verbs::{Exit, Failure};

/// The briefing, with `{{verbs}}`, `{{exit_codes}}`, `{{error_codes}}` and
/// `{{schema_version}}` to fill in.
const TEMPLATE: &str = include_str!("briefing.md.tmpl");

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

/// Prints the briefing.
pub fn run() -> Result<(), Failure> {
    emit_bytes(briefing().as_bytes())
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
    for verb in command
        .get_subcommands()
        .filter(|verb| !verb.is_hide_set() && verb.get_name() != "help")
    {
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
    use clap::CommandFactory;
    use duscope_core::envelope::ERROR_CODES;

    use super::{EXITS, briefing};
    use crate::cli::Cli;

    #[test]
    fn p3_agt_01_every_placeholder_is_filled() {
        assert!(!briefing().contains("{{"), "{}", briefing());
    }

    #[test]
    fn p3_agt_01_the_lists_are_the_binarys_own() {
        let text = briefing();
        let mut command = Cli::command();
        command.build();
        for verb in command.get_subcommands().filter(|v| !v.is_hide_set()) {
            if verb.get_name() != "help" {
                assert!(
                    text.contains(&format!("| `duscope {}` |", verb.get_name())),
                    "{}",
                    verb.get_name()
                );
            }
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
