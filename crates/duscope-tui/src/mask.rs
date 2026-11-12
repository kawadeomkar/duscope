//! The mask: what `/` (a glob) or `⏎` on a legend or types row leaves on
//! the map, the rest not painted, without a new layout, as
//! `GrandPerspective`'s mask draws it.
//!
//! A glob is matched as `--glob` matches, against the path relative to the
//! scan's root (`*.zip` at any depth, `/Library/*` from the root): a tile
//! shows when its entry's path or a folder above it matches. A folder too
//! small to open, a pool (`+N small`) and the files' tile show when
//! something drawn in them would: their members are read, each child a
//! pool steps through and each entry under a folder counted, at most
//! [`WALK_BUDGET`] entries for a map however large its folders, and a tile
//! the budget runs out in, like whatever it does not reach, stays shown.
//! Reading a tile's entry is O(depth), so a map costs its tiles and the
//! budget, once per mask and map, and repainting costs a pass over the
//! cells. A list (the siblings pane, the list view) reads each of its rows
//! and, below its folders, at most [`WALK_BUDGET`] entries for the whole
//! list, a folder the budget does not reach the end of shown.
//!
//! The boxes and gutters of open folders are structure, drawn whatever the
//! mask leaves; what the mask hides takes the base colour, half a cell at a
//! time where the half blocks allow it.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use duscope_core::query::{Filters, Node, globs};
use duscope_core::rules::Class;
use duscope_treemap::Rect;
use duscope_treemap::cells::Cell;
use duscope_treemap::paint::{DrawnKind, Glyphs};
use ratatui_core::layout::Rect as Area;

use crate::panes::{Context, legend_label};
use crate::widgets::Canvas;
use crate::zoom::Map;

/// The entries read for one map, or one list, while deciding what its
/// folders hold.
pub(crate) const WALK_BUDGET: usize = 256 * 1024;
/// The maps whose hidden tiles are kept.
const KEPT: usize = 32;
/// A map's cell marked hidden in its top half, and in its bottom half.
const TOP: u8 = 1;
const BOTTOM: u8 = 2;

/// What decides an entry.
#[derive(Debug, Clone)]
enum Rule {
    /// `/`: its path, or a folder above it, matches the glob.
    Glob(Box<Filters>),
    /// `⏎` on a legend row: it counts under that row.
    Legend(String),
    /// `⏎` on a types row: a file of that class.
    Class(Class),
}

/// A mask on the map, and what it hides on each map painted.
#[derive(Debug)]
pub struct Mask {
    rule: Rule,
    hidden: RefCell<HashMap<(u32, u16, u16), Rc<Hidden>>>,
}

/// What a mask hides on one map.
#[derive(Debug)]
pub struct Hidden {
    /// By tile index: whether the mask hides it.
    tiles: Vec<bool>,
    /// By cell: [`TOP`] and [`BOTTOM`] for the halves hidden.
    cells: Vec<u8>,
    width: u16,
    height: u16,
}

impl Hidden {
    /// Whether the mask hides the tile at this index of [`Map::tiles`].
    #[must_use]
    pub fn tile(&self, index: usize) -> bool {
        self.tiles.get(index).copied().unwrap_or(false)
    }
}

impl Mask {
    /// The mask of `pattern`, a glob as `--glob` takes it; the error says
    /// why it is not one.
    pub fn glob(pattern: &str) -> Result<Mask, String> {
        let set = globs([pattern])?;
        // `Filters` matches the path and the folders above it for
        // `--exclude`: what a tile needs here, read as "keeps".
        let filters = Filters {
            exclude: Some(set),
            ..Filters::default()
        };
        Ok(Mask::new(Rule::Glob(Box::new(filters))))
    }

    /// The mask of the legend row `label`.
    #[must_use]
    pub fn legend(label: String) -> Mask {
        Mask::new(Rule::Legend(label))
    }

    /// The mask of `class`.
    #[must_use]
    pub fn class(class: Class) -> Mask {
        Mask::new(Rule::Class(class))
    }

