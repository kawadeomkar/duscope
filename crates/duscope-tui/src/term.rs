//! The terminal the viewer draws on: raw mode, the alternate screen and
//! mouse capture, taken on entry and given back on every way out (a
//! return, an error, a panic through the binary's hook); the size the
//! viewer checks before it starts; and the background probe.

use std::fs::OpenOptions;
use std::io::{self, Read, Stdout, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crossterm::cursor::Show;
use crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event};
use crossterm::execute;
use crossterm::terminal::{
    self, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use duscope_treemap::cells::Color;
use ratatui_core::terminal::Terminal;
use ratatui_crossterm::CrosstermBackend;

/// The smallest terminal the viewer starts in, columns and rows.
pub const MIN_SIZE: (u16, u16) = (80, 24);

/// How long the background probe waits for the terminal's answer.
const PROBE_WAIT: Duration = Duration::from_millis(50);

/// Whether the viewer holds the terminal: set once raw mode is on, cleared
/// by the [`restore`] that gives it back.
static HELD: AtomicBool = AtomicBool::new(false);

/// The terminal's background, as its answer to OSC 11 tells it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Background {
    /// Dark: light text reads on it.
    Dark,
    /// Light: dark text reads on it.
    Light,
}

/// The terminal, held by the viewer until this is dropped.
pub struct Tui {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    background: Option<Background>,
}

impl std::fmt::Debug for Tui {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tui")
            .field("background", &self.background)
            .finish_non_exhaustive()
    }
}

impl Tui {
    /// Takes the terminal: raw mode; then, when `probe`, the background
    /// question, which needs raw mode to read the answer and must come
    /// before the first event read, which would take the answer for keys;
    /// then the alternate screen and mouse capture. A step that fails gives
    /// back what the ones before it took.
    pub fn enter(probe: bool) -> io::Result<Tui> {
        enable_raw_mode()?;
        HELD.store(true, Ordering::SeqCst);
        let background = if probe { background(PROBE_WAIT) } else { None };
        let taken = execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture)
            .and_then(|()| Terminal::new(CrosstermBackend::new(io::stdout())))
            .and_then(|mut terminal| terminal.clear().map(|()| terminal));
        match taken {
            Ok(terminal) => Ok(Tui {
                terminal,
                background,
            }),
            Err(e) => {
                restore();
                Err(e)
            }
        }
    }

    /// The terminal to draw on.
    pub fn terminal(&mut self) -> &mut Terminal<CrosstermBackend<Stdout>> {
        &mut self.terminal
    }

    /// The terminal's background, when the probe ran and it answered.
    pub fn background(&self) -> Option<Background> {
        self.background
    }
}

impl Drop for Tui {
    fn drop(&mut self) {
        restore();
    }
}

/// Gives the terminal back if the viewer holds it: mouse reporting off,
/// the main screen, the cursor shown, raw mode off. Does nothing otherwise,
/// so a panic hook can call it whatever was running, and a second call
/// (the hook, then the unwinding drop) writes nothing.
pub fn restore() {
    if HELD.swap(false, Ordering::SeqCst) {
        // Nothing to do about a failure here: this is the way out.
        let _ = execute!(
            io::stdout(),
            DisableMouseCapture,
            LeaveAlternateScreen,
            Show
        );
        let _ = disable_raw_mode();
    }
}

/// The terminal's size, columns and rows.
pub fn size() -> io::Result<(u16, u16)> {
    terminal::size()
}

/// Waits for the next key, mouse or resize event.
pub fn next_event() -> io::Result<Event> {
    event::read()
}

/// Asks the terminal for its background colour (OSC 11), with a primary
/// device attributes query (DA1) behind it. Terminals answer in order and
/// every one answers DA1, so DA1's answer ends the wait early where OSC 11
/// gets none. Waits at most `wait`; reads the terminal device, opened on
/// its own and non-blocking, so nothing waits on a terminal that is silent.
fn background(wait: Duration) -> Option<Background> {
    let mut tty = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open("/dev/tty")
        .ok()?;
    tty.write_all(b"\x1b]11;?\x1b\\\x1b[c").ok()?;
    let deadline = Instant::now() + wait;
    let mut answer = Vec::new();
    let mut chunk = [0u8; 256];
    while Instant::now() < deadline && !answered_da1(&answer) {
        match tty.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => answer.extend_from_slice(&chunk[..n]),
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(_) => break,
        }
    }
    parse_background(&answer)
}

/// Whether `bytes` hold the answer to DA1: `ESC [ ?`, parameters, `c`.
pub(crate) fn answered_da1(bytes: &[u8]) -> bool {
    bytes.windows(3).enumerate().any(|(i, w)| {
        w == b"\x1b[?"
            && bytes[i + 3..]
                .iter()
                .find(|b| !(b.is_ascii_digit() || **b == b';'))
                .is_some_and(|&b| b == b'c')
    })
}

/// The background an answer to OSC 11 names: `ESC ] 11 ; rgb:R/G/B` (or
/// `rgba:R/G/B/A`), each channel one to four hex digits, ended by BEL or
/// ST; light when its relative luminance is at least one half, the rule
/// the packs' own dark flag follows.
pub(crate) fn parse_background(bytes: &[u8]) -> Option<Background> {
    let start = bytes.windows(5).position(|w| w == b"\x1b]11;")? + 5;
    let rest = &bytes[start..];
    let end = rest.iter().position(|&b| b == 0x07 || b == 0x1b)?;
    let text = std::str::from_utf8(&rest[..end]).ok()?;
    let channels = text
        .strip_prefix("rgb:")
        .or_else(|| text.strip_prefix("rgba:"))?;
    let mut rgb = [0u8; 3];
    let mut parts = channels.split('/');
    for value in &mut rgb {
        let hex = parts.next()?;
        if hex.is_empty() || hex.len() > 4 {
            return None;
        }
        let n = u32::from_str_radix(hex, 16).ok()?;
        let max = (1u32 << (4 * hex.len())) - 1;
        *value = u8::try_from(n * 255 / max).ok()?;
    }
    let luminance = Color::Rgb(rgb[0], rgb[1], rgb[2]).luminance()?;
    Some(if luminance < 0.5 {
        Background::Dark
    } else {
        Background::Light
    })
}
