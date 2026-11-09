//! The mouse: a click selects the deepest tile drawn under the cell (a row
//! in the siblings pane, the legend, or a table); a second click on the
//! same cell soon after zooms; the wheel walks the path. There is no drag.
//!
//! The screen is read through the layout: this module answers what lies
//! under a cell, and the viewer ([`crate::app`]) acts on it.

use std::time::{Duration, Instant};

use duscope_treemap::Rect;

use crate::mask::Hidden;
use crate::zoom::Map;

/// The time within which a second click on the same cell is a double
/// click.
pub const DOUBLE: Duration = Duration::from_millis(400);

/// What a press of the left button amounts to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Press {
    /// A click.
    Single,
    /// The second of two clicks on one cell.
    Double,
}

/// The last click, for telling a double click.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Clicks {
    last: Option<(u16, u16, Instant)>,
}

impl Clicks {
    /// A press at column `x`, row `y`, at time `now`.
    pub fn press(&mut self, x: u16, y: u16, now: Instant) -> Press {
        match self.last {
            Some((lx, ly, at))
                if lx == x && ly == y && now.saturating_duration_since(at) <= DOUBLE =>
            {
                self.last = None;
                Press::Double
            }
            _ => {
                self.last = Some((x, y, now));
                Press::Single
            }
        }
    }
}

/// Whether point `p` lies in `rect`.
fn holds(rect: Rect, (x, y): (u32, u32)) -> bool {
    let (left, top) = (u32::from(rect.x), u32::from(rect.y));
    x >= left && x < left + u32::from(rect.w) && y >= top && y < top + u32::from(rect.h)
}

/// The deepest of the tiles at a point: a tile inside another has the
/// smaller area, and among equals the last drawn is on top (parents are
/// laid out before what is inside them).
fn deepest(map: &Map, candidates: impl Iterator<Item = usize>) -> Option<usize> {
    let tiles = map.tiles();
    candidates.min_by_key(|&i| {
        let rect = tiles[i].rect;
        (
            u32::from(rect.w) * u32::from(rect.h),
            usize::MAX - i, // the later drawn first
        )
    })
}

/// The tile of `map` under the cell at column `x`, row `y` of the map, the
/// deepest of those drawn there; what `hidden` hides is not drawn. With
/// `--treemap pixels` a tile's rectangle is in pixels: `cell` is the
/// terminal cell's size, and the middle of the cell picks the tile.
#[must_use]
pub fn tile_at(
    map: &Map,
    hidden: Option<&Hidden>,
    x: u16,
    y: u16,
    cell: Option<(u16, u16)>,
) -> Option<usize> {
    let points: Vec<(u32, u32)> = match cell {
        Some((cw, ch)) => {
            let (cw, ch) = (u32::from(cw), u32::from(ch));
            vec![(u32::from(x) * cw + cw / 2, u32::from(y) * ch + ch / 2)]
        }
        // A cell is two half-cell units; a tile on either counts.
        None => vec![
            (u32::from(x), u32::from(y) * 2),
            (u32::from(x), u32::from(y) * 2 + 1),
        ],
    };
    let drawn = map
        .tiles()
        .iter()
        .enumerate()
        .filter(|(i, tile)| {
            points.iter().any(|&point| holds(tile.rect, point))
                && !hidden.is_some_and(|h| h.tile(*i))
        })
        .map(|(i, _)| i);
    deepest(map, drawn)
}

#[cfg(test)]
mod tests;
