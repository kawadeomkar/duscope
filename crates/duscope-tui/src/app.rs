//! The viewer's state, what it draws and how it answers events, and the
//! loop that runs it: a frame, then one more for every event that changes
//! what is shown, until `q` or `Ctrl-C`.
//!
//! The screen is the canvas's ([`crate::layout`]): the header with the
//! badge, the scan's path, the view tabs and the status; the breadcrumbs to
//! the selection; the map in its pane; the selected, siblings and legend
//! panes beside it, or the selected pane folded under it; and the key line.
//! The selection starts at the largest file, reached through the largest
//! child at every level, as the canvas opens.

use std::collections::HashMap;
use std::path::Path;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use duscope_core::query::{TreeView, children};
use duscope_core::rules::Class;
use duscope_treemap::cells::{CellBuffer, Color, Depth};
use duscope_treemap::paint::{ColorBy, Glyphs, PaintOpts, classes_of, paint_unlabelled};
use ratatui_core::backend::Backend;
use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect;
use ratatui_core::style::{Color as Shown, Modifier};
use ratatui_core::terminal::Terminal;
use unicode_width::UnicodeWidthChar;

use crate::layout::{Panes, inner, layout};
use crate::panes::{self, Context, LegendRow};
use crate::term::MIN_SIZE;
use crate::widgets::{Canvas, Message, Notice, border};

