//! The cursor: where it is (the selected path and the zoom), how the
//! navigation keys move it, and how the map marks it.
//!
//! - `↑↓←→` and `hjkl` move to the neighbouring tile at the same depth by
//!   the beam rule ([`beam`]): of the tiles in the cursor's block (its
//!   siblings', and the pool's), those beyond the cursor's edge are the
//!   candidates; those overlapping the cursor on the other axis win, then
//!   the nearest edge, then the nearest centre on the other axis. With no
//!   candidate (the cursor at its block's edge) the cursor stays. A pool
//!   (`+N small`) is a tile like the others: the cursor lands on the
//!   largest entry in it.
//! - `⏎` descends, `⌫` and `Esc` ascend the path ([`crate::path`]); at the
//!   map's root `⌫` zooms out one step to reach the parent.
//! - `z` zooms to the folder under the cursor (a file's folder) and the
//!   cursor goes into it, `Z` zooms out one step and `0` all the way, each
//!   putting back the cursor `z` found ([`crate::zoom`]).
//! - `g`/`Home` and `G`/`End` jump to the first and last sibling, and
//!   `PgUp`/`PgDn` move a page of them, in the siblings pane's order
//!   (largest first); the siblings pane reaches entries too small to draw.
//!
//! The cursor is an accent outline around its tile, with an accent tag
//! carrying `name · size` on the tile's top edge when the tile is at least
//! 8×2 cells; a tile under 2×2 cells is filled with the accent instead. An
//! entry without a tile of its own is marked on the tile that holds it.
//! The borders and titles of the cursor's ancestors, where they are boxes,
//! take the link colour.
//!
//! With `--treemap pixels` the map shown is the picture, laid out in
//! pixels: the arrows move among its tiles ([`Cursor::show`]), and the
//! viewer draws the outlines into the picture ([`Cursor::marks`]).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crossterm::event::KeyCode;
use duscope_treemap::Rect;
use duscope_treemap::paint::{DrawnKind, Glyphs, compact};
use ratatui_core::layout::Rect as Area;

use crate::panes::Context;
use crate::path::{Path, siblings};
use crate::widgets::{Canvas, Span};
use crate::zoom::{Layouts, Map, Mark, Zoom};

/// The siblings `PgUp` and `PgDn` move by: the rows the siblings pane
/// lists.
const PAGE: usize = 9;

/// A direction of the spatial move.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    /// `↑`, `k`.
    Up,
    /// `↓`, `j`.
    Down,
    /// `←`, `h`.
    Left,
    /// `→`, `l`.
    Right,
}

impl Dir {
    /// The four.
    #[cfg(test)]
    pub const ALL: [Dir; 4] = [Dir::Up, Dir::Down, Dir::Left, Dir::Right];
}

/// What a navigation key asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nav {
    /// The spatial move.
    Move(Dir),
    /// `⏎`: down the path.
    Open,
    /// `⌫`, `Esc`: up the path.
    Up,
    /// `z`: zoom to the cursor.
    Zoom,
    /// `Z`: zoom out one step.
    Out,
    /// `0`: zoom out all the way.
    Reset,
    /// `g`, `Home`: the first sibling.
    First,
    /// `G`, `End`: the last sibling.
    Last,
    /// `PgUp`: a page of siblings back.
    PageUp,
    /// `PgDn`: a page of siblings on.
    PageDown,
}

