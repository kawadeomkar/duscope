//! The `duscope` binary: the command line, the verbs, and the exit codes.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod agent;
mod cli;
mod log;
mod out;
mod panic;
mod verbs;

use std::cell::RefCell;
use std::ffi::OsString;
use std::io;
use std::process::ExitCode;
use std::rc::Rc;

use clap::error::ErrorKind;
use clap::parser::ValueSource;
use clap::{ArgMatches, CommandFactory, FromArgMatches, ValueEnum};
use duscope_core::CoreError;
use duscope_core::config::{Config, ConfigSource};

use cli::{Cli, McpArgs, TreemapWhen, Verb};
use out::{Capture, Output};
use verbs::{Exit, Failure};

/// The musl builds (the Linux archives and `.deb`s) allocate with mimalloc:
/// musl's allocator serialises the walk's workers. macOS keeps the system
/// allocator. P1-BENCH-03 measured both (bench/results/allocator.json).
#[cfg(target_env = "musl")]
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

thread_local! {
    /// The command line being run: the process's own, or, while a verb
    /// runs for an MCP tool, that verb's (the next-page hint repeats it).
    static COMMAND_LINE: RefCell<Vec<OsString>> = const { RefCell::new(Vec::new()) };
}

/// The command line being run, `duscope` first.
pub fn command_line() -> Vec<OsString> {
    COMMAND_LINE.with(|line| line.borrow().clone())
}

fn main() -> ExitCode {
    panic::install();
    let args: Vec<OsString> = std::env::args_os().collect();
    COMMAND_LINE.with(|line| line.replace(args.clone()));
    let (cli, matches) = match parse(args.iter().cloned()) {
        Ok(parsed) => parsed,
        Err(error) => return refused(&error, &args),
    };
    log::init(match &cli.verb {
        Some(Verb::Mcp(McpArgs {
            log_level: Some(level),
            ..
        })) => log::Filter::parse(level.name()),
        _ => log::filter(cli.global.verbosity(), |name| std::env::var(name).ok()),
    });
    // mcp's stdout carries the protocol alone: a failure to start goes to
    // stderr.
    if let Some(Verb::Mcp(args)) = &cli.verb {
        return match verbs::mcp::serve(args, &cli.global, &cli.output) {
            Ok(()) => ExitCode::SUCCESS,
            Err(failure) => {
                out::to_stderr(&failure);
                ExitCode::from(failure.exit.code())
            }
        };
    }
    ExitCode::from(execute(cli, &matches, None))
}

/// Runs the command line `argv` (`duscope` first) in this process, as the
/// binary would run it, but with what it writes to stdout kept in a
/// buffer: the verb behind an MCP tool. Returns its exit code and what it
/// wrote.
pub fn run_captured(argv: Vec<OsString>) -> (u8, Vec<u8>) {
    let capture = Capture::default();
    let server = COMMAND_LINE.with(|line| line.replace(argv.clone()));
    let code = match parse(argv) {
        Ok((cli, matches)) => execute(cli, &matches, Some(&capture)),
        Err(error) => {
            let failure = usage_failure(&error);
            json_output(Some(Rc::clone(&capture))).failure(&failure);
            failure.exit.code()
        }
    };
    COMMAND_LINE.with(|line| line.replace(server));
    let written = capture.take();
    (code, written)
}

