//! The viewer's state, what it draws and how it answers events, and the
//! loop that runs it: a frame, then one more for every event that changes
//! what is shown, until `q` or `Ctrl-C`.
//!
//! The screen is the canvas's ([`crate::layout`]): the header with the
//! badge, the scan's path, the view tabs and the status; the breadcrumbs to
//! the selection; the map in its pane; the selected, siblings and legend
//! panes beside it, or the selected pane folded under it; and the key line.
//! The selection starts at the largest file, reached through the largest
//! child at every level, as the canvas opens; the navigation keys move it
//! and zoom the map ([`crate::cursor`]).
//!
//! `i` grows the selected pane into the full fact list over the right
//! column, or opens it over the map where the column is folded under it
//! ([`crate::panes::facts`]); `i` again or `Esc` closes it. The facts of an
//! entry are read once, `explain` among them, and kept.
//!
//! With `--treemap pixels` the map pane shows a picture over its cells: of
//! the zoom's root, in the whole map's colours, drawn once per root and
//! size and kept ([`crate::zoom::Layouts`]), with the cursor's marks drawn
//! into it and encoded again whenever they move.

use std::cell::RefCell;
use std::collections::HashMap;
use std::num::NonZeroU16;
use std::path::Path;
use std::rc::Rc;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use duscope_core::query::{Subtree, TreeView};
use duscope_core::rules::Class;
use duscope_treemap::Rect as Px;
use duscope_treemap::cells::{CellBuffer, Color, Depth};
use duscope_treemap::paint::{ColorBy, Glyphs, PaintOpts, classes_of};
use duscope_treemap::pixels::{Picture, PixelMap, Pixels};
use ratatui_core::backend::Backend;
use ratatui_core::buffer::{Buffer, CellDiffOption};
use ratatui_core::layout::Rect;
use ratatui_core::style::{Color as Shown, Modifier};
use ratatui_core::terminal::Terminal;
use unicode_width::UnicodeWidthChar;

use crate::cursor::{Cursor, Nav};
use crate::layout::{Panes, inner, layout};
use crate::panes::{self, Context, Facts, LegendRow, Ranks, Sources};
use crate::path::Path as Selection;
use crate::term::MIN_SIZE;
use crate::widgets::{Canvas, Message, Notice, border};
use crate::zoom::{self, KEPT_PICTURES, Layouts, Map};

/// The key line: the six most useful bindings, and their stand-ins
/// without UTF-8.
const KEYS: [(&str, &str); 6] = [
    ("←↓↑→", "move"),
    ("⏎", "open"),
    ("⌫", "up"),
    ("z", "zoom"),
    ("Z", "out"),
    ("q", "quit"),
];
const KEYS_ASCII: [(&str, &str); 6] = [
    ("hjkl", "move"),
    ("enter", "open"),
    ("bksp", "up"),
    ("z", "zoom"),
    ("Z", "out"),
    ("q", "quit"),
];

/// The entries whose facts are kept: moving back and forth with the facts
/// open reads each once.
const KEPT_FACTS: usize = 256;

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
    /// The folder the root is in, written the same way.
    root_parent: String,
    /// Each entry's class, by id.
    class_of: Vec<Class>,
    /// The class of most of each folder's bytes, as the panes meet them.
    dominant: RefCell<HashMap<u32, Class>>,
    /// The `--color-by folder` places of the root's tiles.
    ranks: Ranks,
    /// The legend's title and rows.
    legend: (&'static str, Vec<LegendRow>),
    /// The selected path, the zoom and the maps painted for them.
    cursor: Cursor,
    /// The `/` mask over the map, which `Esc` clears before it ascends.
    pub(crate) filter: Option<String>,
    /// `i`: the selected pane grown into the full fact list, which `Esc`
    /// closes before anything else.
    facts_open: bool,
    /// What the facts read besides the view.
    sources: Sources<'a>,
    /// The facts read, by id: `explain` runs once for an entry.
    facts: RefCell<HashMap<u32, Rc<Facts>>>,
    /// Inside a late answer to the background probe, which arrives as
    /// keys: `Alt-]`, then `11;rgb:` and hex digits, then `Alt-\` or
    /// `Ctrl-G`. Any other key ends it and counts as itself.
    answer: bool,
    /// `--treemap pixels`: the pictures, their encoder and what was sent.
    pixels: Option<InPixels>,
}

