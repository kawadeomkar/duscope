//! `man`: the manual page, in roff. `clap_mangen` writes the name,
//! synopsis, description and the options of `duscope` and of each verb
//! from the clap tree, so a new flag cannot miss the page; the sections
//! `clap` knows nothing of (ENVIRONMENT, EXIT STATUS, FILES, EXAMPLES) are
//! written by hand beside this file. Hidden (test-only) flags are left out.

use std::fmt::Write as _;

use clap::CommandFactory;
use clap_mangen::Man;

use super::{Exit, Failure};
use crate::cli::Cli;
use crate::out::emit_bytes;

/// The page's date, changed with its hand-written prose.
const DATE: &str = "2026-10-26";

/// The hand-written sections, in order.
const SECTIONS: [&str; 4] = [
    include_str!("man/environment.roff"),
    include_str!("man/exit-status.roff"),
    include_str!("man/files.roff"),
    include_str!("man/examples.roff"),
];

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
    let mut out = Vec::new();
    man.render_title(&mut out)?;
    man.render_name_section(&mut out)?;
    man.render_synopsis_section(&mut out)?;
    man.render_description_section(&mut out)?;
    man.render_options_section(&mut out)?;
    // Each verb: its summary and its own options.
    for verb in command.get_subcommands().filter(|v| !v.is_hide_set()) {
        let name = verb.get_name().to_uppercase();
        out.extend_from_slice(format!(".SH \"{name}\"\n").as_bytes());
        if let Some(about) = verb.get_about() {
            out.extend_from_slice(format!("{}\n", escape(&about.to_string())).as_bytes());
        }
        let options = Man::new(
            verb.clone()
                .bin_name(format!("duscope {}", verb.get_name())),
        );
        options.render_synopsis_section(&mut out)?;
        let mut flags = Vec::new();
        options.render_options_section(&mut flags)?;
        // The verb's options under its own heading.
        let flags = String::from_utf8_lossy(&flags).replacen(
            ".SH OPTIONS",
            &format!(".SS \"{name} OPTIONS\""),
            1,
        );
        out.extend_from_slice(flags.as_bytes());
    }
    for section in SECTIONS {
        out.extend_from_slice(section.as_bytes());
    }
    man.render_version_section(&mut out)?;
    Ok(tidy(&String::from_utf8_lossy(&out)))
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
        for section in [
            "ENVIRONMENT",
            "EXIT STATUS",
            "FILES",
            "EXAMPLES",
            "TOP OPTIONS",
        ] {
            assert!(
                text.contains(&format!(".SH {section}"))
                    || text.contains(&format!("\"{section}\"")),
                "{section}"
            );
        }
    }
}
