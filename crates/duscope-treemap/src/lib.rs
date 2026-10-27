//! Treemap layouts, the static terminal map and the SVG and PNG writers.
//!
//! This crate is an implementation detail of the `duscope` binary and promises
//! no API stability before 1.0.
//!
//! Layouts work in half-cell units: a unit is one column wide and half a row
//! tall, so a unit is close to square on a terminal and an 80×18 canvas is
//! 80×36 units. [`layout`] places a directory's children with
//! [`squarify`] (the default) or `GrandPerspective`'s bisection
//! ([`layout_gp`]), and pools what would come out smaller than `--min-tile`
//! units into one `+N small` tile.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod cells;
mod gp;
pub mod packs;
pub mod paint;
mod squarify;

pub use gp::layout_gp;
pub use squarify::squarify;

/// A rectangle in half-cell units: `x` and `w` count columns, `y` and `h`
/// half rows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Rect {
    /// The left edge.
    pub x: u16,
    /// The top edge.
    pub y: u16,
    /// The width.
    pub w: u16,
    /// The height.
    pub h: u16,
}

impl Rect {
    /// A rectangle.
    pub const fn new(x: u16, y: u16, w: u16, h: u16) -> Rect {
        Rect { x, y, w, h }
    }

    /// Its area in units.
    pub fn area(self) -> u32 {
        u32::from(self.w) * u32::from(self.h)
    }

    /// Whether it is at least `side` units in both directions.
    pub fn fits(self, side: u16) -> bool {
        self.w >= side && self.h >= side
    }
}

/// How a block's children are placed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Algorithm {
    /// Squarified strips, largest first: near-square tiles that hold labels.
    #[default]
    Squarify,
    /// `GrandPerspective`'s recursive bisection of a size-balanced tree.
    Gp,
}

/// One child's tile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tile {
    /// The id the caller gave the child.
    pub id: usize,
    /// Where it goes.
    pub rect: Rect,
}

/// The `+N small` tile: the children too small for a tile of their own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Small {
    /// Their ids, largest first.
    pub ids: Vec<usize>,
    /// Their weights added up.
    pub weight: u64,
    /// Where the tile goes.
    pub rect: Rect,
}

/// A block's layout: a tile per child large enough, largest first, and the
/// rest pooled. The tiles and the pool cover the area exactly.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Layout {
    /// The children with tiles of their own, largest first.
    pub tiles: Vec<Tile>,
    /// The pool, when any child is too small.
    pub small: Option<Small>,
}

/// The id [`layout`] gives the pool while it places it.
const POOL: usize = usize::MAX;

/// Lays out `items` (`(weight, id)`, any order) in `area`: every child
/// whose tile would be under `min_tile` units in either direction, and
/// every child of weight 0, goes into the `+N small` pool. With
/// [`Algorithm::Squarify`] the pool is placed last, at the bottom right; the
/// bisection places it by its weight. A pool too small to show is laid out
/// as if it weighed more, up to the smallest tile's weight, and takes that
/// child when even that is not enough. Children of equal weight keep their
/// order.
pub fn layout(items: &[(u64, usize)], area: Rect, algorithm: Algorithm, min_tile: u16) -> Layout {
    let mut sorted = items.to_vec();
    sorted.sort_by(|a, b| b.0.cmp(&a.0));
    if sorted.is_empty() {
        return Layout::default();
    }
    let min = min_tile.max(1);
    // The children with tiles are a prefix of `sorted`; the rest are pooled.
    let mut kept = if area.fits(min) {
        sorted.iter().take_while(|&&(weight, _)| weight > 0).count()
    } else {
        0
    };
    // The weight the pool is laid out with, when more than its own.
    let mut boost = None;
    loop {
        let (head, pool) = sorted.split_at(kept);
        if head.is_empty() {
            return Layout {
                tiles: Vec::new(),
                small: Some(small(pool, area)),
            };
        }
        let pool_weight = sum(pool);
        let mut placing = head.to_vec();
        if !pool.is_empty() {
            placing.push((boost.unwrap_or(pool_weight).max(pool_weight), POOL));
        }
        // Both layouts return a tile per item placed, in order: the head,
        // then the pool.
        let placed = match algorithm {
            Algorithm::Squarify => squarify::in_order(&placing, area),
            Algorithm::Gp => layout_gp(&placing, area),
        };
        // The first child, largest first, whose tile is too small: it and
        // everything after it go into the pool.
        if let Some(first) = placed[..head.len()]
            .iter()
            .position(|&(_, rect)| !rect.fits(min))
        {
            kept = first;
            boost = None;
            continue;
        }
        let small = match placed.get(head.len()) {
            None => None,
            Some(&(_, rect)) if !rect.fits(min) => {
                // Double the boost up to the smallest tile's weight, which
                // is tried too; past it the pool takes that tile.
                let smallest = head.last().map_or(0, |&(w, _)| w);
                if boost.unwrap_or(0) < smallest {
                    let more = boost
                        .map_or_else(|| pool_share(head, area, min), |b: u64| b.saturating_mul(2));
                    boost = Some(more.min(smallest));
                } else {
                    kept -= 1;
                    boost = None;
                }
                continue;
            }
            Some(&(_, rect)) => Some(small(pool, rect)),
        };
        let tiles = placed[..head.len()]
            .iter()
            .map(|&(id, rect)| Tile { id, rect })
            .collect();
        return Layout { tiles, small };
    }
}

/// The weight that gives the pool `min × min` units beside `head` in
/// `area`, at least 1.
fn pool_share(head: &[(u64, usize)], area: Rect, min: u16) -> u64 {
    let total: u128 = head.iter().map(|&(w, _)| u128::from(w)).sum();
    let square = u128::from(min) * u128::from(min);
    let rest = u128::from(area.area()).saturating_sub(square).max(1);
    u64::try_from((total * square).div_ceil(rest))
        .unwrap_or(u64::MAX)
        .max(1)
}

/// The pool of `items` in `rect`.
fn small(items: &[(u64, usize)], rect: Rect) -> Small {
    Small {
        ids: items.iter().map(|&(_, id)| id).collect(),
        weight: sum(items),
        rect,
    }
}

/// The weights of `items` added up, at most `u64::MAX`.
fn sum(items: &[(u64, usize)]) -> u64 {
    items.iter().fold(0, |sum, &(w, _)| sum.saturating_add(w))
}

#[cfg(test)]
mod tests;
