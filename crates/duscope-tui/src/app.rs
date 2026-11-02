//! The viewer's state, what it draws and how it answers events, and the
//! loop that runs it: a frame, then one more for every event that changes
//! what is shown, until `q` or `Ctrl-C`.
//!
//! Until the panes arrive (P4-TUI-02) the screen is the static map's frame
//! at the terminal's size, with the key line on the last row.

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use duscope_core::query::TreeView;
use duscope_treemap::cells::{CellBuffer, Color, Depth};
use duscope_treemap::paint::{Footer, PaintOpts, frame};
use ratatui_core::backend::Backend;
use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect;
use ratatui_core::style::{Color as Shown, Modifier};
use ratatui_core::terminal::Terminal;

use crate::term::MIN_SIZE;

/// The key line: the bindings there are.
const KEYS: &str = "q quit";

/// What the loop does after an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    /// Nothing shown changed: wait for the next event.
    Wait,
    /// Draw a frame.
    Redraw,
    /// Give the terminal back.
    Quit,
}

/// The viewer.
pub struct App<'a> {
    view: &'a dyn TreeView,
    opts: PaintOpts<'a>,
    footer: Footer,
    depth: Depth,
    /// Inside a late answer to the background probe, which arrives as
    /// keys: `Alt-]`, then `11;rgb:` and hex digits, then `Alt-\` or
    /// `Ctrl-G`. Any other key ends it and counts as itself.
    answer: bool,
}

impl std::fmt::Debug for App<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("App")
            .field("root", &self.view.root_path())
            .field("depth", &self.depth)
            .finish_non_exhaustive()
    }
}

impl<'a> App<'a> {
    /// The viewer of `view`, painted with `opts` in colours of `depth`,
    /// with the frame's `legend`, `summary` line and first `warning`.
    pub fn new(
        view: &'a dyn TreeView,
        opts: PaintOpts<'a>,
        depth: Depth,
        legend: Vec<(duscope_core::rules::Class, f64)>,
        summary: String,
        warning: Option<String>,
    ) -> App<'a> {
        App {
            view,
            opts,
            footer: Footer {
                legend,
                summary,
                warning,
                next: KEYS.to_owned(),
            },
            depth,
            answer: false,
        }
    }

    /// Draws the viewer into `area` of `buf`: the frame, or below
    /// [`MIN_SIZE`] (a terminal made smaller while the viewer runs) one
    /// line asking for more room.
    pub fn draw(&self, area: Rect, buf: &mut Buffer) {
        let ascii = self.opts.glyphs == duscope_treemap::paint::Glyphs::Ascii;
        let cells = if area.width < MIN_SIZE.0 || area.height < MIN_SIZE.1 {
            let mut cells = CellBuffer::new(area.width, area.height.min(1));
            let line = format!(
                "duscope view needs at least {}×{}; this terminal is {}×{}",
                MIN_SIZE.0, MIN_SIZE.1, area.width, area.height
            );
            cells.text(
                0,
                0,
                &line,
                area.width,
                self.opts.pack.roles.text,
                false,
                ascii,
            );
            cells
        } else {
            // The static map's frame keeps its last row for the shell's
            // prompt; the viewer has none, so it is drawn a row taller and
            // the key line lands on the last row.
            let rows = area.height.saturating_add(1);
            frame(self.view, &self.opts, &self.footer, area.width, rows)
        };
        copy(&cells, self.depth, area, buf);
    }

    /// What `event` does: `q` and `Ctrl-C` quit, a resize redraws, and
    /// the rest waits for the next event.
    pub fn on(&mut self, event: &Event) -> Flow {
        match event {
            Event::Resize(..) => Flow::Redraw,
            Event::Key(key) if key.kind != KeyEventKind::Release => self.key(key),
            _ => Flow::Wait,
        }
    }

    fn key(&mut self, key: &KeyEvent) -> Flow {
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if self.answer {
            match (key.code, alt, ctrl) {
                (KeyCode::Char('\\'), true, _) | (KeyCode::Char('g'), false, true) => {
                    self.answer = false;
                    return Flow::Wait;
                }
                (KeyCode::Char(c), false, false)
                    if c.is_ascii_hexdigit() || "rgba:;/".contains(c) =>
                {
                    return Flow::Wait;
                }
                // Not part of an answer: the key counts.
                _ => self.answer = false,
            }
        }
        match (key.code, alt, ctrl) {
            (KeyCode::Char(']'), true, _) => {
                self.answer = true;
                Flow::Wait
            }
            (KeyCode::Char('q'), false, false) | (KeyCode::Char('c'), false, true) => Flow::Quit,
            _ => Flow::Wait,
        }
    }
}

