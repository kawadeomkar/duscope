//! The `duscope` binary: the command line, the verbs, and the exit codes.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod cli;
mod out;
mod verbs;

use std::process::ExitCode;

use clap::error::ErrorKind;
use clap::parser::ValueSource;
use clap::{CommandFactory, FromArgMatches};

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
    let cli = match parse() {
        Ok(cli) => cli,
        Err(error) => return refused(&error),
    };
    // scan draws a treemap on a terminal; the other verbs print tables.
    let treemap = match &cli.verb {
        None => Some(cli.scan.treemap.unwrap_or(TreemapWhen::Auto)),
        Some(Verb::Scan(args)) => Some(args.treemap.unwrap_or(TreemapWhen::Auto)),
        Some(_) => None,
    };
    let output = match Output::decide(&cli.output, treemap) {
        Ok(output) => output,
        Err(failure) => {
            out::to_stderr(&failure);
            return ExitCode::from(failure.exit.code());
        }
    };
    match verbs::run(cli, &output) {
        Ok(()) => ExitCode::from(Exit::Success.code()),
        // Ctrl-C is the user's own doing: one line on stderr, no envelope.
        Err(failure) if failure.exit == Exit::Interrupted => {
            verbs::report_interrupt();
            ExitCode::from(failure.exit.code())
        }
        Err(failure) => {
            output.failure(&failure);
            ExitCode::from(failure.exit.code())
        }
    }
}

/// The command line. Without a verb, `scan`'s flags and path come first;
/// with one, only the global (output) flags may come before it.
fn parse() -> Result<Cli, clap::Error> {
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
    Cli::from_arg_matches(&matches)
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
}
