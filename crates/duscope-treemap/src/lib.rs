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
mod errors;
mod gp;
pub mod packs;
pub mod paint;
mod squarify;
mod width;

pub use errors::TreemapError;
pub use gp::layout_gp;
pub use pool::layout;
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
    #[must_use]
    pub const fn new(x: u16, y: u16, w: u16, h: u16) -> Rect {
        Rect { x, y, w, h }
    }

    /// Its area in units.
    #[must_use]
    pub fn area(self) -> u32 {
        u32::from(self.w) * u32::from(self.h)
    }

    /// Whether it is at least `side` units in both directions.
    #[must_use]
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

mod pool;

#[cfg(test)]
mod tests;
