//! The terminal's size, for the static treemap: `TIOCGWINSZ` on stdout,
//! the one query the map needs from the terminal (no escape sequences).

/// The columns and rows of the terminal on stdout; `None` when stdout is
/// not a terminal or it reports no size.
pub(crate) fn terminal_size() -> Option<(u16, u16)> {
    // SAFETY: `winsize` is plain data, for which all-zero bytes are valid.
    let mut size: libc::winsize = unsafe { std::mem::zeroed() };
    // SAFETY: TIOCGWINSZ writes one `winsize` through the pointer, which
    // points to a live, writable `winsize`; fd 1 is only read.
    let ok = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &raw mut size) };
    (ok == 0 && size.ws_col > 0 && size.ws_row > 0).then_some((size.ws_col, size.ws_row))
}
