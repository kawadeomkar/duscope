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
//! Every key goes through the key map ([`crate::keys`]) to an
//! [`Action`](crate::keys::Action) here. `1`–`4` swap the map for a table
//! ([`crate::views`]); `⇥` moves the focus between the map (or table), the
//! siblings and the legend, and the arrows act in the pane in focus; `s`
//! sorts the siblings and the list; `t` labels the map's tiles; `c` and
//! `p` cycle the colour mode and the pack, forgetting the maps kept (the
//! pictures too with `--treemap pixels`, and the rest of the viewer's work
//! stays); `/` and `⏎` on a legend or types row mask the map
//! ([`crate::mask`]), and with a mask on the map is drawn in cells.
//! `?` lists the key map over the panes; `y` copies the selection's path
//! with OSC 52 where the terminal takes it, the path on the key line
//! either way; `o` shows the selection in the file manager, selected in
//! the folder that holds it, never opened.
//!
//! With `--treemap pixels` the map pane shows a picture over its cells: of
//! the zoom's root, in the whole map's colours, drawn once per root and
//! size and kept ([`crate::zoom::Layouts`]), with the cursor's marks drawn
//! into it and encoded again whenever they move.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::io;
use std::num::NonZeroU16;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::rc::Rc;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use duscope_core::envelope::base64;
use duscope_core::query::{Subtree, TreeView, abs_path};
use duscope_core::rules::Class;
use duscope_treemap::Rect as Px;
use duscope_treemap::cells::{CellBuffer, Color, Depth};
use duscope_treemap::packs::Pack;
use duscope_treemap::paint::{ColorBy, Glyphs, LsColors, PaintOpts, classes_of};
use duscope_treemap::pixels::{Picture, PixelMap, Pixels};
use ratatui_core::backend::Backend;
use ratatui_core::buffer::{Buffer, CellDiffOption};
use ratatui_core::layout::Rect;
use ratatui_core::style::{Color as Shown, Modifier};
use ratatui_core::terminal::Terminal;
use unicode_width::UnicodeWidthChar;

use crate::cursor::{Cursor, Dir, Nav, PAGE, Step};
use crate::keys::{self, Action};
use crate::layout::{Panes, inner, layout};
use crate::mask::Mask;
use crate::panes::{self, Context, Facts, LegendRow, Ranks, Sources};
use crate::path::Path as Selection;
use crate::term::MIN_SIZE;
use crate::views::{self, Focus, Rows, Sort, Tab, Viewing};
use crate::widgets::{Canvas, Message, Notice, Span, border};
use crate::zoom::{self, KEPT_PICTURES, Layouts, Map};

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

/// Writes to the terminal, between frames.
pub type TermWrite<'a> = Box<dyn FnMut(&[u8]) -> io::Result<()> + 'a>;
/// Shows a path in the desktop's file manager.
pub type Opener<'a> = Box<dyn FnMut(&Path) -> io::Result<()> + 'a>;

/// What the viewer does outside its cells: write to the terminal between
/// frames, show a path in the file manager, read the variables that tell
/// what the terminal allows. A test replaces them; the binary wires them to
/// the terminal and the file manager.
pub struct Effects<'a> {
    /// Writes to the terminal, between frames (OSC 52).
    pub write: TermWrite<'a>,
    /// Shows a path in the file manager (reveals it, never opens it),
    /// detached.
    pub open: Opener<'a>,
    /// Reads a variable: the terminal's name, and tmux's and screen's marks.
    pub env: panes::facts::Env<'a>,
}

impl std::fmt::Debug for Effects<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Effects").finish_non_exhaustive()
    }
}

impl Default for Effects<'_> {
    /// Nothing happens: what a test's viewer does unless told otherwise.
    fn default() -> Self {
        Effects {
            write: Box::new(|_| Ok(())),
            open: Box::new(|_| Ok(())),
            env: Box::new(|_| None),
        }
    }
}

/// A key line in place of the keys, until the next key: the path `y` copied
/// or, where the terminal takes no OSC 52, the path to read.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PathLine {
    /// Whether OSC 52 carried it to the clipboard.
    copied: bool,
    /// The path.
    text: String,
}

