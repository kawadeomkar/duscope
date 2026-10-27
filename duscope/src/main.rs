//! The `duscope` binary: the command line, the verbs, and the exit codes.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod cli;
mod verbs;

use std::process::ExitCode;

use clap::Parser;

use cli::Cli;

fn main() -> ExitCode {
    // clap exits on its own for --help, --version (0) and usage errors (2).
    let cli = Cli::parse();
    match verbs::run(cli) {
        Ok(()) => ExitCode::from(verbs::Exit::Success.code()),
        Err(failure) => {
            eprintln!("duscope: {}", failure.message);
            if let Some(hint) = &failure.hint {
                eprintln!("hint: {hint}");
            }
            ExitCode::from(failure.exit.code())
        }
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
}