impl Nav {
    /// The navigation `code` asks for, without Ctrl or Alt; `Esc` is the
    /// viewer's, which clears a filter first.
    #[must_use]
    pub fn of(code: KeyCode) -> Option<Nav> {
        Some(match code {
            KeyCode::Up | KeyCode::Char('k') => Nav::Move(Dir::Up),
            KeyCode::Down | KeyCode::Char('j') => Nav::Move(Dir::Down),
            KeyCode::Left | KeyCode::Char('h') => Nav::Move(Dir::Left),
            KeyCode::Right | KeyCode::Char('l') => Nav::Move(Dir::Right),
            KeyCode::Enter => Nav::Open,
            KeyCode::Backspace => Nav::Up,
            KeyCode::Char('z') => Nav::Zoom,
            KeyCode::Char('Z') => Nav::Out,
            KeyCode::Char('0') => Nav::Reset,
            KeyCode::Char('g') | KeyCode::Home => Nav::First,
            KeyCode::Char('G') | KeyCode::End => Nav::Last,
            KeyCode::PageUp => Nav::PageUp,
            KeyCode::PageDown => Nav::PageDown,
            _ => return None,
        })
    }
}

/// The beam rule: where the cursor on `from` goes moving `dir`, of the
/// `candidates` (an index and a rectangle each, in half-cell units): the
/// candidates lie wholly beyond `from`'s edge in `dir`; one overlapping
/// `from` on the other axis wins over one that does not, then the one
/// whose near edge is nearest, then the one whose centre on the other axis
/// is nearest `from`'s, then the first. `None`, and the cursor stays, when
/// nothing lies beyond the edge. Empty rectangles are not drawn and are no
/// candidates.
#[must_use]
pub fn beam(
    from: Rect,
    dir: Dir,
    candidates: impl IntoIterator<Item = (usize, Rect)>,
) -> Option<usize> {
    // Along the move and across it, as (start, end) spans.
    let spans = |r: Rect| -> ((u32, u32), (u32, u32)) {
        let x = (u32::from(r.x), u32::from(r.x) + u32::from(r.w));
        let y = (u32::from(r.y), u32::from(r.y) + u32::from(r.h));
        match dir {
            Dir::Left | Dir::Right => (x, y),
            Dir::Up | Dir::Down => (y, x),
        }
    };
    let (along, across) = spans(from);
    let mut best: Option<((bool, u32, u32), usize)> = None;
    for (i, rect) in candidates {
        if rect.w == 0 || rect.h == 0 {
            continue;
        }
        let (c_along, c_across) = spans(rect);
        // How far its near edge is past the cursor's edge.
        let gap = match dir {
            Dir::Right | Dir::Down => c_along.0.checked_sub(along.1),
            Dir::Left | Dir::Up => along.0.checked_sub(c_along.1),
        };
        let Some(gap) = gap else { continue };
        let overlaps = c_across.0 < across.1 && across.0 < c_across.1;
        // Centres doubled, to stay whole.
        let off = (c_across.0 + c_across.1).abs_diff(across.0 + across.1);
        let key = (!overlaps, gap, off);
        if best.is_none_or(|(k, _)| key < k) {
            best = Some((key, i));
        }
    }
    best.map(|(_, i)| i)
}

/// The cursor: the selected path, the zoom, and the maps painted for it.
#[derive(Debug, Default)]
pub struct Cursor {
    path: Path,
    zoom: Zoom,
    /// The maps painted, one per root at the map's size.
    layouts: RefCell<Layouts>,
    /// The map's size at the last frame, which the keys move on.
    size: Cell<(u16, u16)>,
    /// With `--treemap pixels`, the picture's tiles at the last frame: the
    /// map shown, which the arrows move on.
    shown: RefCell<Option<Rc<Map>>>,
}

impl Cursor {
    /// The cursor on `path`, the map unzoomed.
    #[must_use]
    pub fn new(path: Path) -> Cursor {
        Cursor {
            path,
            ..Cursor::default()
        }
    }

    /// The zoom.
    #[must_use]
    pub fn zoom(&self) -> &Zoom {
        &self.zoom
    }

    /// The entry under the cursor.
    #[must_use]
    pub fn selected(&self) -> u32 {
        self.path.cursor()
    }

    /// The map of the visible root at the size of the last frame, painted
    /// once and kept.
    #[must_use]
    pub fn map(&self, ctx: &Context<'_>) -> Rc<Map> {
        let (w, h) = self.size.get();
        let root = self.zoom.root();
        self.layouts
            .borrow_mut()
            .get(root, w, h, || Map::paint(ctx, root, w, h))
    }

