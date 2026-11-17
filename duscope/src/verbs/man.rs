//! `man`: the manual page, in roff. `clap_mangen` writes the name,
//! synopsis, description and options from the clap tree, so a new flag
//! cannot miss the page; the sections `clap` knows nothing of (INTERACTIVE
//! VIEWER, CLEANING UP, SQL QUERIES, MCP SERVER, AGENTS, ENVIRONMENT, EXIT
//! STATUS, FILES, EXAMPLES, CAVEATS) are written by hand beside this file.
//! The tests hold the viewer's key list to its key map, the refusals
//! `CLEANING UP` lists to `clean`'s, the tools of MCP SERVER to the
//! server's, and the tables, functions and limits of SQL QUERIES to the
//! sandbox's. Hidden (test-only) flags are left out.
//!
//! Each flag is described once: the scan and output flags `duscope` itself
//! takes under SCAN OPTIONS and OUTPUT OPTIONS, the filters the list verbs
//! share under FILTER OPTIONS, then under VERBS each verb with its usage
//! and the flags only it takes.

use std::collections::HashMap;
use std::fmt::Write as _;

use clap::{Arg, Command, CommandFactory};
use clap_mangen::Man;

use super::{Exit, Failure};
use crate::cli::{Cli, visible_verbs};
use crate::out::emit_bytes;

/// The page's date, changed with its hand-written prose.
const DATE: &str = "2026-11-16";

/// The hand-written sections, in order.
const SECTIONS: [&str; 10] = [
    include_str!("man/viewer.roff"),
    include_str!("man/clean.roff"),
    include_str!("man/sql.roff"),
    include_str!("man/mcp.roff"),
    include_str!("man/agent.roff"),
    include_str!("man/environment.roff"),
    include_str!("man/exit-status.roff"),
    include_str!("man/files.roff"),
    include_str!("man/examples.roff"),
    include_str!("man/caveats.roff"),
];

/// The help heading of the filters the list verbs share.
const FILTERS: &str = "Filters";

/// Prints the page.
pub fn run() -> Result<(), Failure> {
    let page = page().map_err(|e| {
        Failure::new(
            Exit::Runtime,
            "io_error",
            format!("cannot write the manual page: {e}"),
        )
    })?;
    emit_bytes(page.as_bytes())
}

/// The page.
pub fn page() -> std::io::Result<String> {
    let mut command = Cli::command();
    command.build();
    let man = Man::new(command.clone()).manual("duscope manual");
    let mut out = String::new();
    out += &roff(|w| man.render_title(w))?;
    out += &roff(|w| man.render_name_section(w))?;
    out += &roff(|w| man.render_synopsis_section(w))?;
    out += &roff(|w| man.render_description_section(w))?;
    // duscope's own flags, a section per heading: SCAN OPTIONS, OUTPUT
    // OPTIONS.
    out += &headed(&roff(|w| man.render_options_section(w))?);
    let verbs: Vec<&Command> = visible_verbs(&command).collect();
    // What is described above, by flag: its help text.
    let mut described: HashMap<String, String> = command
        .get_arguments()
        .map(|a| (a.get_id().to_string(), help_of(a)))
        .collect();
    // The filters, once, from the first verb that takes them.
    let takers: Vec<&Command> = verbs
        .iter()
        .copied()
        .filter(|v| v.get_arguments().any(is_filter))
        .collect();
    if let Some(first) = takers.first() {
        let names: Vec<String> = takers.iter().map(|v| v.get_name().to_owned()).collect();
        let _ = writeln!(
            out,
            ".SH \"FILTER OPTIONS\"\nThe verbs {} take these filters.",
            escape(&list(&names))
        );
        let filters = Man::new(only(first, is_filter));
        out += &bare(&roff(|w| filters.render_options_section(w))?);
        described.extend(
            first
                .get_arguments()
                .filter(|a| is_filter(a))
                .map(|a| (a.get_id().to_string(), help_of(a))),
        );
    }
    // Each verb: its summary, its usage and the flags only it takes.
    out += ".SH VERBS\n";
    for verb in verbs {
        let name = format!("duscope {}", verb.get_name());
        let _ = writeln!(out, ".SS \"{name}\"");
        if let Some(about) = verb.get_about() {
            let _ = writeln!(out, "{}", escape(&about.to_string()));
        }
        let usage = verb.clone().bin_name(&name).render_usage().to_string();
        let usage = usage.trim_start_matches("Usage:").trim();
        let _ = writeln!(out, ".PP\n\\fB{}\\fR", escape(usage));
        let new = Man::new(only(verb, |a| {
            described.get(a.get_id().as_str()) != Some(&help_of(a))
                && !matches!(a.get_id().as_str(), "help" | "version")
        }));
        out += &bare(&roff(|w| new.render_options_section(w))?);
    }
    for section in SECTIONS {
        out += section;
    }
    out += &roff(|w| man.render_version_section(w))?;
    Ok(tidy(&out))
}