/// The map in pixels: what encodes it, the pictures drawn and the cells
/// last sent. Without the `pixels` feature no [`Pixels`] exists, so
/// neither does this, and none of the code that draws it is compiled.
struct InPixels {
    /// The encoder.
    encoder: Encoder,
    /// The terminal's cell, in pixels.
    cell: (u16, u16),
    /// The pictures at the map's size: the scan root's and those of the
    /// roots visited last.
    pictures: RefCell<Layouts<InPicture>>,
    /// The marks last encoded, and the cells the encoder made of them:
    /// encoded again only when the size, the zoom or the marks change.
    sent: RefCell<Option<(Marks, Option<PixelMap>)>>,
}

/// What encodes the pictures for the terminal.
enum Encoder {
    /// The terminal's protocol.
    Terminal(Pixels),
    /// None, in tests, which look at the pictures themselves.
    #[cfg(test)]
    Unencoded,
}

/// A picture of the map, and its tiles as the map the cursor moves on;
/// `None` where it could not be drawn, which leaves the cells.
pub(crate) type InPicture = Option<(Picture, Rc<Map>)>;

/// What a picture sent shows: its size in cells and its root, the
/// cursor's tile and its ancestors' that take the link colour, in pixels.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Marks {
    size: (u16, u16),
    root: u32,
    cursor: Px,
    links: Vec<Px>,
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
        let root_parent = view
            .root_path()
            .parent()
            .map_or_else(|| root.clone(), |parent| root_label(parent, home));
        let dominant = RefCell::new(HashMap::new());
        let legend = panes::legend_rows(&Context {
            view,
            opts: &opts,
            class_of: &class_of,
            dominant: &dominant,
            ranks: &ranks,
            root: &root,
            root_parent: &root_parent,
        });
        App {
            view,
            cursor: Cursor::new(Selection::largest(view, opts.size)),
            filter: None,
            facts_open: false,
            sources: Sources::of(view),
            facts: RefCell::new(HashMap::new()),
            summary,
            warning,
            root,
            root_parent,
            class_of,
            dominant,
            ranks,
            legend,
            opts,
            depth,
            answer: false,
            pixels: None,
        }
    }

    /// Reads the facts `i` shows from `sources`: the saved scan `explain`
    /// reads (the one the view reads where none is given), the cache rules,
    /// the environment and the scan's block.
    pub fn with_sources(&mut self, sources: Sources<'a>) {
        self.sources = Sources {
            index: sources.index.or_else(|| self.view.as_index()),
            ..sources
        };
        self.facts.borrow_mut().clear();
    }

    /// The facts of `id`: read once, then kept, as many as [`KEPT_FACTS`].
    fn facts_of(&self, id: u32) -> Rc<Facts> {
        if let Some(facts) = self.facts.borrow().get(&id) {
            return Rc::clone(facts);
        }
        let facts = Rc::new(Facts::read(&self.context(), &self.sources, id));
        let mut kept = self.facts.borrow_mut();
        if kept.len() >= KEPT_FACTS {
            kept.clear();
        }
        kept.insert(id, Rc::clone(&facts));
        facts
    }

    /// Whether the facts are open.
    #[cfg(test)]
    pub(crate) fn facts_open(&self) -> bool {
        self.facts_open
    }

    /// Draws the map in pixels with `pixels` (`--treemap pixels`), over
    /// its cells.
    pub fn draw_in_pixels(&mut self, pixels: Pixels) {
        let cell = pixels.cell();
        self.pixels = Some(InPixels {
            encoder: Encoder::Terminal(pixels),
            cell,
            pictures: RefCell::new(Layouts::pinned(KEPT_PICTURES)),
            sent: RefCell::new(None),
        });
    }

    /// Draws the pictures of the map at a cell of `cell` pixels as
    /// `--treemap pixels` draws them, without encoding them.
    #[cfg(test)]
    pub(crate) fn draw_in_pixels_unencoded(&mut self, cell: (u16, u16)) {
        self.pixels = Some(InPixels {
            encoder: Encoder::Unencoded,
            cell,
            pictures: RefCell::new(Layouts::pinned(KEPT_PICTURES)),
            sent: RefCell::new(None),
        });
    }

    /// The picture of the visible root at the last frame's size and its
    /// tiles, with `--treemap pixels`.
    #[cfg(test)]
    pub(crate) fn shown(&self) -> Option<Rc<InPicture>> {
        let pixels = self.pixels.as_ref()?;
        let (w, h) = pixels.pictures.borrow().size();
        Some(self.picture(pixels, w, h))
    }

    /// The picture shown with the cursor's marks, with `--treemap pixels`.
    #[cfg(test)]
    pub(crate) fn marked(&self) -> Option<duscope_treemap::pixels::Raster> {
        let shown = self.shown()?;
        let (picture, tiles) = shown.as_ref().as_ref()?;
        let (cursor, links) = self.cursor.marks(tiles, self.view);
        Some(picture.marked(cursor, &links))
    }

    /// How many pictures are kept, with `--treemap pixels`.
    #[cfg(test)]
    pub(crate) fn pictures_kept(&self) -> usize {
        self.pixels
            .as_ref()
            .map_or(0, |pixels| pixels.pictures.borrow().kept())
    }

    /// The selected entry's id.
    #[must_use]
    pub fn selected(&self) -> u32 {
        self.cursor.selected()
    }

    /// The selected path, the zoom and the maps painted for them.
    #[cfg(test)]
    pub(crate) fn cursor(&self) -> &Cursor {
        &self.cursor
    }

    /// The map the cursor is on, at the last frame's size.
    #[cfg(test)]
    pub(crate) fn map(&self) -> std::rc::Rc<crate::zoom::Map> {
        self.cursor.map(&self.context())
    }

    /// Puts the cursor on `id`, under the map's root, the path ending
    /// there.
    #[cfg(test)]
    pub(crate) fn jump(&mut self, id: u32) {
        self.cursor.jump(self.view, id);
    }

    /// The scan, how it is painted and what the viewer worked out once:
    /// what the panes and the cursor read.
    fn context(&self) -> Context<'_> {
        Context {
            view: self.view,
            opts: &self.opts,
            class_of: &self.class_of,
            dominant: &self.dominant,
            ranks: &self.ranks,
            root: &self.root,
            root_parent: &self.root_parent,
        }
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
        // The facts over the map cover its picture too.
        let covered = self.facts_open
            && matches!(layout(area.width, area.height).panes, Panes::Folded { .. });
        if area.width >= MIN_SIZE.0 && area.height >= MIN_SIZE.1 && !covered {
            self.draw_pixels(area, buf);
        }
    }

    /// With `--treemap pixels`, the map in pixels over its cells in the
    /// map pane: the picture of the zoomed folder, as the cells are, with
    /// the cursor's marks, encoded again only when the pane's size, the
    /// zoom or the marks change. The arrows move among its tiles. Where it
    /// cannot be drawn the cells stay, the cursor marked on them.
    fn draw_pixels(&self, area: Rect, buf: &mut Buffer) {
        let Some(pixels) = &self.pixels else {
            return;
        };
        let map = layout(area.width, area.height).map;
        let shown = self.picture(pixels, map.width, map.height);
        let Some((picture, tiles)) = shown.as_ref() else {
            self.cursor.show(None);
            return;
        };
        let (cursor, links) = self.cursor.marks(tiles, self.view);
        let marks = Marks {
            size: (map.width, map.height),
            root: tiles.root(),
            cursor,
            links,
        };
        let mut sent = pixels.sent.borrow_mut();
        if sent.as_ref().is_none_or(|(at, _)| *at != marks) {
            let cells = match &pixels.encoder {
                Encoder::Terminal(encoder) => encoder
                    .encode(picture.marked(marks.cursor, &marks.links))
                    .ok(),
                #[cfg(test)]
                Encoder::Unencoded => None,
            };
            *sent = Some((marks, cells));
        }
        let cells = sent.as_ref().and_then(|(_, cells)| cells.as_ref());
        if let Some(cells) = cells {
            overlay(cells, area.x + map.x, area.y + map.y, buf);
        }
        // The arrows move among the tiles shown: the picture's, unless its
        // encoder failed and the cells show through.
        let in_picture = match pixels.encoder {
            Encoder::Terminal(_) => cells.is_some(),
            #[cfg(test)]
            Encoder::Unencoded => true,
        };
        self.cursor.show(in_picture.then(|| Rc::clone(tiles)));
    }

    /// The picture of the visible root in `w`×`h` cells and its tiles: the
    /// one kept, or one drawn now (and kept) with the classes the viewer
    /// worked out once, in the whole map's colours.
    fn picture(&self, pixels: &InPixels, w: u16, h: u16) -> Rc<InPicture> {
        let root = self.cursor.zoom().root();
        pixels.pictures.borrow_mut().get(root, w, h, || {
            let base = zoom::base(&self.context(), root);
            let drawn = if root == 0 || self.view.is_empty() {
                Picture::draw(
                    self.view,
                    &self.opts,
                    w,
                    h,
                    pixels.cell,
                    Some(&self.class_of),
                    base,
                )
            } else {
                let last = self.view.node(root).last_id;
                let classes = self.class_of.get(root as usize..=last as usize);
                let sub = Subtree::new(self.view, root);
                Picture::draw(&sub, &self.opts, w, h, pixels.cell, classes, base)
            };
            drawn.ok().map(|picture| {
                let tiles = Map::of(
                    root,
                    CellBuffer::new(0, 0),
                    picture.area(),
                    picture.tiles().iter().copied(),
                );
                (picture, Rc::new(tiles))
            })
        })
    }

    /// The whole screen at `w`×`h`, at least [`MIN_SIZE`].
    fn screen(&self, w: u16, h: u16) -> CellBuffer {
        let pack = self.opts.pack;
        let roles = pack.roles;
        let at = layout(w, h);
        let mut canvas = Canvas::new(w, h, pack, self.opts.glyphs);
        let ctx = self.context();

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
        let shown = self.shown_path(self.cursor.zoom().root());
        let title = format!("treemap · {shown} · color: {mode}");
        canvas.pane(at.map_pane, &title, border(pack, true));
        self.cursor.draw(&mut canvas, at.map, &ctx);

        // The panes.
        match at.panes {
            Panes::Column {
                selected, legend, ..
            } if self.facts_open => {
                // The selected pane down to the legend's last row.
                let pane = Rect::new(
                    selected.x,
                    selected.y,
                    selected.width,
                    legend.bottom().saturating_sub(selected.y),
                );
                let facts = self.facts_of(self.selected());
                panes::facts::expanded(
                    &mut canvas,
                    pane,
                    &ctx,
                    &self.sources,
                    &facts,
                    self.selected(),
                );
            }
            Panes::Column {
                selected,
                siblings,
                legend,
            } => {
                canvas.pane(selected, "selected", border(pack, false));
                panes::selected(&mut canvas, inner(selected), &ctx, self.selected());
                panes::siblings(&mut canvas, siblings, &ctx, self.selected());
                let total = self.view.node(0).size(self.opts.size);
                panes::legend(&mut canvas, legend, self.legend.0, &self.legend.1, total);
            }
            Panes::Folded { selected } => {
                canvas.pane(selected, "selected", border(pack, false));
                panes::folded(&mut canvas, inner(selected), &ctx, self.selected());
                if self.facts_open {
                    let facts = self.facts_of(self.selected());
                    panes::facts::overlay(
                        &mut canvas,
                        at.map_pane,
                        &ctx,
                        &self.sources,
                        &facts,
                        self.selected(),
                    );
                }
            }
        }

        // The keys, and the first warning where there is room.
        let message = self.warning.as_ref().map(|text| Message {
            text: text.clone(),
            kind: Notice::Warning,
        });
        let keys = if self.opts.glyphs == Glyphs::Ascii {
            &KEYS_ASCII
        } else {
            &KEYS
        };
        canvas.key_line(1, at.keys.y, at.keys.right(), keys, message.as_ref());
        canvas.cells
    }

    /// The path of `id` as the header writes the root's (`~/Library`).
    fn shown_path(&self, id: u32) -> String {
        let rel = String::from_utf8_lossy(&self.view.path(id)).into_owned();
        match (rel.is_empty(), self.root.as_str()) {
            (true, root) => root.to_owned(),
            (false, "/") => format!("/{rel}"),
            (false, root) => format!("{root}/{rel}"),
        }
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
        if self.selected() != 0 {
            let rel = String::from_utf8_lossy(&self.view.path(self.selected())).into_owned();
            segments.extend(rel.split('/').map(str::to_owned));
        }
        segments
    }

    /// What `event` does: `q` and `Ctrl-C` quit, a resize redraws, a
    /// navigation key that moves the cursor or the zoom redraws, and the
    /// rest waits for the next event.
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
            (KeyCode::Char('i'), false, false) => {
                self.facts_open = !self.facts_open;
                Flow::Redraw
            }
            // The facts are closed, then a filter cleared, before `Esc`
            // ascends.
            (KeyCode::Esc, false, false) => {
                if std::mem::take(&mut self.facts_open) || self.filter.take().is_some() {
                    Flow::Redraw
                } else {
                    self.nav(Nav::Up)
                }
            }
            (code, false, false) => Nav::of(code).map_or(Flow::Wait, |nav| self.nav(nav)),
            _ => Flow::Wait,
        }
    }

    /// Moves the cursor or the zoom as `nav` asks: a frame when either
    /// changed.
    fn nav(&mut self, nav: Nav) -> Flow {
        let ctx = Context {
            view: self.view,
            opts: &self.opts,
            class_of: &self.class_of,
            dominant: &self.dominant,
            ranks: &self.ranks,
            root: &self.root,
            root_parent: &self.root_parent,
        };
        if self.cursor.nav(&ctx, nav) {
            Flow::Redraw
        } else {
            Flow::Wait
        }
    }
}

