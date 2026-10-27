//! `man`: the manual page, in roff. `clap_mangen` writes the name,
//! synopsis, description and options from the clap tree, so a new flag
//! cannot miss the page; the sections `clap` knows nothing of (ENVIRONMENT,
//! EXIT STATUS, FILES, EXAMPLES) are written by hand beside this file.
//! Hidden (test-only) flags are left out.
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
use crate::cli::Cli;
use crate::out::emit_bytes;

/// The page's date, changed with its hand-written prose.
const DATE: &str = "2026-10-27";

/// The hand-written sections, in order.
const SECTIONS: [&str; 4] = [
    include_str!("man/environment.roff"),
    include_str!("man/exit-status.roff"),
    include_str!("man/files.roff"),
    include_str!("man/examples.roff"),
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
    let verbs: Vec<&Command> = command
        .get_subcommands()
        .filter(|v| !v.is_hide_set() && v.get_name() != "help")
        .collect();
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

    use super::page;
    use crate::cli::Cli;

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
                "ENVIRONMENT",
                "EXIT STATUS",
                "FILES",
                "EXAMPLES",
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
