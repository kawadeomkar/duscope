//! The `duscope` binary: the command line, the verbs, and the exit codes.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod cli;
mod log;
mod out;
mod verbs;

use std::io;
use std::process::ExitCode;

use clap::error::ErrorKind;
use clap::parser::ValueSource;
use clap::{ArgMatches, CommandFactory, FromArgMatches, ValueEnum};
use duscope_core::CoreError;
use duscope_core::config::{Config, ConfigSource};

use cli::{Cli, TreemapWhen, Verb};
use out::Output;
use verbs::{Exit, Failure};

/// The musl builds (the Linux archives and `.deb`s) allocate with mimalloc:
/// musl's allocator serialises the walk's workers. macOS keeps the system
/// allocator. P1-BENCH-03 measured both (bench/results/allocator.json).
#[cfg(target_env = "musl")]
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() -> ExitCode {
    let (mut cli, matches) = match parse() {
        Ok(parsed) => parsed,
        Err(error) => return refused(&error),
    };
    log::init(log::filter(cli.global.verbosity(), |name| {
        std::env::var(name).ok()
    }));
    // scan draws a treemap on a terminal; the other verbs print tables.
    let treemap = match &cli.verb {
        None => Some(cli.scan.treemap.unwrap_or(TreemapWhen::Auto)),
        Some(Verb::Scan(args)) => Some(args.treemap.unwrap_or(TreemapWhen::Auto)),
        Some(_) => None,
    };
    let source = ConfigSource::locate(cli.global.config.as_deref(), |name| std::env::var_os(name));
    let configured = Config::load(&source)
        .map_err(|e| config_failure(&e))
        .and_then(|config| cli::apply_config(&mut cli, &matches, &config));
    if let Err(failure) = configured {
        // Reported as the flags ask, without the file's own format.
        match Output::decide(&cli.output, treemap) {
            Ok(output) => output.failure(&failure),
            Err(_) => out::to_stderr(&failure),
        }
        return ExitCode::from(failure.exit.code());
    }
    // --all streams: ndjson, unless the flags name another format; the
    // JSON envelope holds a page, not every row.
    let all = cli.wants_all();
    let all_as_json = all && (cli.output.json || cli.output.format == Some(out::FormatArg::Json));
    if all && !cli.output.json && !cli.output.plain && cli.output.format.is_none() {
        cli.output.format = Some(out::FormatArg::Ndjson);
    }
    let mut output = match Output::decide(&cli.output, treemap) {
        Ok(output) => output,
        Err(failure) => {
            out::to_stderr(&failure);
            return ExitCode::from(failure.exit.code());
        }
    };
    if let Some(format) = cli.output.format.filter(|f| f.exports_only())
        && !matches!(cli.verb, Some(Verb::Export(_)))
    {
        let failure = Failure::new(
            Exit::Usage,
            "usage",
            format!(
                "--format {} is one of export's formats",
                format
                    .to_possible_value()
                    .map_or_else(String::new, |v| v.get_name().to_owned())
            ),
        )
        .hint("Run duscope export --format to write a saved scan in it");
        output.failure(&failure);
        return ExitCode::from(failure.exit.code());
    }
    if all_as_json {
        let failure = Failure::new(
            Exit::Usage,
            "usage",
            "--all streams every row, which the JSON envelope does not hold",
        )
        .hint("Run the command with --all --format ndjson, or --json with --limit");
        output.failure(&failure);
        return ExitCode::from(failure.exit.code());
    }
    // A spilled envelope goes beside the saved scans, where scans --prune
    // finds it.
    output.spill_dir = cli
        .global
        .index_dir
        .clone()
        .or_else(|| duscope_core::index::default_dir(|name| std::env::var_os(name)))
        .map(|dir| dir.join("out"));
    match verbs::run(cli, &output) {
        Ok(()) => ExitCode::from(Exit::Success.code()),
        // Ctrl-C is the user's own doing: one line on stderr, no envelope.
        Err(failure) if failure.exit == Exit::Interrupted => {
            verbs::report_interrupt();
            ExitCode::from(failure.exit.code())
        }
        // The index is written after the output, which stands: the failure
        // goes to stderr, never as a second envelope.
        Err(failure) if failure.code == "index_write_failed" => {
            out::to_stderr(&failure);
            ExitCode::from(failure.exit.code())
        }
        Err(failure) => {
            output.failure(&failure);
            ExitCode::from(failure.exit.code())
        }
    }
}

