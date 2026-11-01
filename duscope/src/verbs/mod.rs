//! The verbs, and the failures they report.

mod completions;
mod diff;
mod explain;
mod man;
mod query;
mod scan;
mod scans;
mod source;
mod sql;
mod summary;
mod treemap;

pub use scan::report_interrupt;

use std::fmt;

use crate::cli::{Cli, Verb};
use crate::out::Output;

/// The exit codes of the conventions appendix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// Success, partial reads included.
    Success = 0,
    /// A runtime error.
    Runtime = 1,
    /// A usage error.
    Usage = 2,
    /// The path (or scan) does not exist.
    NotFound = 3,
    /// Permission denied on the root, or partial reads under `--strict`.
    Permission = 4,
    /// A saved scan of another index version, or a damaged one.
    Index = 5,
    /// Interrupted (SIGINT).
    Interrupted = 130,
}

impl Exit {
    /// The process exit status.
    pub fn code(self) -> u8 {
        self as u8
    }
}

/// Why a verb failed: an exit code, the error code of the conventions
/// appendix, one sentence for the user, and a hint naming a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    /// The exit code.
    pub exit: Exit,
    /// The error code (`scan_not_found`, `permission_denied`, …).
    pub code: &'static str,
    /// One sentence.
    pub message: String,
    /// One imperative sentence naming a command, where one helps.
    pub hint: Option<String>,
}

impl Failure {
    /// A failure with no hint.
    pub fn new(exit: Exit, code: &'static str, message: impl Into<String>) -> Failure {
        Failure {
            exit,
            code,
            message: message.into(),
            hint: None,
        }
    }

    /// The failure with a hint.
    #[must_use]
    pub fn hint(mut self, hint: impl Into<String>) -> Failure {
        self.hint = Some(hint.into());
        self
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

/// Runs the verb `cli` names, writing as `output` says.
pub fn run(cli: Cli, output: &Output) -> Result<(), Failure> {
    let index_dir = cli.global.index_dir.as_deref();
    match cli.verb {
        None => scan::run(&cli.scan, index_dir, output),
        Some(Verb::Scan(args)) => scan::run(&args, index_dir, output),
        Some(Verb::Top(args)) => query::top(&args, index_dir, output),
        Some(Verb::Tree(args)) => query::tree(&args, index_dir, output),
        Some(Verb::ByType(args)) => query::by_type(&args, index_dir, output),
        Some(Verb::Stale(args)) => query::stale(&args, index_dir, output),
        Some(Verb::Caches(args)) => query::caches(&args, index_dir, output),
        Some(Verb::Summary(args)) => summary::run(&args, index_dir, output),
        Some(Verb::Diff(args)) => diff::run(&args, index_dir, output),
        Some(Verb::Explain(args)) => explain::run(&args, index_dir, output),
        Some(Verb::Query(args)) => sql::run(&args, index_dir, output),
        Some(Verb::Scans(args)) => scans::run(&args, index_dir, output),
        Some(Verb::Man) => man::run(),
        Some(Verb::Completions(args)) => completions::run(args.shell),
    }
}
