//! `GrandPerspective`'s layout (`TreeBalancer.m`, `TreeLayoutBuilder.m`): the
//! items become a size-balanced binary tree, built as a Huffman tree by
//! pairing the two smallest nodes until one is left, the smaller of each
//! pair first. Each node's rectangle is then cut in two in proportion to its
//! first child's weight (half and half when the node weighs nothing): a
//! rectangle wider than tall is cut vertically with the first child on the
//! right, any other horizontally with the first child at the bottom, so the
//! smallest items drift to the lower right. Cuts are rounded to whole units.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use crate::Rect;

/// A node of the balanced tree: a leaf is an item's index.
enum Node {
    Leaf(usize),
    Pair { first: usize, second: usize },
}

/// Lays out `items` (`(weight, id)`) by bisection in `area`: a tile per
/// item, in the order given. A tile may come out zero units wide or tall;
/// [`crate::layout`] pools those.
#[must_use]
pub fn layout_gp(items: &[(u64, usize)], area: Rect) -> Vec<(usize, Rect)> {
    let mut rects = vec![Rect::new(area.x, area.y, 0, 0); items.len()];
    if items.is_empty() {
        return Vec::new();
    }
    // The balanced tree: nodes[..items.len()] are the leaves. Weights add
    // up in u128, so even u64::MAX items keep their proportions.
    let mut nodes: Vec<(Node, u128)> = items
        .iter()
        .enumerate()
        .map(|(i, &(w, _))| (Node::Leaf(i), u128::from(w)))
        .collect();
    // Smallest weight first, then the oldest node: ties pair in order.
    let mut heap: BinaryHeap<Reverse<(u128, usize)>> = nodes
        .iter()
        .enumerate()
        .map(|(i, &(_, w))| Reverse((w, i)))
        .collect();
    while let (Some(Reverse((w1, first))), Some(Reverse((w2, second)))) = (heap.pop(), heap.pop()) {
        let pair = nodes.len();
        let weight = w1.saturating_add(w2);
        nodes.push((Node::Pair { first, second }, weight));
        heap.push(Reverse((weight, pair)));
    }
    let mut stack = vec![(nodes.len() - 1, area)];
    while let Some((node, rect)) = stack.pop() {
        match nodes[node].0 {
            Node::Leaf(item) => rects[item] = rect,
            Node::Pair { first, second } => {
                let (part, whole) = (nodes[first].1, nodes[node].1);
                let (a, b) = cut(rect, part, whole);
                stack.push((first, a));
                stack.push((second, b));
            }
        }
    }
    items
        .iter()
        .zip(rects)
        .map(|(&(_, id), r)| (id, r))
        .collect()
}

/// Cuts `rect` for a first child of weight `part` out of `whole`: the first
/// child's rectangle (right or bottom), then the second's.
fn cut(rect: Rect, part: u128, whole: u128) -> (Rect, Rect) {
    let share = |extent: u16| -> u16 {
        let extent = u128::from(extent);
        let size = if whole == 0 {
            extent.div_ceil(2)
        } else {
            // `part` is at most `whole`: the product overflows only past
            // 2^47 items of u64::MAX each, and saturates there.
            extent.saturating_mul(part).saturating_add(whole / 2) / whole
        };
        // At most `extent`, which fits.
        u16::try_from(size.min(extent)).unwrap_or(u16::MAX)
    };
    if rect.w > rect.h {
        let w1 = share(rect.w);
        (
            Rect::new(rect.x + rect.w - w1, rect.y, w1, rect.h),
            Rect::new(rect.x, rect.y, rect.w - w1, rect.h),
        )
    } else {
        let h1 = share(rect.h);
        (
            Rect::new(rect.x, rect.y + rect.h - h1, rect.w, h1),
            Rect::new(rect.x, rect.y, rect.w, rect.h - h1),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{Rect, layout_gp};

    #[test]
    fn p1_map_01_bisection_puts_the_smaller_half_right_or_below() {
        // 3 and 1 pair first (1 first), then 4 against that pair (equal, the
        // older node first): the wide area is cut in half, 4 on the right.
        let tiles = layout_gp(&[(4, 10), (3, 11), (1, 12)], Rect::new(0, 0, 8, 4));
        assert_eq!(tiles[0], (10, Rect::new(4, 0, 4, 4)));
        // The pair's square 4×4 is cut horizontally, the 1 at the bottom.
        assert_eq!(tiles[2], (12, Rect::new(0, 3, 4, 1)));
        assert_eq!(tiles[1], (11, Rect::new(0, 0, 4, 3)));
    }
}
