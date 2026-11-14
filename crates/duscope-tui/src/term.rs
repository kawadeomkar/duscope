//! The terminal the viewer draws on: raw mode, the alternate screen, mouse
//! capture and bracketed paste (so that a paste arrives as one event, text
//! for the filter prompt alone, never as keys), taken on entry and given back on every
//! way out (a return, an error, a panic through the binary's hook, and
//! `SIGTERM` or `SIGHUP`, after which the process ends by that signal); the
//! size the viewer checks before it starts; its events, a burst of resizes
//! read as one; and the background probe.

use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io::{self, Read, Stdout, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, Once, PoisonError, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossterm::cursor::Show;
use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event,
};
use crossterm::execute;
use crossterm::terminal::{
    self, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use duscope_treemap::cells::Color;
use ratatui_core::terminal::Terminal;
use ratatui_crossterm::CrosstermBackend;
use signal_hook::consts::{SIGHUP, SIGTERM};
use signal_hook::low_level::{self, pipe};

/// The smallest terminal the viewer starts in, columns and rows.
pub const MIN_SIZE: (u16, u16) = (80, 24);

/// How long the background probe waits for the terminal's answer.
const PROBE_WAIT: Duration = Duration::from_millis(50);

/// Whether the viewer holds the terminal: set once raw mode is on, cleared
/// by the [`restore`] that gives it back.
static HELD: AtomicBool = AtomicBool::new(false);

/// Whether the command lines [`open_detached`] started last are still
/// running: set before the first starts, cleared when the last is done.
static REVEALING: AtomicBool = AtomicBool::new(false);

/// The event read behind a burst of resizes, which [`next_event`] gives
/// next.
static HELD_BACK: Mutex<Option<Event>> = Mutex::new(None);

/// The signals that end the viewer from outside it: `kill`'s default and
/// a terminal that hung up. In raw mode `Ctrl-C` and `Ctrl-\` are keys, not
/// `SIGINT` and `SIGQUIT`.
const ENDING: [libc::c_int; 2] = [SIGTERM, SIGHUP];

/// How long a signal's restore may wait for a terminal that takes no
/// output before the process ends anyway.
const RESTORE_WAIT: Duration = Duration::from_secs(1);

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
    /// then the alternate screen, mouse capture and bracketed paste. A step
    /// that fails gives back what the ones before it took. The first call
    /// in a process first has `SIGTERM` and `SIGHUP` answered
    /// (`answer_signals`).
    pub fn enter(probe: bool) -> io::Result<Tui> {
        answer_signals();
        enable_raw_mode()?;
        HELD.store(true, Ordering::SeqCst);
        let background = if probe { background(PROBE_WAIT) } else { None };
        let taken = execute!(
            io::stdout(),
            EnterAlternateScreen,
            EnableMouseCapture,
            EnableBracketedPaste
        )
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

/// Gives the terminal back if the viewer holds it: bracketed paste and
/// mouse reporting off, the main screen, the cursor shown, raw mode off. Does nothing otherwise,
/// so a panic hook can call it whatever was running, and a second call
/// (the hook, then the unwinding drop) writes nothing. Its callers (a
/// drop, the panic hook, a signal's thread) take turns, holding stdout: a
/// second one waits for the first to finish, then finds nothing to do.
pub fn restore() {
    if !HELD.load(Ordering::SeqCst) {
        return;
    }
    let mut out = io::stdout().lock();
    if HELD.swap(false, Ordering::SeqCst) {
        // Nothing to do about a failure here: this is the way out.
        let _ = execute!(
            out,
            DisableBracketedPaste,
            DisableMouseCapture,
            LeaveAlternateScreen,
            Show
        );
        let _ = disable_raw_mode();
    }
}

/// Answers `SIGTERM` and `SIGHUP` for the rest of the process, from the
/// first call on: for each, a thread that waits on a pipe the signal's
/// handler writes a byte to, then gives the terminal back if the viewer
/// holds it ([`end_by`]) and ends the process by the signal, as its default
/// action would. Once the viewer has given the terminal back the thread
/// only ends the process, so a signal then has its default effect; the
/// actions stay registered, since signal-hook would leave a signal whose
/// last action is gone ignored rather than restore its default. Where a
/// thread or its pipe cannot be had, nothing is registered for its signal,
/// which keeps its default.
fn answer_signals() {
    static ANSWERING: Once = Once::new();
    ANSWERING.call_once(|| {
        for signal in ENDING {
            let (registered, ready) = mpsc::channel();
            let waiting = thread::Builder::new()
                .name("duscope-signal".to_owned())
                .spawn(move || {
                    // A pipe, not signal-hook's iterator, whose self-pipe is
                    // a pair of sockets: crossterm's pair is the only socket
                    // the binary opens (SECURITY.md).
                    let caught = io::pipe().and_then(|(reader, writer)| {
                        pipe::register(signal, writer)?;
                        Ok(reader)
                    });
                    let _ = registered.send(());
                    let mut byte = [0u8];
                    if let Ok(mut reader) = caught
                        && reader.read_exact(&mut byte).is_ok()
                    {
                        end_by(signal);
                    }
                });
            // Registered before the terminal is taken.
            if waiting.is_ok() {
                let _ = ready.recv();
            }
        }
    });
}

/// Gives the terminal back if the viewer holds it, then ends the process by
/// `signal`, its default action. Stdout is taken first and held to the
/// end: a restore already under way (a drop, the panic hook) finishes
/// before the process ends, and no frame the viewer was drawing reaches
/// the main screen after it. A terminal that takes no output, whose writes
/// block, holds that up: [`RESTORE_WAIT`] later the process ends all the
/// same.
fn end_by(signal: libc::c_int) {
    let _ = thread::Builder::new()
        .name("duscope-signal-wait".to_owned())
        .spawn(move || {
            thread::sleep(RESTORE_WAIT);
            let _ = low_level::emulate_default_handler(signal);
        });
    let _held = io::stdout().lock();
    restore();
    // Puts the default action back, unblocks the signal and raises it; it
    // returns only where that failed, after an abort.
    let _ = low_level::emulate_default_handler(signal);
}

/// The terminal's size, columns and rows.
pub fn size() -> io::Result<(u16, u16)> {
    terminal::size()
}

/// Starts the first of `commands` (each a program and its arguments: the
/// ones that show a path in the file manager, which the binary has from
/// `duscope-walk`) that will start, detached from the terminal: its own
/// process group, its streams on the null device. A thread starts it and
/// waits for it, so that a finished one leaves no zombie, and starts the
/// next when it exits with a failure, and so on down the list; the viewer
/// waits for the first to start, never for it to end. One call's command
/// lines run at a time: while an earlier call's are still running (a
/// `dbus-send` waits up to its reply timeout), nothing is started and the
/// call is ignored. An error says none could be started, or the thread
/// could not be; what they do with the path is their own business.
pub fn open_detached(commands: Vec<Vec<OsString>>) -> io::Result<()> {
    run_down(commands, &REVEALING)?;
    Ok(())
}

/// [`open_detached`], with `busy` saying whether an earlier call's command
/// lines are running: the thread that waits, which a test joins; `None`,
/// and nothing started, while they are.
fn run_down(
    commands: Vec<Vec<OsString>>,
    busy: &'static AtomicBool,
) -> io::Result<Option<JoinHandle<()>>> {
    if busy.swap(true, Ordering::SeqCst) {
        return Ok(None);
    }
    let (started, first) = mpsc::channel();
    let waiting = thread::Builder::new()
        .name("duscope-reveal".to_owned())
        .spawn(move || {
            let mut rest = commands.into_iter();
            let mut child = match start_next(&mut rest) {
                Ok(child) => child,
                Err(e) => {
                    busy.store(false, Ordering::SeqCst);
                    let _ = started.send(Err(e));
                    return;
                }
            };
            let _ = started.send(Ok(()));
            while child.wait().is_ok_and(|status| !status.success()) {
                match start_next(&mut rest) {
                    Ok(next) => child = next,
                    Err(_) => break,
                }
            }
            busy.store(false, Ordering::SeqCst);
        });
    let waiting = match waiting {
        Ok(waiting) => waiting,
        Err(e) => {
            busy.store(false, Ordering::SeqCst);
            return Err(e);
        }
    };
    match first.recv() {
        Ok(Ok(())) => Ok(Some(waiting)),
        Ok(Err(e)) => Err(e),
        Err(_) => {
            busy.store(false, Ordering::SeqCst);
            Err(io::Error::other("the thread starting it ended"))
        }
    }
}

/// Starts the next of `commands` that will start, detached; when none
/// will, the error of the last.
fn start_next(commands: &mut impl Iterator<Item = Vec<OsString>>) -> io::Result<Child> {
    let mut failed = io::Error::new(io::ErrorKind::NotFound, "no command to start");
    for line in commands {
        let Some((program, args)) = line.split_first() else {
            continue;
        };
        let started = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn();
        match started {
            Ok(child) => return Ok(child),
            Err(e) => failed = e,
        }
    }
    Err(failed)
}

/// Waits for the next key, mouse or resize event. A resize stands for the
/// resizes read with it: those already waiting behind it are read, and the
/// last size is the one given, so a window dragged through many sizes is
/// one more frame, at the size it came to.
pub fn next_event() -> io::Result<Event> {
    let mut held = HELD_BACK.lock().unwrap_or_else(PoisonError::into_inner);
    coalesced(&mut held, event::poll, event::read)
}

/// Whether an event can be read within `wait`: at once when a burst of
/// resizes held one back.
pub fn event_ready(wait: Duration) -> io::Result<bool> {
    if HELD_BACK
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .is_some()
    {
        return Ok(true);
    }
    event::poll(wait)
}

/// The next event, read with `read`, or the one `held` back last time. A
/// resize is followed by every event `poll` says is waiting (it is asked
/// with no wait) as long as they are resizes, and the last of them is
/// given; the first event after them that is not a resize is held back in
/// `held`, to be given next.
pub(crate) fn coalesced(
    held: &mut Option<Event>,
    mut poll: impl FnMut(Duration) -> io::Result<bool>,
    mut read: impl FnMut() -> io::Result<Event>,
) -> io::Result<Event> {
    if let Some(event) = held.take() {
        return Ok(event);
    }
    let mut event = read()?;
    while matches!(event, Event::Resize(..)) && poll(Duration::ZERO)? {
        match read()? {
            resize @ Event::Resize(..) => event = resize,
            other => {
                *held = Some(other);
                break;
            }
        }
    }
    Ok(event)
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

#[cfg(test)]
mod tests {
    //! P4-TUI-05: `o`'s command lines, started detached, each that cannot
    //! start or fails giving way to the next. P4-TUI-08: a burst of resizes
    //! read as one event, and drawn as one frame.

    use std::ffi::OsString;
    use std::fs;
    use std::io;
    use std::path::Path;
    use std::sync::atomic::AtomicBool;

    use duscope_testkit::TestEnv;

    use super::run_down;

    fn line(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    /// A command line that appends `what` to `log` and exits with `status`.
    fn record(log: &Path, what: &str, status: u8) -> Vec<OsString> {
        let script = format!("echo {what} >> \"$0\"; exit {status}");
        line(&["sh", "-c", &script, log.to_str().unwrap()])
    }

    #[test]
    fn p4_tui_05_o_runs_down_its_command_lines_until_one_succeeds() {
        static BUSY: AtomicBool = AtomicBool::new(false);
        let env = TestEnv::new();
        let log = env.work_dir().join("ran");
        // A program that cannot start and one that fails give way; the
        // first that succeeds ends the list.
        let commands = vec![
            line(&["/nonexistent/duscope-no-such-program"]),
            record(&log, "failed", 3),
            record(&log, "shown", 0),
            record(&log, "never", 0),
        ];
        run_down(commands, &BUSY).unwrap().unwrap().join().unwrap();
        assert_eq!(fs::read_to_string(&log).unwrap(), "failed\nshown\n");
        // A last one that fails ends it too.
        fs::remove_file(&log).unwrap();
        run_down(vec![record(&log, "only", 1)], &BUSY)
            .unwrap()
            .unwrap()
            .join()
            .unwrap();
        assert_eq!(fs::read_to_string(&log).unwrap(), "only\n");
    }

    #[test]
    fn p4_tui_05_o_says_so_when_no_command_line_starts() {
        static BUSY: AtomicBool = AtomicBool::new(false);
        let failed = run_down(
            vec![
                line(&["/nonexistent/duscope-first"]),
                line(&["/nonexistent/duscope-second"]),
            ],
            &BUSY,
        )
        .unwrap_err();
        assert_eq!(failed.kind(), io::ErrorKind::NotFound);
        assert!(run_down(Vec::new(), &BUSY).is_err());
        assert!(run_down(vec![Vec::new()], &BUSY).is_err());
    }

    #[test]
    fn p4_tui_05_o_runs_one_reveal_at_a_time() {
        static BUSY: AtomicBool = AtomicBool::new(false);
        let env = TestEnv::new();
        let (gate, log) = (env.work_dir().join("gate"), env.work_dir().join("ran"));
        // A reveal that runs until the gate opens, or ten seconds pass.
        let script = r#"n=0; while [ ! -e "$0" ] && [ $n -lt 1000 ]; do sleep 0.01; n=$((n+1)); done; echo first >> "$1""#;
        let first = line(&[
            "sh",
            "-c",
            script,
            gate.to_str().unwrap(),
            log.to_str().unwrap(),
        ]);
        let first = run_down(vec![first], &BUSY).unwrap().unwrap();
        // While it runs another is ignored: nothing starts, no thread.
        for _ in 0..3 {
            let second = run_down(vec![record(&log, "second", 0)], &BUSY);
            assert!(second.unwrap().is_none());
        }
        fs::write(&gate, "").unwrap();
        first.join().unwrap();
        assert_eq!(fs::read_to_string(&log).unwrap(), "first\n");
        // Once it is done the next starts, and one that cannot start
        // leaves none running.
        let next = run_down(vec![record(&log, "next", 0)], &BUSY);
        next.unwrap().unwrap().join().unwrap();
        assert!(run_down(vec![line(&["/nonexistent/duscope-x"])], &BUSY).is_err());
        let last = run_down(vec![record(&log, "last", 0)], &BUSY);
        last.unwrap().unwrap().join().unwrap();
        assert_eq!(fs::read_to_string(&log).unwrap(), "first\nnext\nlast\n");
    }

    /// A terminal's queued events, read as [`super::coalesced`] reads them:
    /// `poll` says whether one waits, `read` takes the next.
    struct Queue {
        events: std::cell::RefCell<std::collections::VecDeque<crossterm::event::Event>>,
        reads: std::cell::Cell<usize>,
        polls: std::cell::Cell<usize>,
    }

    impl Queue {
        fn of(events: Vec<crossterm::event::Event>) -> Queue {
            Queue {
                events: std::cell::RefCell::new(events.into()),
                reads: std::cell::Cell::new(0),
                polls: std::cell::Cell::new(0),
            }
        }

        /// The next event, with `held` kept between calls.
        fn next(
            &self,
            held: &mut Option<crossterm::event::Event>,
        ) -> io::Result<crossterm::event::Event> {
            super::coalesced(
                held,
                |wait| {
                    assert_eq!(wait, std::time::Duration::ZERO);
                    self.polls.set(self.polls.get() + 1);
                    Ok(!self.events.borrow().is_empty())
                },
                || {
                    self.reads.set(self.reads.get() + 1);
                    Ok(self
                        .events
                        .borrow_mut()
                        .pop_front()
                        .expect("read with nothing queued"))
                },
            )
        }
    }

    fn key(c: char) -> crossterm::event::Event {
        use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
        Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))
    }

    #[test]
    fn p4_tui_08_a_burst_of_resizes_is_read_as_one_event() {
        use crossterm::event::Event;

        // Three resizes waiting, a key behind them, a resize after it: the
        // last size of the three, then the key held back, then the resize
        // alone, as nothing waits behind it.
        let queue = Queue::of(vec![
            Event::Resize(81, 24),
            Event::Resize(90, 30),
            Event::Resize(120, 40),
            key('j'),
            Event::Resize(100, 30),
        ]);
        let mut held = None;
        assert_eq!(queue.next(&mut held).unwrap(), Event::Resize(120, 40));
        assert_eq!(held, Some(key('j')));
        assert_eq!(queue.reads.get(), 4);
        assert_eq!(queue.next(&mut held).unwrap(), key('j'));
        assert_eq!(queue.reads.get(), 4);
        assert_eq!(queue.next(&mut held).unwrap(), Event::Resize(100, 30));
        assert_eq!(held, None);
        // Polled behind each resize: three found one waiting, the last
        // found none.
        assert_eq!((queue.reads.get(), queue.polls.get()), (5, 4));
        // A key is given as it is read: nothing is polled behind it.
        let queue = Queue::of(vec![key('k'), Event::Resize(80, 24)]);
        assert_eq!(queue.next(&mut held).unwrap(), key('k'));
        assert_eq!((queue.reads.get(), queue.polls.get()), (1, 0));
    }

    #[test]
    fn p4_tui_08_a_burst_of_resizes_yields_one_redraw() {
        use std::convert::Infallible;
        use std::path::{Path, PathBuf};

        use crossterm::event::Event;
        use duscope_core::query::ArenaView;
        use duscope_treemap::cells::Depth;
        use duscope_treemap::packs::Pack;
        use duscope_treemap::paint::ColorBy;
        use ratatui_core::backend::TestBackend;
        use ratatui_core::terminal::Terminal;

        use crate::panes::tests::{classes, mockup, opts, status};
        use crate::{App, run};

        let tree = mockup();
        let view = ArenaView::new(&tree.0, &tree.1, PathBuf::from("/Users/alex/Library"));
        let (mocha, classes) = (Pack::named("mocha").unwrap(), classes());
        // A window dragged through twenty sizes, a key that draws nothing
        // new, and `q`.
        let mut events: Vec<Event> = (0..20u16)
            .map(|i| Event::Resize(100 + i, 30 + i / 2))
            .collect();
        events.extend([key('x'), key('q')]);
        let frames = |coalesce: bool| {
            let mut app = App::new(
                &view,
                opts(&mocha, &classes, ColorBy::Type),
                Depth::TrueColor,
                Some(Path::new("/Users/alex")),
                status(&view),
                None,
            );
            let mut terminal = Terminal::new(TestBackend::new(119, 39)).unwrap();
            let queue = Queue::of(events.clone());
            let mut held = None;
            run(&mut terminal, &mut app, || -> Result<Event, Infallible> {
                Ok(if coalesce {
                    queue.next(&mut held).unwrap()
                } else {
                    queue.reads.set(queue.reads.get() + 1);
                    queue.events.borrow_mut().pop_front().unwrap()
                })
            })
            .unwrap();
            assert!(queue.events.borrow().is_empty());
            terminal.get_frame().count()
        };
        // The first frame and one for the burst; read one at a time, one
        // for each size.
        assert_eq!(frames(true), 2);
        assert_eq!(frames(false), 21);
    }
}
