//! The panic hook: gives the terminal back before a panic's message prints,
//! so a crash in the viewer never leaves a broken shell (raw mode, the
//! alternate screen, the mouse reported). It does nothing while the viewer
//! does not hold the terminal, so every verb runs under it.
//!
//! In a build that aborts on a panic, as the release build does (DEV-87),
//! the hook then has the MCP server answer what it owes before the process
//! ends ([`duscope_mcp::worker::answer_before_abort`]): the running call and
//! the requests queued behind it. It does nothing when no server runs. Where
//! panics unwind, as in the tests, the server survives a verb's panic and
//! answers its calls itself, so the hook leaves it alone, unless
//! `DUSCOPE_TEST_PANIC=abort` (debug builds only) asks for the release
//! build's ending: the hook answers, then ends the process with
//! [`TEST_ABORT_STATUS`]. It does not abort, since on macOS an abort leaves
//! a crash report in the account's own `~/Library/Logs/DiagnosticReports`,
//! which a test must not write to. `DUSCOPE_TEST_PANIC=sigabrt` (debug builds
//! only) does abort once the hook has run, so only a Linux test sets it.

use std::sync::Once;

/// How `DUSCOPE_TEST_PANIC=abort` ends the process once the hook has run:
/// 134, the status a shell reports for a process that `SIGABRT` ended (128
/// and the signal's number, 6), which no exit code of duscope's is.
pub const TEST_ABORT_STATUS: i32 = 134;

/// Installs the hook, in front of the one already installed, which still
/// prints the message: once the terminal is back, on the main screen.
pub fn install() {
    static INSTALLED: Once = Once::new();
    INSTALLED.call_once(|| {
        // test-only: DUSCOPE_TEST_PANIC=abort or =sigabrt, in a debug build,
        // ends the process after a panic as the release build does, the MCP
        // server's answers first (the viewer reads DUSCOPE_TEST_PANIC=1).
        let knob = |value: &str| {
            cfg!(debug_assertions)
                && std::env::var_os("DUSCOPE_TEST_PANIC").is_some_and(|v| v == value)
        };
        let test_abort = knob("abort");
        let test_sigabrt = knob("sigabrt");
        let aborts = cfg!(panic = "abort") || test_abort || test_sigabrt;
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            duscope_tui::restore();
            if aborts {
                duscope_mcp::worker::answer_before_abort();
            }
            previous(info);
            if test_sigabrt {
                std::process::abort();
            }
            if test_abort {
                std::process::exit(TEST_ABORT_STATUS);
            }
        }));
    });
}