    /// Whether the mask is a glob `/` typed.
    #[must_use]
    pub fn is_glob(&self) -> bool {
        matches!(self.rule, Rule::Glob(_))
    }

    /// Whether the mask reads the legend's rows.
    #[must_use]
    pub fn is_legend(&self) -> bool {
        matches!(self.rule, Rule::Legend(_))
    }

    fn new(rule: Rule) -> Mask {
        Mask {
            rule,
            hidden: RefCell::new(HashMap::new()),
        }
    }

    /// Whether the rule keeps `node`, read by itself.
    #[must_use]
    pub fn keeps(&self, ctx: &Context<'_>, node: &Node) -> bool {
        match &self.rule {
            Rule::Glob(filters) => filters.excludes(ctx.view, node),
            Rule::Legend(label) => !node.is_dir() && legend_label(ctx, node) == *label,
            Rule::Class(class) => {
                !node.is_dir()
                    && ctx
                        .class_of
                        .get(node.id as usize)
                        .is_some_and(|c| c == class)
            }
        }
    }

    /// Whether `node`, a row of a list, shows: its own path, or for a
    /// folder something inside it within `budget` entries, which the whole
    /// list shares; a folder the budget does not reach the end of shows.
    #[must_use]
    pub fn shows(&self, ctx: &Context<'_>, node: &Node, budget: &mut usize) -> bool {
        self.keeps(ctx, node) || (node.is_dir() && self.below(ctx, node, budget))
    }

    /// Whether a file under `dir` is kept, within `budget` entries; out of
    /// budget counts as kept.
    fn below(&self, ctx: &Context<'_>, dir: &Node, budget: &mut usize) -> bool {
        for id in dir.id + 1..=dir.last_id {
            if *budget == 0 {
                return true;
            }
            *budget -= 1;
            let node = ctx.view.node(id);
            if !node.is_dir() && self.keeps(ctx, &node) {
                return true;
            }
        }
        false
    }

    /// Whether a leaf tile of entry `id` shows, within `budget` entries.
    fn tile_shows(&self, ctx: &Context<'_>, id: u32, budget: &mut usize) -> bool {
        let node = ctx.view.node(id);
        self.keeps(ctx, &node) || (node.is_dir() && self.below(ctx, &node, budget))
    }

    /// Whether a pool of `dir` (its children with no tile of their own on
    /// `map`, for [`DrawnKind::Files`] its files) shows, within `budget`:
    /// each child read on the way counts, as each entry `below` reads
    /// does, and a pool the budget runs out in shows, as a folder does
    /// there.
    fn pool_shows(
        &self,
        ctx: &Context<'_>,
        map: &Map,
        dir: u32,
        files_only: bool,
        budget: &mut usize,
    ) -> bool {
        let node = ctx.view.node(dir);
        // The children in id order, the files and then each folder, whose
        // subtree is stepped over (the files alone for the files' tile):
        // one read each, as they are reached.
        let last = if files_only {
            node.files_end
        } else {
            node.last_id
        };
        let mut next = dir.checked_add(1);
        while let Some(id) = next.filter(|&id| id <= last) {
            if *budget == 0 {
                return true;
            }
            *budget -= 1;
            let child = ctx.view.node(id);
            next = child.last_id.checked_add(1);
            if map.own(id).is_some() {
                continue;
            }
            if self.keeps(ctx, &child) || (child.is_dir() && self.below(ctx, &child, budget)) {
                return true;
            }
        }
        false
    }

