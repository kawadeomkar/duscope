//! P1-MAP-01: both layouts cover the area exactly, keep the order, give
//! every tile at least `--min-tile` units, and lay out the proposal's
//! mockup as it is drawn.

use std::collections::HashSet;
use std::path::Path;

use proptest::prelude::*;
use serde::Deserialize;

use super::{Algorithm, Layout, Rect, layout};

/// Every cell of `area` covered exactly once by the layout's tiles and pool,
/// and nothing outside it.
fn covers_exactly(layout: &Layout, area: Rect) -> Result<(), String> {
    let mut cells = vec![0u8; area.area() as usize];
    let rects = layout
        .tiles
        .iter()
        .map(|t| t.rect)
        .chain(layout.small.iter().map(|s| s.rect));
    for rect in rects {
        if rect.x < area.x
            || rect.y < area.y
            || rect.x + rect.w > area.x + area.w
            || rect.y + rect.h > area.y + area.h
        {
            return Err(format!("{rect:?} lies outside {area:?}"));
        }
        for y in rect.y..rect.y + rect.h {
            for x in rect.x..rect.x + rect.w {
                let i = usize::from(y - area.y) * usize::from(area.w) + usize::from(x - area.x);
                cells[i] += 1;
            }
        }
    }
    match cells.iter().position(|&n| n != 1) {
        Some(i) => Err(format!("cell {i} is covered {} times", cells[i])),
        None => Ok(()),
    }
}

fn items() -> impl Strategy<Value = Vec<(u64, usize)>> {
    let weight = prop_oneof![
        8 => 1u64..1_000_000_000_000,
        2 => 0u64..10,
        // Ratios past what a float's mantissa holds.
        2 => (1u64 << 55)..(1u64 << 60),
        // Sums that saturate a u64.
        1 => Just(u64::MAX),
    ];
    // Mostly a screenful of children, sometimes a crowd.
    let count = prop_oneof![9 => 1usize..80, 1 => 80usize..2000];
    count
        .prop_flat_map(move |n| prop::collection::vec(weight.clone(), n))
        .prop_map(|weights| {
            weights
                .into_iter()
                .enumerate()
                .map(|(i, w)| (w, i))
                .collect()
        })
}

/// Areas at an offset, empty ones included.
fn area() -> impl Strategy<Value = Rect> {
    (0u16..20, 0u16..20, 0u16..200, 0u16..80).prop_map(|(x, y, w, h)| Rect::new(x, y, w, h))
}

fn check(items: &[(u64, usize)], area: Rect, algorithm: Algorithm, min: u16) -> Result<(), String> {
    verify(
        &layout(items, area, algorithm, min),
        items,
        area,
        algorithm,
        min,
    )
}