/// The viewer.
// Independent settings the keys turn on and off.
#[allow(clippy::struct_excessive_bools)]
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
    /// The tab: the map or a table.
    tab: Tab,
    /// The pane the arrows act in: the map or a table, the siblings, the
    /// legend.
    focus: Focus,
    /// The sort of the siblings and the list.
    sort: Sort,
    /// The tables' rows, worked out once.
    rows: Rows,
    /// `t`: labels on the map's tiles.
    labels: bool,
    /// The filter's text as the map's title writes it, which `Esc` clears
    /// before it ascends.
    pub(crate) filter: Option<String>,
    /// The mask the filter is: `/`'s glob, or a legend or types row.
    mask: Option<Mask>,
    /// `/`: the prompt's text while it is open.
    prompt: Option<String>,
    /// `?`: the key map over the panes.
    help: bool,
    /// The legend row the cursor is on when the legend is focused.
    legend_at: usize,
    /// The types row the cursor is on.
    types_at: usize,
    /// `i`: the selected pane grown into the full fact list, which `Esc`
    /// closes before anything else.
    facts_open: bool,
    /// A message on the key line until the next key.
    notice: Option<Message>,
    /// The path `y` puts on the key line, until the next key.
    path_line: Option<PathLine>,
    /// The packs `p` cycles through, this viewer's first.
    packs: &'a [Pack],
    /// `LS_COLORS`, for the colour mode that reads it.
    ls_colors: Option<LsColors>,
    /// What the viewer does outside its cells.
    effects: Effects<'a>,
    /// The screen's size at the last frame, which the keys read.
    last: Cell<(u16, u16)>,
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