/// Runs the verb `cli` names, after the configuration file, and returns
/// the exit code. What would go to stdout goes to `capture` when there is
/// one.
fn execute(mut cli: Cli, matches: &ArgMatches, capture: Option<&Capture>) -> u8 {
    // scan draws a treemap on a terminal; the other verbs print tables.
    let treemap = match &cli.verb {
        None => Some(cli.scan.treemap.unwrap_or(TreemapWhen::Auto)),
        Some(Verb::Scan(args)) => Some(args.treemap.unwrap_or(TreemapWhen::Auto)),
        Some(_) => None,
    };
    let source = ConfigSource::locate(cli.global.config.as_deref(), |name| std::env::var_os(name));
    let configured = Config::load(&source)
        .map_err(|e| config_failure(&e))
        .and_then(|config| cli::apply_config(&mut cli, matches, &config));
    if let Err(failure) = configured {
        // Reported as the flags ask, without the file's own format.
        match Output::decide(&cli.output, treemap) {
            Ok(output) => captured(output, capture).failure(&failure),
            Err(_) => report_undecided(&failure, capture),
        }
        return failure.exit.code();
    }
    // import's --format names the file's format, not the output's.
    if let Some(Verb::Import(args)) = &mut cli.verb {
        args.format = cli.output.format.take();
    }
    // --all streams: ndjson, unless the flags name another format; the
    // JSON envelope holds a page, not every row.
    let all = cli.wants_all();
    let all_as_json = all && (cli.output.json || cli.output.format == Some(out::FormatArg::Json));
    if all && !cli.output.json && !cli.output.plain && cli.output.format.is_none() {
        cli.output.format = Some(out::FormatArg::Ndjson);
    }
    let mut output = match Output::decide(&cli.output, treemap) {
        Ok(output) => captured(output, capture),
        Err(failure) => {
            report_undecided(&failure, capture);
            return failure.exit.code();
        }
    };
    if let Some(failure) = format_refused(&cli) {
        output.failure(&failure);
        return failure.exit.code();
    }
    if all_as_json {
        let failure = Failure::new(
            Exit::Usage,
            "usage",
            "--all streams every row, which the JSON envelope does not hold",
        )
        .hint("Run the command with --all --format ndjson, or --json with --limit");
        output.failure(&failure);
        return failure.exit.code();
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
        Ok(()) => Exit::Success.code(),
        // Ctrl-C is the user's own doing: one line on stderr, no envelope.
        // A verb run for an MCP tool call was stopped by the call's
        // cancellation or deadline, and writes its envelope as for any
        // other failure.
        Err(failure) if failure.exit == Exit::Interrupted && capture.is_none() => {
            verbs::report_interrupt();
            failure.exit.code()
        }
        // A verb run for an MCP tool call wrote its envelope to the capture,
        // which nothing has read yet: a failed save, or one the call's
        // cancellation or deadline stopped, replaces it with the error, so
        // that the client is not handed a scan id no saved scan has
        // (P3-MCP-09). A stop before anything was written finds the capture
        // empty.
        Err(failure)
            if capture.is_some()
                && (failure.code == "index_write_failed"
                    || (failure.exit == Exit::Interrupted && !failure.reported)) =>
        {
            if let Some(capture) = capture {
                capture.borrow_mut().clear();
            }
            output.failure(&failure);
            failure.exit.code()
        }
        // On the command line an answer written already stands (a scan's
        // output, whose save failed after it; clean's plan, which carries
        // its own error): the failure goes to stderr, never as a second
        // envelope. A save before any output (query --sql, export) fails
        // as anything else does.
        Err(failure) if failure.reported => {
            out::to_stderr(&failure);
            failure.exit.code()
        }
        Err(failure) => {
            output.failure(&failure);
            failure.exit.code()
        }
    }
}

/// `output`, writing to `capture` when there is one.
fn captured(mut output: Output, capture: Option<&Capture>) -> Output {
    output.capture = capture.map(Rc::clone);
    output
}

/// A failure before the output's format was decided: on stderr, or, for a
/// verb an MCP tool runs, which always asks for JSON, as the error envelope.
fn report_undecided(failure: &Failure, capture: Option<&Capture>) {
    match capture {
        Some(capture) => json_output(Some(Rc::clone(capture))).failure(failure),
        None => out::to_stderr(failure),
    }
}

