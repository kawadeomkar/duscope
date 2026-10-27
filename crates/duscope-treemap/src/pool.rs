//! [`layout`]: a block's children placed by either algorithm, with the
//! children too small for a tile of their own pooled into one `+N small`
//! tile, and the `--min-tile` threshold that decides what is too small.

use crate::{Algorithm, Layout, Rect, Small, Tile, layout_gp, squarify};

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