/// The key line: the bindings there are.
const KEYS: [(&str, &str); 1] = [("q", "quit")];

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
    depth: Depth,
    /// The scan's totals, for the status.
    summary: String,
    /// The first warning, for the key line.
    warning: Option<String>,
    /// The root's path as the header writes it: `~` for the home folder.
    root: String,
    /// Each entry's class, by id.
    class_of: Vec<Class>,
    /// Each top-level folder's `--color-by folder` index.
    ranks: HashMap<u32, usize>,
    /// The legend's title and rows.
    legend: (&'static str, Vec<LegendRow>),
    /// The selected entry.
    selected: u32,
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
    /// The viewer of `view`, painted with `opts` in colours of `depth`; the
    /// root's path written from `home` (`~`), the scan's `summary` in the
    /// status and the first `warning` on the key line.
    pub fn new(
        view: &'a dyn TreeView,
        opts: PaintOpts<'a>,
        depth: Depth,
        home: Option<&Path>,
        summary: String,
        warning: Option<String>,
    ) -> App<'a> {
        let class_of = classes_of(view, opts.classes);
        let ranks = panes::ranks(view, &opts);
        let root = root_label(view.root_path(), home);
        let legend = panes::legend_rows(&Context {
            view,
            opts: &opts,
            class_of: &class_of,
            ranks: &ranks,
            root: &root,
        });
        App {
            view,
            selected: largest_file(view, &opts),
            summary,
            warning,
            root,
            class_of,
            ranks,
            legend,
            opts,
            depth,
            answer: false,
        }
    }

    /// The selected entry's id.
    #[must_use]
    pub fn selected(&self) -> u32 {
        self.selected
    }

    /// Draws the viewer into `area` of `buf`: the frame, or below
    /// [`MIN_SIZE`] (a terminal made smaller while the viewer runs) one
    /// line asking for more room.
    pub fn draw(&self, area: Rect, buf: &mut Buffer) {
        let ascii = self.opts.glyphs == Glyphs::Ascii;
        let cells = if area.width < MIN_SIZE.0 || area.height < MIN_SIZE.1 {
            let mut cells = CellBuffer::new(area.width, area.height.min(1));
            let by = if ascii { 'x' } else { '×' };
            let line = format!(
                "duscope view needs at least {}{by}{}; this terminal is {}{by}{}",
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
            self.screen(area.width, area.height)
        };
        copy(&cells, self.depth, area, buf);
    }

    /// The whole screen at `w`×`h`, at least [`MIN_SIZE`].
    fn screen(&self, w: u16, h: u16) -> CellBuffer {
        let pack = self.opts.pack;
        let roles = pack.roles;
        let at = layout(w, h);
        let mut canvas = Canvas::new(w, h, pack, self.opts.glyphs);
        let ctx = Context {
            view: self.view,
            opts: &self.opts,
            class_of: &self.class_of,
            ranks: &self.ranks,
            root: &self.root,
        };

        // The header: the badge, the path, the tabs, the status.
        let (y, end) = (at.header.y, at.header.right());
        let mut x = canvas.badge(0, y, end);
        let tabs = 39;
        let path_room = end.saturating_sub(x + tabs) / 2;
        let path = canvas.cut(&self.root, path_room.saturating_sub(3));
        x = canvas.spans(
            x,
            y,
            end,
            &[crate::widgets::Span::new(
                format!(" {path}  "),
                roles.subtext,
            )],
        );
        x = canvas.tabs(x, y, end, 0);
        canvas.status(x.saturating_add(1), y, end, &self.summary);

        // The breadcrumbs, from the root's path to the selection.
        canvas.crumbs(0, at.crumbs.y, at.crumbs.right(), &self.crumbs());

        // The map in its pane.
        let mode = match &self.opts.color_by {
            ColorBy::Type => "file type",
            ColorBy::Folder => "top folder",
            ColorBy::Level => "depth",
            ColorBy::Age => "age",
            ColorBy::LsColors(_) => "LS_COLORS",
        };
        let title = format!("treemap · {} · color: {mode}", self.root);
        canvas.pane(at.map_pane, &title, border(pack, true));
        let map = paint_unlabelled(self.view, &self.opts, at.map.width, at.map.height);
        for my in 0..map.height() {
            for mx in 0..map.width() {
                if let (Some(&cell), Some(slot)) = (
                    map.get(mx, my),
                    canvas.cells.get_mut(at.map.x + mx, at.map.y + my),
                ) {
                    *slot = cell;
                }
            }
        }

        // The panes.
        match at.panes {
            Panes::Column {
                selected,
                siblings,
                legend,
            } => {
                canvas.pane(selected, "selected", border(pack, false));
                panes::selected(&mut canvas, inner(selected), &ctx, self.selected);
                panes::siblings(&mut canvas, siblings, &ctx, self.selected);
                let total = self.view.node(0).size(self.opts.size);
                panes::legend(&mut canvas, legend, self.legend.0, &self.legend.1, total);
            }
            Panes::Folded { selected } => {
                canvas.pane(selected, "selected", border(pack, false));
                panes::folded(&mut canvas, inner(selected), &ctx, self.selected);
            }
        }

        // The keys, and the first warning where there is room.
        let message = self.warning.as_ref().map(|text| Message {
            text: text.clone(),
            kind: Notice::Warning,
        });
        canvas.key_line(1, at.keys.y, at.keys.right(), &KEYS, message.as_ref());
        canvas.cells
    }

    /// The breadcrumbs' segments: the root's path, then the folders down to
    /// the selection, and the selection.
    fn crumbs(&self) -> Vec<String> {
        let mut segments: Vec<String> = match self.root.strip_prefix('/') {
            Some(rest) => std::iter::once("/".to_owned())
                .chain(rest.split('/').filter(|s| !s.is_empty()).map(str::to_owned))
                .collect(),
            None => self.root.split('/').map(str::to_owned).collect(),
        };
        if self.selected != 0 {
            let rel = String::from_utf8_lossy(&self.view.path(self.selected)).into_owned();
            segments.extend(rel.split('/').map(str::to_owned));
        }
        segments
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

/// The root's path as the header writes it: under `home`, `~` and the rest.
fn root_label(root: &Path, home: Option<&Path>) -> String {
    if let Some(rest) = home.and_then(|home| root.strip_prefix(home).ok()) {
        let rest = rest.to_string_lossy();
        return if rest.is_empty() {
            "~".to_owned()
        } else {
            format!("~/{rest}")
        };
    }
    root.to_string_lossy().into_owned()
}

/// The largest file, reached through the largest child at every level (the
/// first of equals); a folder with nothing in it ends the way; the root of
/// an empty scan.
fn largest_file(view: &dyn TreeView, opts: &PaintOpts<'_>) -> u32 {
    if view.is_empty() {
        return 0;
    }
    let mut at = view.node(0);
    while at.is_dir() {
        let Some(next) = children(view, &at)
            .map(|id| view.node(id))
            .reduce(|best, n| {
                if n.size(opts.size) > best.size(opts.size) {
                    n
                } else {
                    best
                }
            })
        else {
            break;
        };
        at = next;
    }
    at.id
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
pub(crate) fn copy(cells: &CellBuffer, depth: Depth, area: Rect, buf: &mut Buffer) {
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
            // A wide character is whole when its right half follows it, a
            // `'\0'` cell, which the frame diff skips while the terminal
            // prints the character from the cell to its left. Either half
            // without the other (the other painted over) is a space, as the
            // static map prints it.
            let wide = UnicodeWidthChar::width(cell.ch) == Some(2);
            let whole = cell.ch != '\0'
                && (!wide || cells.get(x + 1, y).is_some_and(|next| next.ch == '\0'));
            if !whole {
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
/// are, or the nearest of the 256 xterm colours, or of the eight basic
/// ones on a 16-colour terminal; the 16 ANSI colours by name. Each is the
/// colour the static map writes at that depth.
pub(crate) fn color(color: Color, depth: Depth) -> Shown {
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
        (rgb @ Color::Rgb(..), Depth::Ansi256) => Shown::Indexed(rgb.to_256().unwrap_or(16)),
        (rgb @ Color::Rgb(..), Depth::Ansi16) => ANSI[usize::from(rgb.to_ansi8().unwrap_or(7))],
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
