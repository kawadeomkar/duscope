//! P4-TUI-06: the mouse in `TestBackend`: a click selects the deepest tile
//! drawn under the cell, or a row in the siblings pane, the legend or a
//! table; a double click zooms and the wheel walks the path; drags do
//! nothing and neither does a click on a pane's frame.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use duscope_core::arena::Arena;
use duscope_core::query::{ArenaView, TreeView};
use duscope_core::rollup::RollUp;
use duscope_treemap::cells::Depth;
use duscope_treemap::packs::Pack;
use duscope_treemap::paint::{ColorBy, DrawnKind};
use ratatui_core::buffer::Buffer;

use super::{Clicks, DOUBLE, Press, tile_at};
use crate::cursor::tests::{char, first_frame, press};
use crate::layout::{Panes, inner, layout};
use crate::panes::tests::{classes, mockup, opts, status};
use crate::{App, Flow};

fn mockup_view(tree: &(Arena, RollUp)) -> ArenaView<'_> {
    ArenaView::new(&tree.0, &tree.1, PathBuf::from("/Users/alex/Library"))
}

fn viewer<'a>(
    view: &'a ArenaView<'a>,
    pack: &'a Pack,
    classes: &'a duscope_core::rules::Classes,
) -> App<'a> {
    App::new(
        view,
        opts(pack, classes, ColorBy::Type),
        Depth::TrueColor,
        Some(Path::new("/Users/alex")),
        status(view),
        None,
    )
}