/// A configuration file that cannot be used: one that is not there is a
/// usage error (the flag or variable named it), one that cannot be read
/// otherwise a runtime error, and one with a bad key or value a usage error.
pub(crate) fn config_failure(error: &CoreError) -> Failure {
    let failure = match error {
        CoreError::ConfigRead { source, .. } if source.kind() != io::ErrorKind::NotFound => {
            Failure::new(Exit::Runtime, "io_error", error.to_string())
        }
        _ => Failure::new(Exit::Usage, "usage", error.to_string()),
    };
    failure.hint("Fix the configuration file, or run duscope with --config none")
}

/// A `--format` the verb does not take: beside `--json` or `--plain`,
/// which name the output as it does (import's, which names its input, was
/// moved out already), or one only another verb takes.
fn format_refused(cli: &Cli) -> Option<Failure> {
    let format = cli.output.format?;
    if cli.output.json || cli.output.plain {
        let flag = if cli.output.json { "--json" } else { "--plain" };
        return Some(
            Failure::new(
                Exit::Usage,
                "usage",
                format!("the argument '--format <FORMAT>' cannot be used with '{flag}'"),
            )
            .hint("Run duscope --help for the verbs and their flags"),
        );
    }
    let verbs = format.verbs()?;
    let taken = matches!(
        (format, &cli.verb),
        (
            out::FormatArg::Ncdu
                | out::FormatArg::Gpscan
                | out::FormatArg::Qdirstat
                | out::FormatArg::Csv,
            Some(Verb::Export(_))
        ) | (
            out::FormatArg::Svg | out::FormatArg::Png,
            Some(Verb::Render(_))
        )
    );
    if taken {
        return None;
    }
    let name = format
        .to_possible_value()
        .map_or_else(String::new, |v| v.get_name().to_owned());
    let verb = verbs.split(' ').next().unwrap_or(verbs);
    Some(
        Failure::new(
            Exit::Usage,
            "usage",
            format!("--format {name} is for {verbs} alone"),
        )
        .hint(format!("Run duscope {verb} --format {name}")),
    )
}

/// The command line and the matches of its verb (the top level's without
/// one). Without a verb, `scan`'s flags and path come first; with one, only
/// the global flags may come before it.
fn parse(argv: impl IntoIterator<Item = OsString>) -> Result<(Cli, ArgMatches), clap::Error> {
    let mut command = Cli::command();
    let matches = command.try_get_matches_from_mut(argv)?;
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
fn refused(error: &clap::Error, args: &[OsString]) -> ExitCode {
    let args: Vec<String> = args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let shown = matches!(
        error.kind(),
        ErrorKind::DisplayHelp
            | ErrorKind::DisplayVersion
            | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
    );
    if !shown && !Output::json_requested(&args) {
        // clap repeats the argument it refused. Built without its `color`
        // feature, it styles nothing and so strips nothing: a control
        // character in that argument survives `render().to_string()` as it
        // is, and text holding one is written as the rest of stderr is.
        let text = error.render().to_string();
        if text.chars().any(|c| c.is_control() && c != '\n') {
            text.trim_end().lines().for_each(out::stderr_line);
            return ExitCode::from(Exit::Usage.code());
        }
    }
    if shown || !Output::json_requested(&args) {
        error.exit();
    }
    let failure = usage_failure(error);
    json_output(None).failure(&failure);
    ExitCode::from(failure.exit.code())
}

/// The usage error of a command line clap refused: its first line.
fn usage_failure(error: &clap::Error) -> Failure {
    let rendered = error.render().to_string();
    let message = rendered
        .lines()
        .next()
        .unwrap_or_default()
        .trim_start_matches("error: ")
        .to_owned();
    Failure::new(Exit::Usage, "usage", message)
        .hint("Run duscope --help for the verbs and their flags")
}

/// The output of a failure reported as the JSON envelope whatever the
/// flags: to stdout, or to `capture`.
fn json_output(capture: Option<Capture>) -> Output {
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
        capture,
    }
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