/// What a `clap_mangen` renderer writes.
fn roff(render: impl FnOnce(&mut Vec<u8>) -> std::io::Result<()>) -> std::io::Result<String> {
    let mut bytes = Vec::new();
    render(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// `command` with every flag but those `keep` takes hidden.
fn only(command: &Command, keep: impl Fn(&Arg) -> bool) -> Command {
    let hide: Vec<String> = command
        .get_arguments()
        .filter(|a| !keep(a))
        .map(|a| a.get_id().to_string())
        .collect();
    hide.iter().fold(command.clone(), |command, id| {
        command.mut_arg(id, |a| a.hide(true))
    })
}

fn is_filter(arg: &Arg) -> bool {
    arg.get_help_heading() == Some(FILTERS)
}

fn help_of(arg: &Arg) -> String {
    arg.get_help().map(ToString::to_string).unwrap_or_default()
}

/// The options roff of `duscope` itself, each heading `X` a section
/// `X OPTIONS`.
fn headed(roff: &str) -> String {
    let mut out = String::with_capacity(roff.len());
    for line in roff.lines() {
        match line.strip_prefix(".SH ") {
            Some("OPTIONS") | None => out.push_str(line),
            Some(heading) => {
                let _ = write!(out, ".SH \"{} OPTIONS\"", heading.trim_matches('"'));
            }
        }
        out.push('\n');
    }
    out
}

/// The options roff of one part of the page, without its section
/// headings: the flags fall under the heading the page gives them.
fn bare(roff: &str) -> String {
    let mut out = String::with_capacity(roff.len());
    for line in roff.lines().filter(|line| !line.starts_with(".SH ")) {
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// `a, b and c`.
fn list(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// `clap_mangen`'s roff, made clean under `mandoc -T lint`: the title in
/// capitals with the page's date, no line break beside a blank line, no
/// trailing whitespace.
fn tidy(roff: &str) -> String {
    let version = env!("CARGO_PKG_VERSION");
    let roff = roff.replace(".br\n\n.br\n", ".sp\n");
    let mut out = String::with_capacity(roff.len());
    for line in roff.lines() {
        if line.starts_with(".TH ") {
            let _ = writeln!(
                out,
                ".TH DUSCOPE 1 {DATE} \"duscope {version}\" \"duscope manual\""
            );
            continue;
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

/// `text` with the characters roff reads as requests made literal.
fn escape(text: &str) -> String {
    let text = text.replace('\\', "\\e").replace('-', "\\-");
    if text.starts_with('.') || text.starts_with('\'') {
        format!("\\&{text}")
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use clap::{Command, CommandFactory};
    use duscope_core::envelope::values;

    use super::page;
    use crate::cli::Cli;

    /// The lines of `page`'s section `.SH name`, and within it, given
    /// `sub`, those of its subsection `.SS sub`: up to the next heading of
    /// the same level or above.
    fn part<'p>(page: &'p str, name: &str, sub: Option<&str>) -> Vec<&'p str> {
        let quoted = |s: &str| {
            if s.contains(' ') {
                format!("\"{s}\"")
            } else {
                s.to_owned()
            }
        };
        let within = |lines: Vec<&'p str>, heading: String, level: &[&str]| -> Vec<&'p str> {
            lines
                .into_iter()
                .skip_while(|l| *l != heading)
                .skip(1)
                .take_while(|l| !level.iter().any(|m| l.starts_with(m)))
                .collect()
        };
        let lines = within(
            page.lines().collect(),
            format!(".SH {}", quoted(name)),
            &[".SH "],
        );
        match sub {
            Some(sub) => within(lines, format!(".SS {}", quoted(sub)), &[".SH ", ".SS "]),
            None => lines,
        }
    }

    /// The words of each `.TP` entry's tag in `lines` (the line after the
    /// `.TP`), its macro, roff's quotes and escapes and the commas between
    /// keys gone: `.BR q ", " Ctrl\-C` is `q`, `Ctrl-C`.
    fn tags(lines: &[&str]) -> Vec<Vec<String>> {
        lines
            .windows(2)
            .filter(|pair| pair[0] == ".TP")
            .map(|pair| {
                let tag = pair[1];
                let text = tag
                    .strip_prefix(".BR ")
                    .or_else(|| tag.strip_prefix(".B "))
                    .unwrap_or(tag);
                text.replace("\\-", "-")
                    .replace(['"', ','], " ")
                    .split_whitespace()
                    .map(str::to_owned)
                    .collect()
            })
            .collect()
    }

    #[test]
    fn p4_doc_01_the_viewers_keys_in_the_page_are_its_key_map() {
        let text = page().unwrap();
        let listed = tags(&part(&text, "INTERACTIVE VIEWER", Some("Keys")));
        let viewer: Vec<Vec<String>> = duscope_tui::key_rows()
            .into_iter()
            .map(|row| row.names)
            .collect();
        assert_eq!(
            listed, viewer,
            "INTERACTIVE VIEWER's key list against the keys `?` lists, row by row"
        );
    }

    #[test]
    fn p4_doc_01_the_refusals_in_the_page_are_clean_s() {
        let text = page().unwrap();
        let mut listed: Vec<String> = tags(&part(&text, "CLEANING UP", Some("Refusals")))
            .into_iter()
            .flatten()
            .collect();
        listed.sort();
        let mut refusals = values::REFUSALS.map(str::to_owned).to_vec();
        refusals.sort();
        assert_eq!(listed, refusals, "CLEANING UP's refusals against clean's");
    }

    #[test]
    fn p3_doc_01_the_tools_in_the_page_are_the_server_s() {
        let text = page().unwrap();
        let mut listed: Vec<String> = tags(&part(&text, "MCP SERVER", Some("Tools")))
            .into_iter()
            .flatten()
            .collect();
        listed.sort();
        let mut shipped: Vec<String> = duscope_mcp::tools::shipped()
            .iter()
            .map(|tool| tool.name().to_owned())
            .collect();
        shipped.sort();
        assert_eq!(
            listed, shipped,
            "MCP SERVER's tools against the tools the server ships"
        );
    }

    #[test]
    fn p3_doc_01_the_skill_s_folders_in_the_page_are_the_ones_it_writes() {
        let text = page().unwrap();
        let skill = part(&text, "AGENTS", Some("The skill")).join("\n");
        for folder in crate::agent::SKILL_DIRS {
            assert!(
                skill.contains(&format!(".I {folder}/")),
                "AGENTS names {folder}/:\n{skill}"
            );
        }
    }

    #[test]
    fn p5_doc_01_the_sql_section_is_the_sandbox_s_tables_functions_and_limits() {
        use duscope_core::query::sql;
        let text = page().unwrap();
        let words = |lines: Vec<&str>| -> Vec<String> {
            lines
                .join(" ")
                .replace(['(', ')', ',', '"'], " ")
                .split_whitespace()
                .map(str::to_owned)
                .collect()
        };
        let allowed = words(part(&text, "SQL QUERIES", Some("What a statement may do")));
        for table in sql::TABLES {
            assert!(
                allowed.iter().any(|w| w == table),
                "SQL QUERIES names the table {table}"
            );
        }
        // Each `.BI name( arg )` tag is one of duscope's functions, which
        // the sandbox allows.
        let mut functions: Vec<String> =
            part(&text, "SQL QUERIES", Some("Functions and parameters"))
                .iter()
                .filter_map(|l| l.strip_prefix(".BI "))
                .filter_map(|l| l.split_once('(').map(|(name, _)| name.to_owned()))
                .collect();
        functions.sort();
        assert_eq!(functions, ["human_size", "path", "under"]);
        assert!(
            functions
                .iter()
                .all(|f| sql::FUNCTIONS.contains(&f.as_str()))
        );
        let limits = part(&text, "SQL QUERIES", Some("Limits")).join(" ");
        for figure in [
            format!("{} rows at most", sql::DEFAULT_LIMIT),
            format!(
                "{},{:03} by default and at most",
                sql::MAX_ROWS / 1000,
                sql::MAX_ROWS % 1000
            ),
            format!("after {} seconds", sql::DEFAULT_TIMEOUT.as_secs()),
            format!("{} at most", sql::MAX_TIMEOUT.as_secs()),
        ] {
            assert!(
                limits.contains(&figure),
                "SQL QUERIES' limits say {figure:?}"
            );
        }
    }

    #[test]
    fn p4_doc_01_the_page_s_parts_and_tags_are_read_as_roff_writes_them() {
        let roff = ".SH NAME\nx\n.SH \"A B\"\ntext\n.SS Keys\n.TP\n.BR q \", \" Ctrl\\-C\n\
                    Quit.\n.TP\n.B /\nFilter.\n.SS Other\n.TP\n.B z\n.SH C\n";
        let keys = part(roff, "A B", Some("Keys"));
        assert_eq!(tags(&keys), [vec!["q", "Ctrl-C"], vec!["/"]]);
        assert_eq!(part(roff, "A B", None).len(), 11);
        assert!(part(roff, "A B", Some("Missing")).is_empty());
    }

    /// Every argument of `command` and its verbs, with whether it is hidden.
    fn args(command: &Command, out: &mut Vec<(String, bool)>) {
        for arg in command.get_arguments() {
            if let Some(long) = arg.get_long() {
                out.push((format!("--{long}"), arg.is_hide_set()));
            }
        }
        for verb in command.get_subcommands() {
            args(verb, out);
        }
    }

    #[test]
    fn p1_cli_04_every_visible_flag_is_in_the_page_and_no_hidden_one() {
        let text = page().unwrap();
        // roff writes a flag's dashes as \-.
        let shown = text.replace("\\-", "-");
        let mut all = Vec::new();
        let mut command = Cli::command();
        command.build();
        args(&command, &mut all);
        assert!(all.len() > 40, "{all:?}");
        for (flag, hidden) in &all {
            if *hidden {
                let shown_as = [
                    format!("{flag}\\fR"),
                    format!("{flag} "),
                    format!("{flag}="),
                ];
                assert!(
                    !shown_as.iter().any(|form| shown.contains(form.as_str())),
                    "{flag} is hidden but in the page"
                );
            } else if flag != "--help" && flag != "--version" {
                assert!(
                    shown.contains(flag.as_str()),
                    "{flag} is missing from the page"
                );
            }
        }
        // Each section once, in order; each verb a subsection of VERBS.
        let sections: Vec<&str> = text
            .lines()
            .filter_map(|l| l.strip_prefix(".SH "))
            .map(|s| s.trim_matches('"'))
            .collect();
        assert_eq!(
            sections,
            [
                "NAME",
                "SYNOPSIS",
                "DESCRIPTION",
                "OPTIONS",
                "SCAN OPTIONS",
                "OUTPUT OPTIONS",
                "FILTER OPTIONS",
                "VERBS",
                "INTERACTIVE VIEWER",
                "CLEANING UP",
                "SQL QUERIES",
                "MCP SERVER",
                "AGENTS",
                "ENVIRONMENT",
                "EXIT STATUS",
                "FILES",
                "EXAMPLES",
                "CAVEATS",
                "VERSION",
            ]
        );
        for verb in command.get_subcommands().filter(|v| !v.is_hide_set()) {
            let heading = format!(".SS \"duscope {}\"", verb.get_name());
            assert_eq!(
                text.contains(&heading),
                verb.get_name() != "help",
                "{heading}"
            );
        }
    }
}