/// A configuration file that cannot be used: one that is not there is a
/// usage error (the flag or variable named it), one that cannot be read
/// otherwise a runtime error, and one with a bad key or value a usage error.
fn config_failure(error: &CoreError) -> Failure {
    let failure = match error {
        CoreError::ConfigRead { source, .. } if source.kind() != io::ErrorKind::NotFound => {
            Failure::new(Exit::Runtime, "io_error", error.to_string())
        }
        _ => Failure::new(Exit::Usage, "usage", error.to_string()),
    };
    failure.hint("Fix the configuration file, or run duscope with --config none")
}

/// The command line and the matches of its verb (the top level's without
/// one). Without a verb, `scan`'s flags and path come first; with one, only
/// the global flags may come before it.
fn parse() -> Result<(Cli, ArgMatches), clap::Error> {
    let mut command = Cli::command();
    let matches = command.try_get_matches_from_mut(std::env::args_os())?;
    if let Some((verb, _)) = matches.subcommand() {
        let before = command
            .get_arguments()
            .filter(|arg| !arg.is_global_set())
            .find(|arg| {
                matches.value_source(arg.get_id().as_str()) == Some(ValueSource::CommandLine)
            });
        if let Some(arg) = before {
            let name = arg.get_long().map_or_else(
                || arg.get_id().to_string().to_uppercase(),
                |long| format!("--{long}"),
            );
            return Err(command.error(
                ErrorKind::ArgumentConflict,
                format!("{name} comes before the verb; write it after: duscope {verb} {name} …"),
            ));
        }
    }
    let cli = Cli::from_arg_matches(&matches)?;
    let verb = matches
        .subcommand()
        .map_or_else(|| matches.clone(), |(_, sub)| sub.clone());
    Ok((cli, verb))
}

/// A command line clap did not run: `--help` and `--version` (exit 0), or a
/// usage error (exit 2), which is the error envelope when JSON was asked
/// for and clap's own message otherwise.
fn refused(error: &clap::Error) -> ExitCode {
    let args: Vec<String> = std::env::args_os()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let shown = matches!(
        error.kind(),
        ErrorKind::DisplayHelp
            | ErrorKind::DisplayVersion
            | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
    );
    if shown || !Output::json_requested(&args) {
        error.exit();
    }
    let rendered = error.render().to_string();
    let message = rendered
        .lines()
        .next()
        .unwrap_or_default()
        .trim_start_matches("error: ")
        .to_owned();
    let failure = Failure::new(Exit::Usage, "usage", message)
        .hint("Run duscope --help for the verbs and their flags");
    Output {
        format: out::Format::Json,
        color: false,
        units: out::Units::default(),
        null: false,
        header: false,
        quiet: false,
        fields: None,
        file: None,
        max_chars: 0,
        spill_dir: None,
    }
    .failure(&failure);
    ExitCode::from(failure.exit.code())
}

#[cfg(test)]
mod tests {
    use crate::verbs::Exit;

    #[test]
    fn p1_cli_01_the_exit_code_table() {
        let table = [
            (Exit::Success, 0),
            (Exit::Runtime, 1),
            (Exit::Usage, 2),
            (Exit::NotFound, 3),
            (Exit::Permission, 4),
            (Exit::Interrupted, 130),
        ];
        for (exit, code) in table {
            assert_eq!(exit.code(), code, "{exit:?}");
        }
    }

    #[test]
    fn p2_idx_03_a_saved_scan_that_cannot_be_read_exits_5() {
        assert_eq!(Exit::Index.code(), 5);
    }
}