    /// How many maps are kept.
    #[cfg(test)]
    pub fn kept(&self) -> usize {
        self.layouts.borrow().kept()
    }

    /// Shows the cursor on `map` from now on, the tiles of the picture
    /// drawn over the cells (`--treemap pixels`), which the arrows then
    /// move among; `None` goes back to the cell map.
    pub fn show(&self, map: Option<Rc<Map>>) {
        *self.shown.borrow_mut() = map;
    }

    /// The map the arrows move on: the picture shown, where it is of the
    /// visible root, else the cell map.
    fn moves_on(&self, ctx: &Context<'_>) -> Rc<Map> {
        match self.shown.borrow().as_ref() {
            Some(map) if map.root() == self.zoom.root() => Rc::clone(map),
            _ => self.map(ctx),
        }
    }

    /// Puts the cursor on `id`, under the map's root, the path ending
    /// there.
    #[cfg(test)]
    pub fn jump(&mut self, view: &dyn duscope_core::query::TreeView, id: u32) {
        self.path = Path::to(view, id);
    }

    /// Does what `nav` asks; true when the cursor or the zoom changed.
    pub fn nav(&mut self, ctx: &Context<'_>, nav: Nav) -> bool {
        let view = ctx.view;
        if view.is_empty() {
            return false;
        }
        let size = ctx.opts.size;
        let cursor = self.path.cursor();
        let at_root = cursor == self.zoom.root();
        match nav {
            Nav::Move(dir) => !at_root && self.spatial(ctx, dir),
            Nav::Open => self.path.down(view, size),
            Nav::Up => {
                // At the map's root its parent is outside the map: zoom out
                // to reach it.
                if at_root && self.zoom.pop().is_none() {
                    return false;
                }
                self.path.up()
            }
            Nav::Zoom => {
                let node = view.node(cursor);
                let target = match (node.is_dir(), self.path.parent()) {
                    (true, _) => cursor,
                    (false, Some(parent)) => parent,
                    (false, None) => return false,
                };
                if target == self.zoom.root() {
                    return false;
                }
                self.zoom.push(target, cursor);
                if target == cursor {
                    self.path.down(view, size);
                }
                true
            }
            Nav::Out | Nav::Reset => {
                let back = if nav == Nav::Out {
                    self.zoom.pop()
                } else {
                    self.zoom.reset()
                };
                let Some((_, cursor)) = back else {
                    return false;
                };
                self.path.jump(view, cursor);
                true
            }
            Nav::First | Nav::Last | Nav::PageUp | Nav::PageDown => {
                let Some(parent) = self.path.parent().filter(|_| !at_root) else {
                    return false;
                };
                let list = siblings(view, parent, size);
                let Some(end) = list.len().checked_sub(1) else {
                    return false;
                };
                let i = list.iter().position(|&id| id == cursor).unwrap_or(0);
                let j = match nav {
                    Nav::First => 0,
                    Nav::Last => end,
                    Nav::PageUp => i.saturating_sub(PAGE),
                    _ => (i + PAGE).min(end),
                };
                if list[j] == cursor {
                    return false;
                }
                self.path.select(list[j]);
                true
            }
        }
    }

