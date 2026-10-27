//! P1-MAP-01: both layouts cover the area exactly, keep the order, give
//! every tile at least `--min-tile` units, and lay out the proposal's
//! mockup as it is drawn.

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
    prop::collection::vec(
        prop_oneof![
            4 => 1u64..1_000_000_000_000,
            1 => 0u64..10,
        ],
        1..80,
    )
    .prop_map(|weights| {
        weights
            .into_iter()
            .enumerate()
            .map(|(i, w)| (w, i))
            .collect()
    })
}

fn area() -> impl Strategy<Value = Rect> {
    (0u16..20, 0u16..20, 1u16..200, 1u16..80).prop_map(|(x, y, w, h)| Rect::new(x, y, w, h))
}

fn check(items: &[(u64, usize)], area: Rect, algorithm: Algorithm, min: u16) -> Result<(), String> {
    let laid = layout(items, area, algorithm, min);
    covers_exactly(&laid, area)?;
    // Every child is either a tile or in the pool, once.
    let mut ids: Vec<usize> = laid.tiles.iter().map(|t| t.id).collect();
    ids.extend(laid.small.iter().flat_map(|s| s.ids.iter().copied()));
    ids.sort_unstable();
    if ids != (0..items.len()).collect::<Vec<_>>() {
        return Err(format!("children placed: {ids:?}"));
    }
    // The tiles are the largest children, largest first, equal weights in
    // their order; the pool holds the rest, largest first.
    let mut sorted = items.to_vec();
    sorted.sort_by(|a, b| b.0.cmp(&a.0));
    let order: Vec<usize> = sorted.iter().map(|&(_, id)| id).collect();
    let tiles: Vec<usize> = laid.tiles.iter().map(|t| t.id).collect();
    if order[..tiles.len()] != tiles[..] {
        return Err(format!("tiles {tiles:?}, order {order:?}"));
    }
    if let Some(small) = &laid.small
        && order[tiles.len()..] != small.ids[..]
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
        min in prop::sample::select(vec![1u16, 2, 4]),
    ) {
        check(&items, area, Algorithm::Squarify, min).map_err(TestCaseError::fail)?;
    }

    #[test]
    fn p1_map_01_bisection_covers_the_area_in_order(
        items in items(),
        area in area(),
        min in prop::sample::select(vec![1u16, 2, 4]),
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