/// The root's path as the header writes it: under `home`, `~` and the rest.
/// A home of `/`, or empty, abbreviates nothing.
fn root_label(root: &Path, home: Option<&Path>) -> String {
    let home = home.filter(|home| !home.as_os_str().is_empty() && *home != Path::new("/"));
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

/// Puts the map in pixels at `x`, `y` of `buf`: each cell's text, one
/// column wide whatever it holds, in its colour; the cells a picture
/// drawn from an earlier one covers are left out of the frame's diff.
fn overlay(cells: &PixelMap, x: u16, y: u16, buf: &mut Buffer) {
    for dy in 0..cells.height() {
        for dx in 0..cells.width() {
            let Some(target) = buf.cell_mut((x + dx, y + dy)) else {
                continue;
            };
            match cells.get(dx, dy) {
                Some(cell) => {
                    target.reset();
                    target.set_symbol(&cell.symbol);
                    if let Some((r, g, b)) = cell.fg {
                        target.set_fg(Shown::Rgb(r, g, b));
                    }
                    target.set_diff_option(CellDiffOption::ForcedWidth(NonZeroU16::MIN));
                }
                None => {
                    target.set_diff_option(CellDiffOption::Skip);
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

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::root_label;

    #[test]
    fn p4_tui_02_the_root_is_written_from_home() {
        let home = Some(Path::new("/Users/alex"));
        assert_eq!(root_label(Path::new("/Users/alex"), home), "~");
        assert_eq!(
            root_label(Path::new("/Users/alex/Library"), home),
            "~/Library"
        );
        assert_eq!(root_label(Path::new("/Users"), home), "/Users");
        assert_eq!(root_label(Path::new("/var/log"), None), "/var/log");
        // A home of `/` or empty abbreviates nothing.
        assert_eq!(root_label(Path::new("/usr"), Some(Path::new("/"))), "/usr");
        assert_eq!(root_label(Path::new("/usr"), Some(Path::new(""))), "/usr");
    }
}