    /// The spatial move: by the beam rule among the tiles of the cursor's
    /// block; true when the cursor moved.
    fn spatial(&mut self, ctx: &Context<'_>, dir: Dir) -> bool {
        let Some(parent) = self.path.parent() else {
            return false;
        };
        let map = self.moves_on(ctx);
        let view = ctx.view;
        let cursor = self.path.cursor();
        let mark = map.mark(view, cursor);
        let from = map.rect(mark);
        let candidates = map
            .block(parent)
            .iter()
            .filter(|&&i| mark != Mark::Tile(i))
            .map(|&i| (i, map.tiles()[i].rect));
        let Some(to) = beam(from, dir, candidates) else {
            return false;
        };
        // A tile of its own is its entry's; a pool's is the largest entry
        // marked on it.
        let tile = map.tiles()[to];
        let landing = match tile.kind {
            DrawnKind::Leaf | DrawnKind::Box { .. } | DrawnKind::Gutter => Some(tile.id),
            DrawnKind::Small | DrawnKind::Files => siblings(view, parent, ctx.opts.size)
                .into_iter()
                .find(|&id| map.mark(view, id) == Mark::Tile(to)),
        };
        match landing {
            Some(id) if id != cursor => {
                self.path.select(id);
                true
            }
            _ => false,
        }
    }

    /// Draws the map into `area` of `canvas` (the map's size from now on),
    /// then the link colour on the cursor's ancestors and the cursor.
    pub fn draw(&self, canvas: &mut Canvas<'_>, area: Area, ctx: &Context<'_>) {
        self.size.set((area.width, area.height));
        let map = self.map(ctx);
        for y in 0..map.cells.height().min(area.height) {
            for x in 0..map.cells.width().min(area.width) {
                if let (Some(&cell), Some(slot)) = (
                    map.cells.get(x, y),
                    canvas.cells.get_mut(area.x + x, area.y + y),
                ) {
                    *slot = cell;
                }
            }
        }
        let view = ctx.view;
        for i in self.linked(&map) {
            let (x, y, w, h) = whole_cells(map.tiles()[i].rect);
            link(canvas, area, (x, y, w, h));
        }
        if view.is_empty() {
            return;
        }
        let cursor = self.path.cursor();
        let node = view.node(cursor);
        let name = String::from_utf8_lossy(&view.name(cursor)).into_owned();
        let tag = (name, compact(node.size(ctx.opts.size)));
        mark(canvas, area, map.rect(map.mark(view, cursor)), &tag);
    }

    /// The tiles of `map` that take the link colour: the cursor's
    /// ancestors below the map's root that are boxes, outermost first.
    fn linked(&self, map: &Map) -> Vec<usize> {
        let root = self.zoom.root();
        let ancestors = self.path.ancestors();
        let below = ancestors
            .iter()
            .position(|&id| id == root)
            .map_or(ancestors.len(), |i| i + 1);
        ancestors[below..]
            .iter()
            .filter_map(|&id| map.own(id))
            .filter(|&i| matches!(map.tiles()[i].kind, DrawnKind::Box { .. }))
            .collect()
    }

    /// The cursor's marks on `map` (a picture's tiles, in pixels): the
    /// rectangle of the tile it is marked on, and those of its ancestors
    /// that take the link colour, outermost first.
    #[must_use]
    pub fn marks(&self, map: &Map, view: &dyn duscope_core::query::TreeView) -> (Rect, Vec<Rect>) {
        let links = self
            .linked(map)
            .into_iter()
            .map(|i| map.tiles()[i].rect)
            .collect();
        let cursor = if view.is_empty() {
            map.area()
        } else {
            map.rect(map.mark(view, self.path.cursor()))
        };
        (cursor, links)
    }
}

/// The cells inside `rect` (half-cell units) a box's outline is drawn on:
/// the whole rows, as the map outlines a folder.
fn whole_cells(rect: Rect) -> (u16, u16, u16, u16) {
    let top = rect.y.div_ceil(2);
    let bottom = rect.y.midpoint(rect.h);
    (rect.x, top, rect.w, bottom.saturating_sub(top))
}

/// The cells the cursor on `rect` is drawn on: the whole rows inside it
/// when there are two, else every row it touches.
fn cursor_cells(rect: Rect) -> (u16, u16, u16, u16) {
    let whole = whole_cells(rect);
    if whole.3 >= 2 {
        return whole;
    }
    let top = rect.y / 2;
    let bottom = (u32::from(rect.y) + u32::from(rect.h)).div_ceil(2);
    let rows = u16::try_from(bottom)
        .unwrap_or(u16::MAX)
        .saturating_sub(top);
    (rect.x, top, rect.w, rows)
}