fn text_of(buf: &Buffer) -> String {
    let mut out = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            out.push_str(buf[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

fn left(column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

fn event(kind: MouseEventKind, column: u16, row: u16) -> Event {
    Event::Mouse(MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    })
}

/// A cell on the map, and the tile under it; the map at 120×40 starts at
/// the column and row of its pane's inside.
fn cell_of(
    app: &App<'_>,
    w: u16,
    h: u16,
    wanted: impl Fn(&duscope_treemap::paint::Drawn) -> bool,
) -> (u16, u16, usize) {
    let map = app.map();
    let area = layout(w, h).map;
    let (i, tile) = map
        .tiles()
        .iter()
        .enumerate()
        .find(|(_, tile)| wanted(tile))
        .expect("a tile");
    let middle = tile.rect.y + tile.rect.h / 2;
    (
        area.x + tile.rect.x + tile.rect.w / 2,
        area.y + middle / 2,
        i,
    )
}

#[test]
fn p4_tui_06_clicks_are_single_or_double_by_cell_and_time() {
    let mut clicks = Clicks::default();
    let t = Instant::now();
    assert_eq!(clicks.press(3, 4, t), Press::Single);
    assert_eq!(
        clicks.press(3, 4, t + Duration::from_millis(100)),
        Press::Double
    );
    // A double click is spent: a third click starts over.
    assert_eq!(
        clicks.press(3, 4, t + Duration::from_millis(200)),
        Press::Single
    );
    // Too late, or another cell.
    assert_eq!(clicks.press(3, 4, t + DOUBLE * 3), Press::Single);
    assert_eq!(clicks.press(5, 4, t + DOUBLE * 3), Press::Single);
}

#[test]
fn p4_tui_06_a_click_selects_the_deepest_tile_drawn_under_the_cell() {
    let tree = mockup();
    let view = mockup_view(&tree);
    let (mocha, classes) = (Pack::named("mocha").unwrap(), classes());
    let mut app = viewer(&view, &mocha, &classes);
    let mut terminal = first_frame(&app, 120, 40);
    // A tile of its own, not the one the cursor starts on.
    let start = app.selected();
    let (x, y, i) = cell_of(&app, 120, 40, |tile| {
        tile.kind == DrawnKind::Leaf && tile.id != start
    });
    let map = app.map();
    let tile = map.tiles()[i];
    assert_eq!(app.on(&Event::Mouse(left(x, y))), Flow::Redraw);
    assert_eq!(app.selected(), tile.id);
    // Again on the same cell, later: nothing new (not a double click).
    let later = Instant::now() + DOUBLE * 2;
    assert_eq!(app.mouse(left(x, y), later), Flow::Wait);
    assert_eq!(app.selected(), tile.id);
    // Inside an open folder a click lands on the entry drawn there.
    let (x, y, j) = cell_of(&app, 120, 40, |tile| {
        tile.kind == DrawnKind::Leaf && view.node(tile.id).depth >= 3
    });
    assert_eq!(app.mouse(left(x, y), later), Flow::Redraw);
    assert_eq!(app.selected(), map.tiles()[j].id);
    // A pool's tile selects the largest entry in it.
    if let Some((i, tile)) = map
        .tiles()
        .iter()
        .enumerate()
        .find(|(_, tile)| matches!(tile.kind, DrawnKind::Small))
    {
        let area = layout(120, 40).map;
        let middle = tile.rect.y + tile.rect.h / 2;
        let (px, py) = (area.x + tile.rect.x + tile.rect.w / 2, area.y + middle / 2);
        assert_eq!(app.mouse(left(px, py), later), Flow::Redraw);
        assert_eq!(map.tiles()[i].id, tile.id);
        let selected = view.node(app.selected());
        assert_eq!(selected.parent, tile.id);
    }
    // On the map's border, the screen around it and the key line: nothing.
    let pane = layout(120, 40).map_pane;
    assert_eq!(app.mouse(left(pane.x, pane.y), later), Flow::Wait);
    assert_eq!(app.mouse(left(0, 0), later), Flow::Wait);
    assert_eq!(app.mouse(left(0, 39), later), Flow::Wait);
    let _ = &mut terminal;
}

#[test]
fn p4_tui_06_what_the_mask_hides_is_not_clicked() {
    let tree = mockup();
    let view = mockup_view(&tree);
    let (mocha, classes) = (Pack::named("mocha").unwrap(), classes());
    let mut app = viewer(&view, &mocha, &classes);
    let mut terminal = first_frame(&app, 120, 40);
    // `/*.pdf`: nothing of the tree matches, so the map's leaf tiles are
    // hidden and a click on one of them selects nothing.
    for ch in "/*.xyz\r".chars() {
        let event = if ch == '\r' {
            crate::cursor::tests::key(crossterm::event::KeyCode::Enter)
        } else {
            char(ch)
        };
        press(&mut app, &mut terminal, &event);
    }
    let (x, y, i) = cell_of(&app, 120, 40, |tile| tile.kind == DrawnKind::Leaf);
    let hidden_id = app.map().tiles()[i].id;
    app.mouse(left(x, y), Instant::now());
    // What the click lands on is what is drawn there: the folder around
    // the hidden tile, never the hidden tile itself.
    assert_ne!(app.selected(), hidden_id);
}

#[test]
fn p4_tui_06_a_double_click_zooms_and_the_wheel_walks_the_path() {
    let tree = mockup();
    let view = mockup_view(&tree);
    let (mocha, classes) = (Pack::named("mocha").unwrap(), classes());
    let mut app = viewer(&view, &mocha, &classes);
    let _terminal = first_frame(&app, 120, 40);
    let start = app.selected();
    let (x, y, _) = cell_of(&app, 120, 40, |tile| {
        tile.kind == DrawnKind::Leaf && tile.id != start
    });
    let t = Instant::now();
    assert_eq!(app.mouse(left(x, y), t), Flow::Redraw);
    assert_eq!(app.zoom_root(), 0);
    assert_eq!(app.mouse(left(x, y), t + DOUBLE / 2), Flow::Redraw);
    let root = app.zoom_root();
    assert_ne!(root, 0, "the double click zoomed");
    // The wheel: down the path, up the path; at the top nothing moves.
    let selected = app.selected();
    assert_eq!(app.on(&event(MouseEventKind::ScrollUp, x, y)), Flow::Redraw);
    assert_ne!(app.selected(), selected);
    let up = app.selected();
    assert_eq!(
        app.on(&event(MouseEventKind::ScrollDown, x, y)),
        Flow::Redraw
    );
    assert_ne!(app.selected(), up);
    // The wheel clears what the last key left on the key line, as a key
    // does.
    let mut terminal = first_frame(&app, 120, 40);
    press(&mut app, &mut terminal, &char('y'));
    assert!(text_of(terminal.backend().buffer()).contains("copied "));
    assert_eq!(
        press(
            &mut app,
            &mut terminal,
            &event(MouseEventKind::ScrollUp, x, y)
        ),
        Flow::Redraw
    );
    assert!(!text_of(terminal.backend().buffer()).contains("copied "));
    // A drag, a move or the right button do nothing.
    let at = app.selected();
    assert_eq!(
        app.on(&event(MouseEventKind::Drag(MouseButton::Left), x, y)),
        Flow::Wait
    );
    assert_eq!(app.on(&event(MouseEventKind::Moved, x, y)), Flow::Wait);
    assert_eq!(
        app.on(&event(MouseEventKind::Down(MouseButton::Right), x, y)),
        Flow::Wait
    );
    assert_eq!(app.selected(), at);
    // While the help or a prompt is open the mouse does nothing.
    let mut terminal = first_frame(&app, 120, 40);
    press(&mut app, &mut terminal, &char('?'));
    assert_eq!(app.mouse(left(x, y), t + DOUBLE * 2), Flow::Wait);
}

#[test]
fn p4_tui_06_clicks_on_the_sibling_and_legend_rows_and_the_tables() {
    let tree = mockup();
    let view = mockup_view(&tree);
    let (mocha, classes) = (Pack::named("mocha").unwrap(), classes());
    let mut app = viewer(&view, &mocha, &classes);
    let mut terminal = first_frame(&app, 120, 40);
    let at = layout(120, 40);
    let (siblings, legend) = match at.panes {
        Panes::Column {
            siblings, legend, ..
        } => (siblings, legend),
        Panes::Folded { .. } => panic!("the column"),
    };
    let t = Instant::now();
    // The second row of the siblings pane: its second child.
    let area = inner(siblings);
    let before = app.selected();
    assert_eq!(
        app.mouse(left(area.x + 2, area.y + 1), t),
        Flow::Redraw,
        "{}",
        text_of(terminal.backend().buffer())
    );
    assert_ne!(app.selected(), before);
    assert_eq!(view.node(app.selected()).parent, view.node(before).parent);
    terminal
        .draw(|f| app.draw(f.area(), f.buffer_mut()))
        .unwrap();

    // A row of the legend filters by it, as `⏎` on it does.
    let area = inner(legend);
    assert_eq!(app.mouse(left(area.x + 2, area.y), t), Flow::Redraw);
    terminal
        .draw(|f| app.draw(f.area(), f.buffer_mut()))
        .unwrap();
    assert!(text_of(terminal.backend().buffer()).contains("filter: "));

    // The list view: a click on its row selects it.
    let mut app = viewer(&view, &mocha, &classes);
    let mut terminal = first_frame(&app, 120, 40);
    press(&mut app, &mut terminal, &char('2'));
    let table = inner(at.map_pane);
    let row = table.y + 2 + 1; // the second row of the table
    let before = app.selected();
    assert_eq!(app.mouse(left(table.x + 2, row), t), Flow::Redraw);
    assert_ne!(app.selected(), before);
    // Below the last row, and on the table's header: nothing.
    let at_header = table.y;
    assert_eq!(app.mouse(left(table.x + 2, at_header), t), Flow::Wait);

    // The types table: a click picks its row.
    press(&mut app, &mut terminal, &char('4'));
    let row = table.y + 2 + 1;
    assert_eq!(app.mouse(left(table.x + 2, row), t), Flow::Redraw);
    assert_eq!(app.types_at(), 1);

    // The largest view: a row selects its file.
    press(&mut app, &mut terminal, &char('3'));
    let row = table.y + 2 + 2;
    let before = app.selected();
    assert_eq!(app.mouse(left(table.x + 2, row), t), Flow::Redraw);
    assert_ne!(app.selected(), before);
}

#[test]
fn p4_tui_06_tile_at_reads_half_cells_and_a_pictures_pixels() {
    use duscope_treemap::Rect as R;
    use duscope_treemap::cells::CellBuffer;
    use duscope_treemap::paint::Drawn;

    use crate::zoom::Map;

    // A top tile and, under it, one inside an open box: the inside one is
    // the deepest at the point they share.
    let drawn = [
        Drawn {
            id: 1,
            parent: 0,
            rect: R::new(0, 0, 8, 4),
            kind: DrawnKind::Box { open: true },
        },
        Drawn {
            id: 2,
            parent: 1,
            rect: R::new(1, 1, 3, 2),
            kind: DrawnKind::Leaf,
        },
        Drawn {
            id: 3,
            parent: 0,
            rect: R::new(0, 4, 8, 4),
            kind: DrawnKind::Leaf,
        },
    ];
    let map = Map::of(0, CellBuffer::new(8, 4), R::new(0, 0, 8, 8), drawn);
    // The cell at row 0 is half-cells 0 and 1: the box's, and the leaf's
    // in the box, which the deepest wins.
    assert_eq!(tile_at(&map, None, 1, 0, None), Some(1));
    assert_eq!(tile_at(&map, None, 0, 0, None), Some(0));
    // The next tile's half-cells start at row 4 of the map (y = 2).
    assert_eq!(tile_at(&map, None, 2, 2, None), Some(2));
    assert_eq!(tile_at(&map, None, 2, 3, None), Some(2));
    // Outside.
    assert_eq!(tile_at(&map, None, 9, 0, None), None);

    // In pixels, the middle of the cell picks the tile, at 8×16 px a cell.
    let tiles = [
        Drawn {
            id: 1,
            parent: 0,
            rect: R::new(0, 0, 32, 32),
            kind: DrawnKind::Leaf,
        },
        Drawn {
            id: 2,
            parent: 0,
            rect: R::new(0, 32, 32, 32),
            kind: DrawnKind::Leaf,
        },
    ];
    let pixels = Map::of(0, CellBuffer::new(0, 0), R::new(0, 0, 32, 64), tiles);
    assert_eq!(tile_at(&pixels, None, 0, 0, Some((8, 16))), Some(0));
    assert_eq!(tile_at(&pixels, None, 0, 2, Some((8, 16))), Some(1));
    assert_eq!(tile_at(&pixels, None, 0, 4, Some((8, 16))), None);
}