    /// What the mask hides on `map`, worked out once.
    fn hidden(&self, ctx: &Context<'_>, map: &Map) -> Rc<Hidden> {
        let (w, h) = (map.cells.width(), map.cells.height());
        let key = (map.root(), w, h);
        if let Some(found) = self.hidden.borrow().get(&key) {
            return Rc::clone(found);
        }
        let mut budget = WALK_BUDGET;
        let tiles: Vec<bool> = map
            .tiles()
            .iter()
            .map(|tile| match tile.kind {
                DrawnKind::Leaf | DrawnKind::Box { open: false } => {
                    !self.tile_shows(ctx, tile.id, &mut budget)
                }
                DrawnKind::Small => !self.pool_shows(ctx, map, tile.id, false, &mut budget),
                DrawnKind::Files => !self.pool_shows(ctx, map, tile.id, true, &mut budget),
                DrawnKind::Box { open: true } | DrawnKind::Gutter => false,
            })
            .collect();
        let mut cells = vec![0u8; usize::from(w) * usize::from(h)];
        for (tile, _) in map
            .tiles()
            .iter()
            .zip(&tiles)
            .filter(|&(_, &hidden)| hidden)
        {
            // A box at the `--depth` cap keeps its outline and title.
            let rect = if matches!(tile.kind, DrawnKind::Box { .. }) {
                inset(tile.rect)
            } else {
                tile.rect
            };
            mark(&mut cells, rect, w, h);
        }
        let hidden = Rc::new(Hidden {
            tiles,
            cells,
            width: w,
            height: h,
        });
        let mut kept = self.hidden.borrow_mut();
        if kept.len() >= KEPT {
            kept.clear();
        }
        kept.insert(key, Rc::clone(&hidden));
        hidden
    }

    /// Hides what the mask hides on `map`, drawn into `area` of `canvas`.
    pub fn paint(&self, canvas: &mut Canvas<'_>, area: Area, ctx: &Context<'_>, map: &Map) {
        let hidden = self.hidden(ctx, map);
        let base = canvas.pack.roles.base;
        let halves = canvas.glyphs == Glyphs::Unicode && !canvas.pack.shades;
        for y in 0..hidden.height.min(area.height) {
            for x in 0..hidden.width.min(area.width) {
                let index = usize::from(y) * usize::from(hidden.width) + usize::from(x);
                let flags = hidden.cells[index];
                if flags == 0 {
                    continue;
                }
                let Some(cell) = canvas.cells.get_mut(area.x + x, area.y + y) else {
                    continue;
                };
                *cell = masked(*cell, flags, base, halves);
            }
        }
    }

    /// What the mask hides on `map`.
    #[must_use]
    pub fn hidden_tiles(&self, ctx: &Context<'_>, map: &Map) -> Rc<Hidden> {
        self.hidden(ctx, map)
    }
}

/// `rect` less its one-cell inset (a column at each side, a row, two
/// units, at each end).
fn inset(rect: Rect) -> Rect {
    Rect::new(
        rect.x + 1,
        rect.y + 2,
        rect.w.saturating_sub(2),
        rect.h.saturating_sub(4),
    )
}

/// Marks the cells of `rect` (half-cell units) hidden.
fn mark(cells: &mut [u8], rect: Rect, w: u16, h: u16) {
    let right = rect.x.saturating_add(rect.w).min(w);
    let bottom = u32::from(rect.y) + u32::from(rect.h);
    let units = u32::from(h) * 2;
    for y in u32::from(rect.y)..bottom.min(units) {
        let half = if y % 2 == 0 { TOP } else { BOTTOM };
        let row = usize::try_from(y / 2).unwrap_or(usize::MAX);
        for x in rect.x..right {
            cells[row * usize::from(w) + usize::from(x)] |= half;
        }
    }
}

/// `cell`, with the halves `flags` names not painted: the base colour in
/// their place; a cell with both halves hidden is blank, and where the
/// map's cells cannot be split in halves (`halves` false) a half keeps the
/// cell.
fn masked(cell: Cell, flags: u8, base: duscope_treemap::cells::Color, halves: bool) -> Cell {
    if flags == TOP | BOTTOM {
        return Cell {
            ch: ' ',
            bg: base,
            ..Cell::default()
        };
    }
    if !halves {
        return cell;
    }
    let half_block = cell.ch == '▀';
    let (fg, bg) = if half_block {
        (cell.fg, cell.bg)
    } else {
        (cell.bg, cell.bg)
    };
    let (fg, bg) = if flags == TOP { (base, bg) } else { (fg, base) };
    Cell {
        ch: '▀',
        fg,
        bg,
        ..Cell::default()
    }
}