/// The viewer's context and its `Viewing`, read from `$app`'s fields apart
/// from its cursor, so that a move may borrow the cursor mutably.
macro_rules! fields {
    ($app:ident) => {
        (
            Context {
                view: $app.view,
                opts: &$app.opts,
                class_of: &$app.class_of,
                dominant: &$app.dominant,
                ranks: &$app.ranks,
                root: &$app.root,
                root_parent: &$app.root_parent,
            },
            Viewing {
                sort: $app.sort,
                mask: $app.mask.as_ref(),
                filter: $app.filter.as_deref(),
                rows: &$app.rows,
            },
        )
    };
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
            tab: Tab::default(),
            focus: Focus::default(),
            sort: Sort::default(),
            rows: Rows::default(),
            labels: false,
            filter: None,
            mask: None,
            prompt: None,
            help: false,
            legend_at: 0,
            types_at: 0,
            facts_open: false,
            notice: None,
            path_line: None,
            packs: &[],
            ls_colors: None,
            effects: Effects::default(),
            last: Cell::new((0, 0)),
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

    /// The packs `p` cycles through, this viewer's first; with fewer than
    /// two `p` does nothing.
    pub fn with_packs(&mut self, packs: &'a [Pack]) {
        self.packs = packs;
    }

    /// `LS_COLORS` as the terminal gives it, for the colour mode that
    /// reads it (`c`); without it that mode is the class colours.
    pub fn with_ls_colors(&mut self, table: LsColors) {
        self.ls_colors = Some(table);
    }

    /// What the viewer does outside its cells.
    pub fn with_effects(&mut self, effects: Effects<'a>) {
        self.effects = effects;
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

    /// The sort, the mask and the rows the keys have set.
    fn viewing(&self) -> Viewing<'_> {
        Viewing {
            sort: self.sort,
            mask: self.mask.as_ref(),
            filter: self.filter.as_deref(),
            rows: &self.rows,
        }
    }

    /// Whether the layout at the last frame's size has a right column,
    /// whose panes the focus can reach.
    fn has_column(&self) -> bool {
        let (w, h) = self.last.get();
        matches!(layout(w, h).panes, Panes::Column { .. })
    }

    /// The pane the keys act in: the focus, unless the siblings and the
    /// legend are not drawn, the layout folding them away or the facts
    /// covering them; it comes back with them.
    fn focus(&self) -> Focus {
        if self.has_column() && !self.facts_open {
            self.focus
        } else {
            Focus::Map
        }
    }

    /// Draws the viewer into `area` of `buf`: the frame, or below
    /// [`MIN_SIZE`] (a terminal made smaller while the viewer runs) one
    /// line asking for more room.
    pub fn draw(&self, area: Rect, buf: &mut Buffer) {
        self.last.set((area.width, area.height));
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
        // The facts over the map, a table in its place, the help cover its
        // picture; a mask is drawn on the cells.
        let covered = self.help
            || self.tab != Tab::Treemap
            || self.mask.is_some()
            || (self.facts_open
                && matches!(layout(area.width, area.height).panes, Panes::Folded { .. }));
        if self.mask.is_some() {
            self.cursor.show(None);
        }
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
        let viewing = self.viewing();
        let root_id = self.cursor.zoom().root();

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
        x = canvas.tabs(x, y, end, self.tab.index());
        canvas.status(x.saturating_add(1), y, end, &self.summary);

        // The breadcrumbs, from the root's path to the selection.
        canvas.crumbs(0, at.crumbs.y, at.crumbs.right(), &self.crumbs());

        self.draw_map_or_table(&mut canvas, &at, &ctx, &viewing, root_id);
        let focus = self.focus();

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
                panes::siblings(
                    &mut canvas,
                    siblings,
                    &ctx,
                    self.selected(),
                    &viewing,
                    focus == Focus::Siblings,
                );
                let total = self.view.node(0).size(self.opts.size);
                panes::legend(
                    &mut canvas,
                    legend,
                    self.legend.0,
                    &self.legend.1,
                    total,
                    (focus == Focus::Legend).then_some(self.legend_row()),
                );
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

        if self.help {
            let pane = Rect::new(0, at.map_pane.y, w, h.saturating_sub(3));
            views::help::draw(&mut canvas, pane, self.opts.glyphs == Glyphs::Ascii);
        }
        self.draw_key_row(&mut canvas, at.keys.y, at.keys.right(), focus);
        canvas.cells
    }

    /// The map in its pane with its title, or the tab's table in its
    /// place.
    fn draw_map_or_table(
        &self,
        canvas: &mut Canvas<'_>,
        at: &crate::layout::Layout,
        ctx: &Context<'_>,
        viewing: &Viewing<'_>,
        root: u32,
    ) {
        match self.tab {
            Tab::Treemap => {
                let mode = match &self.opts.color_by {
                    ColorBy::Type => "file type",
                    ColorBy::Folder => "top folder",
                    ColorBy::Level => "depth",
                    ColorBy::Age => "age",
                    ColorBy::LsColors(_) => "LS_COLORS",
                };
                let tail = format!(" · color: {mode}{}", viewing.filter_suffix());
                let title = views::path_title(
                    canvas,
                    at.map_pane,
                    "treemap · ",
                    &self.shown_path(root),
                    &tail,
                );
                canvas.pane(at.map_pane, &title, border(canvas.pack, true));
                self.cursor.draw(canvas, at.map, ctx, viewing);
            }
            Tab::List => {
                views::list::draw(canvas, at.map_pane, ctx, viewing, self.selected(), root);
            }
            Tab::Largest => {
                views::largest::draw(canvas, at.map_pane, ctx, viewing, self.selected(), root);
            }
            Tab::Types => {
                views::types::draw(canvas, at.map_pane, ctx, viewing, self.types_at, root);
            }
        }
    }

    /// The last row: the prompt (a refused glob's reason beside it), the
    /// path `y` left on it, or the keys and the first message where there
    /// is room.
    fn draw_key_row(&self, canvas: &mut Canvas<'_>, y: u16, end: u16, focus: Focus) {
        let roles = canvas.pack.roles;
        if let Some(input) = &self.prompt {
            canvas.prompt(0, y, end, input);
            if let Some(message) = &self.notice {
                let text = canvas.cut(&message.text, end / 2);
                let x = end.saturating_sub(canvas.width(&text));
                canvas.message(
                    x,
                    y,
                    end,
                    &Message {
                        text,
                        kind: message.kind,
                    },
                );
            }
        } else if let Some(line) = &self.path_line {
            let (word, color) = if line.copied {
                ("copied ", roles.green)
            } else {
                ("path ", roles.overlay1)
            };
            let room = end.saturating_sub(1 + canvas.width(word));
            let text = canvas.cut_left(&line.text, room);
            canvas.spans(
                1,
                y,
                end,
                &[Span::new(word, color), Span::new(text, roles.text)],
            );
        } else {
            let message = self.notice.clone().or_else(|| {
                self.warning.as_ref().map(|text| Message {
                    text: text.clone(),
                    kind: Notice::Warning,
                })
            });
            let items = keys::line(
                self.tab,
                focus,
                self.opts.glyphs == Glyphs::Ascii,
                self.sort.word(),
                color_word(&self.opts.color_by),
            );
            canvas.key_items(1, y, end, &items, message.as_ref());
        }
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
        if alt {
            if key.code == KeyCode::Char(']') {
                self.answer = true;
            }
            return Flow::Wait;
        }
        if self.prompt.is_some() {
            return self.prompt_key(key.code, ctrl);
        }
        // Any key clears what the last one left on the key line.
        let cleared = self.notice.take().is_some() | self.path_line.take().is_some();
        let Some(action) = keys::lookup(key) else {
            return if cleared { Flow::Redraw } else { Flow::Wait };
        };
        let flow = if self.help {
            match action {
                Action::Quit => Flow::Quit,
                Action::Help | Action::Back => {
                    self.help = false;
                    Flow::Redraw
                }
                _ => Flow::Wait,
            }
        } else {
            self.act(action)
        };
        if cleared && flow == Flow::Wait {
            Flow::Redraw
        } else {
            flow
        }
    }

    /// Does what `action` asks.
    fn act(&mut self, action: Action) -> Flow {
        match action {
            Action::Quit => Flow::Quit,
            Action::Facts => {
                self.facts_open = !self.facts_open;
                Flow::Redraw
            }
            // The facts are closed, then a filter cleared, before `Esc`
            // ascends.
            Action::Back => {
                if std::mem::take(&mut self.facts_open) || self.clear_filter() {
                    Flow::Redraw
                } else {
                    self.nav(Nav::Up)
                }
            }
            Action::Nav(nav) => self.nav(nav),
            Action::View(tab) => {
                if self.tab == tab {
                    Flow::Wait
                } else {
                    self.tab = tab;
                    Flow::Redraw
                }
            }
            Action::Sort => {
                self.sort = self.sort.next();
                Flow::Redraw
            }
            Action::Labels => {
                self.labels = !self.labels;
                self.cursor.set_labels(self.labels);
                Flow::Redraw
            }
            Action::Color => self.cycle_color(),
            Action::Pack => self.cycle_pack(),
            Action::Filter => {
                let text = self
                    .mask
                    .as_ref()
                    .filter(|mask| mask.is_glob())
                    .and(self.filter.clone())
                    .unwrap_or_default();
                self.prompt = Some(text);
                Flow::Redraw
            }
            Action::FocusNext => self.step_focus(false),
            Action::FocusPrev => self.step_focus(true),
            Action::Help => {
                self.help = true;
                Flow::Redraw
            }
            Action::Copy => self.copy(),
            Action::Open => self.open_selection(),
        }
    }

    /// The prompt's keys: text, `⌫` to delete, `⏎` to mask, `Esc` to leave
    /// the mask as it was. `Ctrl-C` quits as everywhere.
    fn prompt_key(&mut self, code: KeyCode, ctrl: bool) -> Flow {
        self.notice = None;
        match (code, ctrl) {
            (KeyCode::Char('c'), true) => Flow::Quit,
            (KeyCode::Esc, _) => {
                self.prompt = None;
                Flow::Redraw
            }
            (KeyCode::Enter, _) => self.apply_prompt(),
            (KeyCode::Backspace, _) => {
                if let Some(text) = &mut self.prompt {
                    text.pop();
                }
                Flow::Redraw
            }
            (KeyCode::Char(c), false) => {
                if let Some(text) = &mut self.prompt {
                    text.push(c);
                }
                Flow::Redraw
            }
            _ => Flow::Wait,
        }
    }

    /// Masks the map with the prompt's glob: an empty one clears the mask,
    /// and one that is no glob stays in the prompt with why on the key
    /// line.
    fn apply_prompt(&mut self) -> Flow {
        let text = self.prompt.as_deref().unwrap_or_default().to_owned();
        if text.is_empty() {
            self.prompt = None;
            self.clear_filter();
            return Flow::Redraw;
        }
        match Mask::glob(&text) {
            Ok(mask) => {
                self.prompt = None;
                self.set_mask(mask, text);
            }
            Err(why) => {
                self.notice = Some(Message {
                    text: format!("not a filter: {why}"),
                    kind: Notice::Warning,
                });
            }
        }
        Flow::Redraw
    }

    /// Puts `mask` on the map, `text` in its title.
    fn set_mask(&mut self, mask: Mask, text: String) {
        self.mask = Some(mask);
        self.filter = Some(text);
        self.rows.forget();
    }

    /// Clears the mask; true when one was on.
    fn clear_filter(&mut self) -> bool {
        let had_text = self.filter.take().is_some();
        let had_mask = self.mask.take().is_some();
        if had_mask {
            self.rows.forget();
        }
        had_text || had_mask
    }

    /// The legend rows the viewer can reach (the pane's rows, less `… n
    /// more`): none where the column is folded away.
    fn legend_shown(&self) -> usize {
        let (w, h) = self.last.get();
        match layout(w, h).panes {
            Panes::Column { legend, .. } => {
                panes::shown_rows(self.legend.1.len(), usize::from(inner(legend).height))
            }
            Panes::Folded { .. } => 0,
        }
    }

    /// The legend row the cursor is on, among the rows the legend lists at
    /// the last frame's size: the last of them where a resize shortened it.
    fn legend_row(&self) -> usize {
        self.legend_at.min(self.legend_shown().saturating_sub(1))
    }

    /// `⏎` in the legend: the map keeps that row's files, a row it lists.
    fn filter_legend(&mut self) -> Flow {
        if self.legend_shown() == 0 {
            return Flow::Wait;
        }
        let Some(row) = self.legend.1.get(self.legend_row()) else {
            return Flow::Wait;
        };
        let word = color_word(&self.opts.color_by);
        let text = format!("{word}: {}", row.label);
        let mask = Mask::legend(row.label.clone());
        self.set_mask(mask, text);
        Flow::Redraw
    }

    /// `⇥` and `Shift-⇥`: the next or the previous pane, where the panes
    /// are there to focus.
    fn step_focus(&mut self, back: bool) -> Flow {
        if self.facts_open || !self.has_column() {
            return Flow::Wait;
        }
        self.focus = self.focus.next(back);
        Flow::Redraw
    }

    /// `c`: the next colour mode, the maps painted in the last one
    /// forgotten; a filter on a legend row goes with the legend it read.
    fn cycle_color(&mut self) -> Flow {
        self.opts.color_by = match &self.opts.color_by {
            ColorBy::Type => ColorBy::Folder,
            ColorBy::Folder => ColorBy::Level,
            ColorBy::Level => ColorBy::Age,
            ColorBy::Age => ColorBy::LsColors(self.ls_colors.clone().unwrap_or_default()),
            ColorBy::LsColors(_) => ColorBy::Type,
        };
        if self.mask.as_ref().is_some_and(Mask::is_legend) {
            self.clear_filter();
        }
        self.repaint();
        Flow::Redraw
    }

    /// `p`: the next pack, the maps painted in the last one forgotten;
    /// nothing without a pack to go to, or without colours to show one.
    fn cycle_pack(&mut self) -> Flow {
        if self.packs.len() < 2 || self.depth == Depth::None {
            return Flow::Wait;
        }
        let at = self
            .packs
            .iter()
            .position(|p| p.name == self.opts.pack.name);
        let next = at.map_or(0, |i| (i + 1) % self.packs.len());
        self.opts.pack = &self.packs[next];
        self.notice = Some(Message {
            text: format!("theme: {}", self.packs[next].name),
            kind: Notice::Success,
        });
        self.repaint();
        Flow::Redraw
    }

    /// Everything the colours went into: the legend and the maps (the
    /// pictures too, DEV-136).
    fn repaint(&mut self) {
        self.legend = panes::legend_rows(&self.context());
        self.legend_at = 0;
        self.cursor.forget();
        self.forget_pictures();
    }

    /// With `--treemap pixels`, forgets the pictures and what was sent:
    /// the next frame draws the map afresh.
    fn forget_pictures(&self) {
        if let Some(pixels) = &self.pixels {
            pixels.pictures.borrow_mut().clear();
            *pixels.sent.borrow_mut() = None;
        }
    }

    /// `y`: the selection's absolute path, to the clipboard through OSC 52
    /// where the terminal takes it; on the key line either way.
    fn copy(&mut self) -> Flow {
        let path = abs_path(self.view, self.selected());
        let copied =
            clipboard_allowed(&self.effects.env) && (self.effects.write)(&osc52(&path)).is_ok();
        self.path_line = Some(PathLine {
            copied,
            text: String::from_utf8_lossy(&path).into_owned(),
        });
        Flow::Redraw
    }

    /// `o`: the selection shown in the file manager, detached. A path that
    /// is not absolute is refused: it can only come from the scan's root,
    /// which a saved scan names for itself and any file may claim, and a
    /// command line would read it as an option (`-a`) or a URL.
    fn open_selection(&mut self) -> Flow {
        let path = abs_path(self.view, self.selected());
        let path = Path::new(OsStr::from_bytes(&path));
        let opened = if path.is_absolute() {
            (self.effects.open)(path)
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the scan's root is not an absolute path",
            ))
        };
        if let Err(e) = opened {
            self.notice = Some(Message {
                text: format!("could not show it: {e}"),
                kind: Notice::Warning,
            });
            return Flow::Redraw;
        }
        Flow::Wait
    }

    /// Answers a move: in the focused pane, or the map or table on the
    /// tab. Everything but the arrows, the list steps and `⏎` is the
    /// map's, whichever pane has the focus.
    fn nav(&mut self, nav: Nav) -> Flow {
        match (self.focus(), self.tab) {
            (Focus::Legend, _) => self.nav_legend(nav),
            (Focus::Siblings, _) | (_, Tab::List) => self.nav_list(nav),
            (_, Tab::Largest) => self.nav_largest(nav),
            (_, Tab::Types) => self.nav_types(nav),
            (_, Tab::Treemap) => self.nav_map(nav),
        }
    }

    /// `nav` on the map: a frame when the cursor or the zoom changed.
    fn nav_map(&mut self, nav: Nav) -> Flow {
        let (ctx, viewing) = fields!(self);
        if self.cursor.nav(&ctx, nav, &viewing) {
            Flow::Redraw
        } else {
            Flow::Wait
        }
    }

    /// The siblings pane in focus, or the list view: the arrows and the
    /// pages step along the list; the rest is the map's.
    fn nav_list(&mut self, nav: Nav) -> Flow {
        let page = if self.tab == Tab::List {
            let (w, h) = self.last.get();
            views::list::table_rows(layout(w, h).map_pane).max(1)
        } else {
            PAGE
        };
        match nav {
            Nav::Move(Dir::Left | Dir::Right) => Flow::Wait,
            Nav::Open => self.nav_map(nav),
            _ => match Step::of(nav, page) {
                Some(step) => {
                    let (ctx, viewing) = fields!(self);
                    if self.cursor.step(&ctx, &viewing, step) {
                        Flow::Redraw
                    } else {
                        Flow::Wait
                    }
                }
                None => self.nav_map(nav),
            },
        }
    }

    /// The legend in focus: the arrows pick a row and `⏎` filters by it;
    /// the rest is the map's.
    fn nav_legend(&mut self, nav: Nav) -> Flow {
        match nav {
            Nav::Open => self.filter_legend(),
            Nav::Move(Dir::Left | Dir::Right) => Flow::Wait,
            _ => match Step::of(nav, PAGE) {
                Some(step) => {
                    let count = self.legend_shown();
                    let at = (count > 0).then(|| self.legend_at.min(count - 1));
                    match step.from(at, count) {
                        Some(row) if row != self.legend_at => {
                            self.legend_at = row;
                            Flow::Redraw
                        }
                        _ => Flow::Wait,
                    }
                }
                None => self.nav_map(nav),
            },
        }
    }

    /// The largest view: the arrows and the pages move along its rows, the
    /// cursor following them; `⏎` shows the row on the map.
    fn nav_largest(&mut self, nav: Nav) -> Flow {
        match nav {
            Nav::Move(Dir::Left | Dir::Right) => Flow::Wait,
            Nav::Open => {
                self.tab = Tab::Treemap;
                Flow::Redraw
            }
            _ => match Step::of(nav, PAGE) {
                Some(step) => {
                    let ctx = self.context();
                    let viewing = self.viewing();
                    let rows = views::largest::rows(&ctx, &viewing, self.cursor.zoom().root());
                    let at = rows.iter().position(|&id| id == self.selected());
                    let Some(to) = step.from(at, rows.len()).map(|i| rows[i]) else {
                        return Flow::Wait;
                    };
                    drop((rows, viewing));
                    if self.go_to(to) {
                        Flow::Redraw
                    } else {
                        Flow::Wait
                    }
                }
                None => self.nav_map(nav),
            },
        }
    }

    /// Puts the cursor on `id`; false when it was there.
    fn go_to(&mut self, id: u32) -> bool {
        self.cursor.go_to(self.view, id)
    }

    /// The types view: the arrows and the pages pick a class; `⏎` filters
    /// the map to it and shows the map.
    fn nav_types(&mut self, nav: Nav) -> Flow {
        let ctx = self.context();
        let viewing = self.viewing();
        let rows = views::types::rows(&ctx, &viewing, self.cursor.zoom().root());
        match nav {
            Nav::Move(Dir::Left | Dir::Right) => Flow::Wait,
            Nav::Open => {
                let at = self.types_at.min(rows.len().saturating_sub(1));
                let Some(row) = rows.get(at).cloned() else {
                    return Flow::Wait;
                };
                drop((rows, viewing));
                let text = format!("type: {}", row.class.as_str());
                self.set_mask(Mask::class(row.class), text);
                self.tab = Tab::Treemap;
                Flow::Redraw
            }
            _ => {
                if let Some(step) = Step::of(nav, PAGE) {
                    let at = (!rows.is_empty()).then(|| self.types_at.min(rows.len() - 1));
                    match step.from(at, rows.len()) {
                        Some(row) if row != self.types_at => {
                            self.types_at = row;
                            Flow::Redraw
                        }
                        _ => Flow::Wait,
                    }
                } else {
                    drop((rows, viewing));
                    self.nav_map(nav)
                }
            }
        }
    }
}

