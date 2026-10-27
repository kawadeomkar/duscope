//! The squarified layout (Bruls, Huizing and van Wijk, 2000), as d3's
//! `treemapSquarify` draws it with ratio 1: items largest first, laid in
//! strips along the shorter side of what is left, each strip taking items
//! while its worst aspect ratio does not get worse.
//!
//! The edges are computed as floats, then rounded to whole units. Each edge
//! is computed once and shared by the tiles on both sides of it, and the
//! last strip and the last tile of a strip end on the area's own edge, so
//! the rounded tiles cover the area exactly.

use crate::Rect;

/// Squarifies `items` (`(weight, id)`) in `area`: a tile per item of
/// positive weight, largest first (equal weights keep their order). A tile
/// may round to zero width or height; [`crate::layout`] pools those.
pub fn squarify(items: &[(u64, usize)], area: Rect) -> Vec<(usize, Rect)> {
    let mut sorted: Vec<(u64, usize)> = items.iter().copied().filter(|&(w, _)| w > 0).collect();
    sorted.sort_by(|a, b| b.0.cmp(&a.0));
    in_order(&sorted, area)
}

/// Squarifies `items` in the order given, which should be largest first
/// but for a last item (the pool): a tile per item, in that order.
pub(crate) fn in_order(items: &[(u64, usize)], area: Rect) -> Vec<(usize, Rect)> {
    let total: f64 = items.iter().map(|&(w, _)| weight(w)).sum();
    if total <= 0.0 || area.area() == 0 {
        let empty = Rect::new(area.x, area.y, 0, 0);
        return items.iter().map(|&(_, id)| (id, empty)).collect();
    }
    let (x1, y1) = (
        f64::from(area.x) + f64::from(area.w),
        f64::from(area.y) + f64::from(area.h),
    );
    // What is left to fill: from (x0, y0) to the area's far corner.
    let (mut x0, mut y0) = (f64::from(area.x), f64::from(area.y));
    // Units of area per unit of weight.
    let k = f64::from(area.w) * f64::from(area.h) / total;
    let mut out = Vec::with_capacity(items.len());
    let mut left = items;
    while !left.is_empty() {
        // A strip along the shorter side: a column when what is left is
        // wider than tall (or square), a row otherwise.
        let column = x1 - x0 >= y1 - y0;
        let side = if column { y1 - y0 } else { x1 - x0 };
        let len = strip(left, side, k);
        let (row, after) = left.split_at(len);
        let sum: f64 = row.iter().map(|&(w, _)| weight(w)).sum();
        let start = if column { x0 } else { y0 };
        let far = if after.is_empty() {
            if column { x1 } else { y1 }
        } else {
            start + sum * k / side
        };
        // The tiles' edges along the strip, the last on the area's edge.
        let (along, end) = if column { (y0, y1) } else { (x0, x1) };
        let mut edge = along;
        let mut cum = 0.0;
        for (i, &(w, id)) in row.iter().enumerate() {
            cum += weight(w);
            let next = if i + 1 == row.len() {
                end
            } else {
                along + (end - along) * cum / sum
            };
            let rect = if column {
                snapped(start, edge, far, next)
            } else {
                snapped(edge, start, next, far)
            };
            out.push((id, rect));
            edge = next;
        }
        if column {
            x0 = far;
        } else {
            y0 = far;
        }
        left = after;
    }
    out
}

/// How many of `items` the next strip takes: items while the strip's worst
/// aspect ratio does not get worse, at least one. A strip's worst ratio
/// comes from its largest or its smallest item, so those are all it keeps.
fn strip(items: &[(u64, usize)], side: f64, k: f64) -> usize {
    let side2 = side * side;
    let (mut sum, mut max, mut min) = (0.0f64, 0.0f64, f64::INFINITY);
    let mut worst = f64::INFINITY;
    for (i, &(w, _)) in items.iter().enumerate() {
        let area = weight(w) * k;
        let (s, hi, lo) = (sum + area, max.max(area), min.min(area));
        let s2 = s * s;
        let next = (side2 * hi / s2).max(s2 / (side2 * lo));
        if i > 0 && next > worst {
            return i;
        }
        (sum, max, min, worst) = (s, hi, lo, next);
    }
    items.len()
}

/// The rectangle between two float corners, each edge rounded to the
/// nearest unit (halves up).
fn snapped(left: f64, top: f64, right: f64, bottom: f64) -> Rect {
    let (x, y) = (snap(left), snap(top));
    Rect::new(x, y, snap(right) - x, snap(bottom) - y)
}

// Edges lie between 0 and u16::MAX, so the cast is exact after rounding.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn snap(v: f64) -> u16 {
    (v + 0.5).floor().clamp(0.0, f64::from(u16::MAX)) as u16
}

/// A weight as a float: only ratios of weights matter, and a ratio loses
/// nothing a unit could show.
#[allow(clippy::cast_precision_loss)]
pub(crate) fn weight(w: u64) -> f64 {
    w as f64
}

#[cfg(test)]
mod tests {
    use super::{Rect, squarify};

    #[test]
    fn p1_map_01_squarify_lays_out_one_strip_per_shorter_side() {
        // Six items on a 6×4 area, the example of Bruls et al.: weights in
        // units of area.
        let items = [(6, 0), (6, 1), (4, 2), (3, 3), (2, 4), (2, 5), (1, 6)];
        let tiles = squarify(&items, Rect::new(0, 0, 6, 4));
        let area: u32 = tiles.iter().map(|&(_, r)| r.area()).sum();
        assert_eq!(area, 24);
        // The first strip is a column of the two largest along the height.
        assert_eq!(tiles[0], (0, Rect::new(0, 0, 3, 2)));
        assert_eq!(tiles[1], (1, Rect::new(0, 2, 3, 2)));
        // Zero weights get no tile; the order is largest first.
        let tiles = squarify(&[(0, 9), (1, 1), (5, 2)], Rect::new(0, 0, 6, 1));
        assert_eq!(tiles.iter().map(|&(id, _)| id).collect::<Vec<_>>(), [2, 1]);
    }
}