/// What [`check`] checks, of a layout already made.
fn verify(
    laid: &Layout,
    items: &[(u64, usize)],
    area: Rect,
    algorithm: Algorithm,
    min: u16,
) -> Result<(), String> {
    covers_exactly(laid, area)?;
    // Every child is either a tile or in the pool, once.
    let mut ids: Vec<usize> = laid.tiles.iter().map(|t| t.id).collect();
    ids.extend(laid.small.iter().flat_map(|s| s.ids.iter().copied()));
    ids.sort_unstable();
    if ids != (0..items.len()).collect::<Vec<_>>() {
        return Err(format!("children placed: {ids:?}"));
    }
    // The tiles and the pool each keep the children largest first, equal
    // weights in their order. Squarify's tiles are the largest children.
    let mut sorted = items.to_vec();
    sorted.sort_by(|a, b| b.0.cmp(&a.0));
    let order: Vec<usize> = sorted.iter().map(|&(_, id)| id).collect();
    let tiles: Vec<usize> = laid.tiles.iter().map(|t| t.id).collect();
    let with_tiles: HashSet<usize> = tiles.iter().copied().collect();
    let (in_order, pool): (Vec<usize>, Vec<usize>) =
        order.iter().partition(|&id| with_tiles.contains(id));
    if in_order != tiles {
        return Err(format!("tiles {tiles:?}, order {order:?}"));
    }
    if algorithm == Algorithm::Squarify && order[..tiles.len()] != tiles[..] {
        return Err(format!("tiles {tiles:?} are not the largest of {order:?}"));
    }
    if let Some(small) = &laid.small
        && pool != small.ids
    {
        return Err(format!("pool {:?}, order {order:?}", small.ids));
    }
    // Every tile is at least `min` units each way, and so is the pool
    // unless it is the whole area.
    if let Some(tile) = laid.tiles.iter().find(|t| !t.rect.fits(min)) {
        return Err(format!("{tile:?} is under {min} units"));
    }
    if let Some(small) = &laid.small
        && !small.rect.fits(min.min(area.w).min(area.h))
    {
        return Err(format!("pool {:?} is under {min} units", small.rect));
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn p1_map_01_squarify_covers_the_area_in_order(
        items in items(),
        area in area(),
        min in 1u16..=8,
    ) {
        check(&items, area, Algorithm::Squarify, min).map_err(TestCaseError::fail)?;
    }

    #[test]
    fn p1_map_01_bisection_covers_the_area_in_order(
        items in items(),
        area in area(),
        min in 1u16..=8,
    ) {
        check(&items, area, Algorithm::Gp, min).map_err(TestCaseError::fail)?;
    }
}

#[test]
fn p1_map_01_small_children_are_pooled_at_the_bottom_right() {
    // One large child and a hundred that round below a unit.
    let mut items = vec![(1_000_000, 0)];
    items.extend((1..=100).map(|i| (10, i)));
    let area = Rect::new(0, 0, 20, 10);
    let laid = layout(&items, area, Algorithm::Squarify, 1);
    assert_eq!(laid.tiles.len(), 1);
    let small = laid.small.expect("a pool");
    assert_eq!(small.ids.len(), 100);
    assert_eq!(small.weight, 1000);
    // The pool is placed last: it ends on the area's far corner.
    assert_eq!(
        (small.rect.x + small.rect.w, small.rect.y + small.rect.h),
        (20, 10)
    );
    // --min-tile 4 pools more of a block of equals.
    let items: Vec<(u64, usize)> = (0..40).map(|i| (100, i)).collect();
    let calm = layout(&items, area, Algorithm::Squarify, 4);
    let busy = layout(&items, area, Algorithm::Squarify, 1);
    assert!(calm.tiles.len() < busy.tiles.len());
    assert!(calm.tiles.iter().all(|t| t.rect.fits(4)));
    // Children that weigh nothing are pooled; an empty block lays out
    // nothing.
    let laid = layout(&[(5, 0), (0, 1)], area, Algorithm::Squarify, 1);
    assert_eq!(laid.small.map(|s| s.ids), Some(vec![1]));
    assert_eq!(layout(&[], area, Algorithm::Gp, 1), Layout::default());
}

#[derive(Deserialize)]
struct Mockup {
    canvas: Canvas,
    tile: Vec<MockTile>,
}

#[derive(Deserialize)]
struct Canvas {
    columns: u16,
    units_high: u16,
}

#[derive(Deserialize)]
struct MockTile {
    name: String,
    gib: f64,
    rect: [u16; 4],
}

#[test]
fn p1_map_01_the_mockup_lays_out_to_its_eleven_tiles() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/mockup.toml");
    let mockup: Mockup = toml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(mockup.tile.len(), 11);
    // Sizes in tenths of a GiB, as the mockup gives them.
    let items: Vec<(u64, usize)> = mockup
        .tile
        .iter()
        .enumerate()
        .map(|(i, t)| (format!("{:.0}", t.gib * 10.0).parse().unwrap(), i))
        .collect();
    let area = Rect::new(0, 0, mockup.canvas.columns, mockup.canvas.units_high);
    let laid = layout(&items, area, Algorithm::Squarify, 1);
    assert_eq!(laid.small, None);
    assert_eq!(laid.tiles.len(), 11);
    for (tile, mock) in laid.tiles.iter().zip(&mockup.tile) {
        let [x, y, w, h] = mock.rect;
        assert_eq!(tile.rect, Rect::new(x, y, w, h), "{}", mock.name);
    }
}