/// The word the key line and a legend filter use for the colour mode.
fn color_word(color_by: &ColorBy) -> &'static str {
    match color_by {
        ColorBy::Type => "type",
        ColorBy::Folder => "folder",
        ColorBy::Level => "level",
        ColorBy::Age => "age",
        ColorBy::LsColors(_) => "lscolors",
    }
}

/// Whether the terminal takes OSC 52, as far as the variables `env` reads
/// tell: not macOS's Terminal.app, the Linux console or a dumb terminal, and
/// not tmux or screen, where an application's sequence stops by default
/// (tmux known by `TMUX` or by a `TERM` it sets, `tmux*`, which a shell
/// that dropped `TMUX` keeps). Anything else is taken to; the path is on
/// the key line either way.
pub(crate) fn clipboard_allowed(env: &panes::facts::Env<'_>) -> bool {
    let set = |name: &str| env(name).is_some();
    let value = |name: &str| {
        env(name)
            .map(|v| v.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let term = value("TERM");
    !(set("TMUX")
        || set("STY")
        || value("TERM_PROGRAM") == "Apple_Terminal"
        || term == "linux"
        || term == "dumb"
        || term.starts_with("screen")
        || term.starts_with("tmux"))
}

/// The OSC 52 sequence that sets the clipboard to `path`.
pub(crate) fn osc52(path: &[u8]) -> Vec<u8> {
    let mut out = b"\x1b]52;c;".to_vec();
    out.extend_from_slice(base64(path).as_bytes());
    out.extend_from_slice(b"\x1b\\");
    out
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