/// Turns the border of the box on cells `(x, y, w, h)` of the map in
/// `area`, its title with it, to the link colour.
fn link(canvas: &mut Canvas<'_>, area: Area, (x, y, w, h): (u16, u16, u16, u16)) {
    if w == 0 || h == 0 {
        return;
    }
    let color = canvas.pack.roles.link;
    let (right, bottom) = (x + w - 1, y + h - 1);
    let edge = (x..=right)
        .flat_map(|cx| [(cx, y), (cx, bottom)])
        .chain((y..=bottom).flat_map(|cy| [(x, cy), (right, cy)]));
    for (cx, cy) in edge {
        if cx < area.width
            && cy < area.height
            && let Some(cell) = canvas.cells.get_mut(area.x + cx, area.y + cy)
        {
            cell.fg = color;
        }
    }
}

/// Marks the cursor on `rect` (half-cell units) of the map in `area`: an
/// accent outline, and on a tile at least 8×2 cells the `tag` (a name and
/// a size) on its top edge; on a tile under 2×2 cells, the accent filling
/// the cells it touches.
fn mark(canvas: &mut Canvas<'_>, area: Area, rect: Rect, tag: &(String, String)) {
    let (x, y, w, h) = cursor_cells(rect);
    let (x, y) = (x.min(area.width), y.min(area.height));
    let (w, h) = (
        w.min(area.width.saturating_sub(x)),
        h.min(area.height.saturating_sub(y)),
    );
    if w == 0 || h == 0 {
        return;
    }
    let roles = canvas.pack.roles;
    let ascii = canvas.glyphs == Glyphs::Ascii;
    let (x, y) = (area.x + x, area.y + y);
    let put = |canvas: &mut Canvas<'_>, cx: u16, cy: u16, ch: char| {
        if let Some(cell) = canvas.cells.get_mut(cx, cy) {
            *cell = duscope_treemap::cells::Cell {
                ch,
                fg: roles.accent,
                bg: roles.base,
                ..duscope_treemap::cells::Cell::default()
            };
        }
    };
    if w < 2 || h < 2 {
        let block = if ascii { '#' } else { '█' };
        for cy in y..y + h {
            for cx in x..x + w {
                put(canvas, cx, cy, block);
            }
        }
        return;
    }
    let (hz, vt, corners) = if ascii {
        ('=', '#', ['#', '#', '#', '#'])
    } else {
        ('━', '┃', ['┏', '┓', '┗', '┛'])
    };
    let (right, bottom) = (x + w - 1, y + h - 1);
    for cx in x + 1..right {
        put(canvas, cx, y, hz);
        put(canvas, cx, bottom, hz);
    }
    for cy in y + 1..bottom {
        put(canvas, x, cy, vt);
        put(canvas, right, cy, vt);
    }
    put(canvas, x, y, corners[0]);
    put(canvas, right, y, corners[1]);
    put(canvas, x, bottom, corners[2]);
    put(canvas, right, bottom, corners[3]);
    if w < 8 {
        return;
    }
    // ` name · size ` between the corners: the name alone, then cut,
    // when the whole does not fit.
    let room = w - 4;
    let (name, size) = tag;
    let dot = if ascii { "-" } else { "·" };
    let full = format!("{name} {dot} {size}");
    let text = if canvas.width(&full) <= room {
        full
    } else {
        canvas.cut(name, room)
    };
    let label = format!(" {text} ");
    let width = canvas.width(&label);
    canvas.fill(Area::new(x + 1, y, width, 1), roles.accent);
    canvas.spans(x + 1, y, right, &[Span::bold(label, roles.crust)]);
}

#[cfg(test)]
pub(crate) mod tests;