#[test]
fn p1_map_01_weights_far_apart_still_cover_the_area() {
    // Ratios near 2^58: the float edges round past the area's own.
    let weights = [
        774,
        2692,
        212,
        389,
        3538,
        0,
        528,
        172,
        288_248_411_558_657_120,
    ];
    let items: Vec<(u64, usize)> = weights.into_iter().zip(0..).collect();
    for algorithm in [Algorithm::Squarify, Algorithm::Gp] {
        for min in [1, 4] {
            check(&items, Rect::new(0, 0, 55, 58), algorithm, min).unwrap();
        }
    }
    let huge = [(u64::MAX, 0), (u64::MAX, 1), (1, 2)];
    check(&huge, Rect::new(0, 0, 80, 24), Algorithm::Squarify, 1).unwrap();
}

#[test]
fn p1_map_01_the_pool_tries_the_smallest_tiles_weight() {
    let weights = [
        269_870, 549_675, 701_720, 465_351, 1, 36_722, 51_267, 58_903,
    ];
    let items: Vec<(u64, usize)> = weights.into_iter().zip(0..).collect();
    for algorithm in [Algorithm::Squarify, Algorithm::Gp] {
        // Doubling the pool's weight overshoots the smallest tile's; tried
        // at that weight exactly, the pool fits beside six tiles.
        let laid = layout(&items, Rect::new(0, 0, 59, 13), algorithm, 4);
        assert_eq!(laid.tiles.len(), 6, "{algorithm:?}");
    }
}

#[test]
fn p1_map_01_bisection_pools_only_the_tiles_too_small() {
    // A wide directory: tile sizes under the bisection do not follow the
    // weights, so pooling everything after the first tile too small would
    // leave none.
    let items: Vec<(u64, usize)> = (0..100_000)
        .zip(0..)
        .map(|(i, id)| (100_000 - i, id))
        .collect();
    let area = Rect::new(0, 0, 300, 200);
    let bisected = layout(&items, area, Algorithm::Gp, 1);
    assert!(
        bisected.tiles.len() > 1_000,
        "{} tiles",
        bisected.tiles.len()
    );
    verify(&bisected, &items, area, Algorithm::Gp, 1).unwrap();
    // Weights that saturate a u64 when added keep their proportions.
    let huge = [(u64::MAX, 0), (u64::MAX, 1), (u64::MAX, 2), (1, 3)];
    let area = Rect::new(0, 0, 80, 36);
    for algorithm in [Algorithm::Squarify, Algorithm::Gp] {
        let laid = layout(&huge, area, algorithm, 1);
        assert_eq!(laid.tiles.len(), 3, "{algorithm:?}");
        assert_eq!(laid.small.map(|s| s.ids), Some(vec![3]), "{algorithm:?}");
        check(&huge, area, algorithm, 1).unwrap();
    }
}

#[test]
fn p1_map_01_an_area_too_small_pools_every_child() {
    let items = [(5, 0), (u64::MAX, 1), (0, 2), (7, 3)];
    for area in [
        Rect::new(3, 4, 0, 36),
        Rect::new(3, 4, 80, 0),
        Rect::new(0, 0, 0, 0),
        Rect::new(9, 9, 1, 1),
    ] {
        for algorithm in [Algorithm::Squarify, Algorithm::Gp] {
            // --min-tile 8 does not fit in 1×1 either.
            let laid = layout(&items, area, algorithm, 8);
            assert_eq!(laid.tiles, [], "{area:?} {algorithm:?}");
            let small = laid.small.expect("a pool");
            assert_eq!(small.ids, [1, 3, 0, 2], "{area:?} {algorithm:?}");
            assert_eq!(small.rect, area);
            check(&items, area, algorithm, 1).unwrap();
        }
    }
}