/// Runs `app` on `terminal`: a frame, then the events `next` waits for,
/// each answered as [`App::on`] says, until one quits. `Terminal::draw`
/// reads the terminal's size again for every frame, so a resize is one
/// more frame.
pub fn run<B, E>(
    terminal: &mut Terminal<B>,
    app: &mut App<'_>,
    mut next: impl FnMut() -> Result<Event, E>,
) -> Result<(), E>
where
    B: Backend,
    E: From<B::Error>,
{
    terminal.draw(|f| app.draw(f.area(), f.buffer_mut()))?;
    loop {
        match app.on(&next()?) {
            Flow::Quit => return Ok(()),
            Flow::Redraw => {
                terminal.draw(|f| app.draw(f.area(), f.buffer_mut()))?;
            }
            Flow::Wait => {}
        }
    }
}

/// Copies `cells` into `area` of `buf`, in colours of `depth`.
fn copy(cells: &CellBuffer, depth: Depth, area: Rect, buf: &mut Buffer) {
    let mut symbol = String::new();
    for y in 0..cells.height().min(area.height) {
        for x in 0..cells.width().min(area.width) {
            let Some(cell) = cells.get(x, y) else {
                continue;
            };
            let Some(target) = buf.cell_mut((area.x + x, area.y + y)) else {
                continue;
            };
            target.reset();
            // The right half of a wide character: the terminal prints the
            // character from the cell to its left, and the frame diff skips
            // this one.
            if cell.ch == '\0' {
                target.set_char(' ');
            } else if cell.marks == ['\0'; 2] {
                target.set_char(cell.ch);
            } else {
                symbol.clear();
                symbol.push(cell.ch);
                symbol.extend(cell.marks.iter().filter(|&&m| m != '\0'));
                target.set_symbol(&symbol);
            }
            if depth != Depth::None {
                target.fg = color(cell.fg, depth);
                target.bg = color(cell.bg, depth);
                if cell.bold {
                    target.modifier = Modifier::BOLD;
                }
            }
        }
    }
}

/// `color` as the terminal shows it at `depth`: 24-bit colours as they
/// are, or the nearest of the 256 xterm colours; the 16 ANSI colours by
/// name, which writes the same escapes the static map writes.
fn color(color: Color, depth: Depth) -> Shown {
    const ANSI: [Shown; 16] = [
        Shown::Black,
        Shown::Red,
        Shown::Green,
        Shown::Yellow,
        Shown::Blue,
        Shown::Magenta,
        Shown::Cyan,
        Shown::Gray,
        Shown::DarkGray,
        Shown::LightRed,
        Shown::LightGreen,
        Shown::LightYellow,
        Shown::LightBlue,
        Shown::LightMagenta,
        Shown::LightCyan,
        Shown::White,
    ];
    match (color, depth) {
        (Color::Reset, _) | (_, Depth::None) => Shown::Reset,
        (Color::Ansi(n), _) => ANSI[usize::from(n % 16)],
        (Color::Rgb(r, g, b), Depth::TrueColor) => Shown::Rgb(r, g, b),
        (rgb @ Color::Rgb(..), Depth::Ansi256 | Depth::Ansi16) => {
            Shown::Indexed(rgb.to_256().unwrap_or(16))
        }
    }
}

/// `buf` as snapshot text, in the form of the static map's
/// (`CellBuffer::snapshot`): the symbols row by row, then each row's runs
/// of style as `count fg bg[ b]`. The viewer's buffer tests compare it
/// with golden files.
pub fn snapshot(buf: &Buffer) -> String {
    use std::fmt::Write as _;
    let area = buf.area;
    let mut out = String::new();
    for y in area.top()..area.bottom() {
        let mut skip = 0usize;
        for x in area.left()..area.right() {
            let symbol = buf[(x, y)].symbol();
            if skip > 0 {
                skip -= 1;
                continue;
            }
            out.push_str(symbol);
            skip = unicode_width::UnicodeWidthStr::width(symbol).saturating_sub(1);
        }
        out.push('\n');
    }
    out.push_str("--- styles: count fg bg [b]\n");
    let token = |color: Shown| match color {
        Shown::Reset => "-".to_owned(),
        Shown::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        Shown::Indexed(n) => n.to_string(),
        named => format!("{named:?}"),
    };
    for y in area.top()..area.bottom() {
        let mut runs: Vec<(usize, (Shown, Shown, bool))> = Vec::new();
        for x in area.left()..area.right() {
            let cell = &buf[(x, y)];
            let style = (cell.fg, cell.bg, cell.modifier.contains(Modifier::BOLD));
            match runs.last_mut() {
                Some((count, last)) if *last == style => *count += 1,
                _ => runs.push((1, style)),
            }
        }
        let line: Vec<String> = runs
            .iter()
            .map(|(count, (fg, bg, bold))| {
                format!(
                    "{count} {} {}{}",
                    token(*fg),
                    token(*bg),
                    if *bold { " b" } else { "" }
                )
            })
            .collect();
        let _ = writeln!(out, "{}: {}", y - area.top(), line.join(" | "));
    }
    out
}
