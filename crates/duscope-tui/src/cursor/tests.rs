//! P4-TUI-03: the beam rule on random layouts, the path, the zoom and its
//! memoised maps, the marks on the map, scripted navigation over the
//! canvas's example tree, and the same in pixels (`--treemap pixels`): the
//! marks drawn into the picture, the arrows on its tiles, the whole map's
//! colours when zoomed and the pictures kept per root.

use std::fmt::Write as _;
use std::path::{Path as FsPath, PathBuf};
use std::rc::Rc;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use duscope_core::arena::Arena;
use duscope_core::interchange::import::{ImportFormat, import};
use duscope_core::query::{ArenaView, SizeKind, TreeView, children};
use duscope_core::rollup::{RollUp, roll_up};
use duscope_core::rules::Classes;
use duscope_testkit::golden;
use duscope_treemap::cells::{Color, Depth};
use duscope_treemap::packs::Pack;
use duscope_treemap::paint::{
    Base, ColorBy, DrawnKind, Glyphs, PaintOpts, classes_of, paint_unlabelled, paint_viewer,
};
use duscope_treemap::{Algorithm, Rect, layout};
use proptest::prelude::*;
use ratatui_core::backend::TestBackend;
use ratatui_core::buffer::Buffer;
use ratatui_core::style::Color as Shown;
use ratatui_core::terminal::Terminal;

use super::{Dir, beam};
use crate::panes::tests::{classes, mockup, opts, status};
use crate::path::{Path, siblings};
use crate::zoom::{KEPT_PICTURES, Map, Mark, Zoom};
use crate::{App, Flow, snapshot};

// ---------------------------------------------------------------- the beam

/// Whether `to` lies wholly beyond `from`'s edge going `dir`.
fn beyond(from: Rect, to: Rect, dir: Dir) -> bool {
    match dir {
        Dir::Right => to.x >= from.x + from.w,
        Dir::Left => to.x + to.w <= from.x,
        Dir::Down => to.y >= from.y + from.h,
        Dir::Up => to.y + to.h <= from.y,
    }
}

/// The beam rule's order, written out: overlap across the move first, then
/// the gap to the near edge, then the distance between centres across.
fn rank(from: Rect, to: Rect, dir: Dir) -> (bool, u32, u32) {
    let span = |a: u16, b: u16| (u32::from(a), u32::from(a) + u32::from(b));
    let (fa, fx, ta, tx) = match dir {
        Dir::Left | Dir::Right => (
            span(from.x, from.w),
            span(from.y, from.h),
            span(to.x, to.w),
            span(to.y, to.h),
        ),
        Dir::Up | Dir::Down => (
            span(from.y, from.h),
            span(from.x, from.w),
            span(to.y, to.h),
            span(to.x, to.w),
        ),
    };
    let gap = match dir {
        Dir::Right | Dir::Down => ta.0 - fa.1,
        Dir::Left | Dir::Up => fa.0 - ta.1,
    };
    let overlap = tx.0 < fx.1 && fx.0 < tx.1;
    (!overlap, gap, (tx.0 + tx.1).abs_diff(fx.0 + fx.1))
}

/// Checks one move of the beam rule over `rects` from `rects[from]`:
/// it lands on a drawn rectangle beyond the edge that no other candidate
/// ranks before, or stays when none lies beyond.
fn check_move(rects: &[Rect], from: usize, dir: Dir) -> Result<Option<usize>, TestCaseError> {
    let cursor = rects[from];
    let others = rects
        .iter()
        .copied()
        .enumerate()
        .filter(|&(i, _)| i != from);
    let got = beam(cursor, dir, others.clone());
    let candidates: Vec<(usize, Rect)> = others
        .filter(|&(_, r)| r.w > 0 && r.h > 0 && beyond(cursor, r, dir))
        .collect();
    match got {
        None => prop_assert!(
            candidates.is_empty(),
            "stayed on {cursor:?} going {dir:?} with {candidates:?} beyond"
        ),
        Some(to) => {
            let rect = rects[to];
            prop_assert!(to != from);
            prop_assert!(rect.w > 0 && rect.h > 0, "landed on an empty {rect:?}");
            prop_assert!(
                beyond(cursor, rect, dir),
                "{rect:?} is not beyond {cursor:?}"
            );
            let best = candidates
                .iter()
                .map(|&(_, r)| rank(cursor, r, dir))
                .min()
                .unwrap();
            prop_assert_eq!(rank(cursor, rect, dir), best);
            // The first of the best.
            let first = candidates
                .iter()
                .find(|&&(_, r)| rank(cursor, r, dir) == best)
                .unwrap()
                .0;
            prop_assert_eq!(to, first);
        }
    }
    Ok(got)
}

/// Whether `rect`'s edge going `dir` is `area`'s.
fn at_edge(rect: Rect, area: Rect, dir: Dir) -> bool {
    match dir {
        Dir::Right => rect.x + rect.w == area.x + area.w,
        Dir::Left => rect.x == area.x,
        Dir::Down => rect.y + rect.h == area.y + area.h,
        Dir::Up => rect.y == area.y,
    }
}

fn block() -> impl Strategy<Value = (Vec<(u64, usize)>, Rect, Algorithm, u16)> {
    let weight = prop_oneof![8 => 1u64..1_000_000_000, 1 => 0u64..4];
    (
        prop::collection::vec(weight, 1..60),
        (0u16..20, 0u16..20, 1u16..120, 1u16..90),
        prop_oneof![Just(Algorithm::Squarify), Just(Algorithm::Gp)],
        1u16..5,
    )
        .prop_map(|(weights, (x, y, w, h), algorithm, min)| {
            let items = weights
                .into_iter()
                .enumerate()
                .map(|(i, w)| (w, i))
                .collect();
            (items, Rect::new(x, y, w, h), algorithm, min)
        })
}

fn rect() -> impl Strategy<Value = Rect> {
    (0u16..60, 0u16..60, 0u16..20, 0u16..20).prop_map(|(x, y, w, h)| Rect::new(x, y, w, h))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// On a block laid out as the map lays one out, every move lands on a
    /// tile beyond the cursor's edge, adjacent and overlapping it across,
    /// or stays: exactly when the cursor is at the block's edge.
    #[test]
    fn p4_tui_03_the_beam_rule_lands_on_a_tile_or_stays_on_random_layouts(
        (items, area, algorithm, min) in block()
    ) {
        let laid = layout(&items, area, algorithm, min);
        let rects: Vec<Rect> = laid
            .tiles
            .iter()
            .map(|t| t.rect)
            .chain(laid.small.iter().map(|s| s.rect))
            .collect();
        for from in 0..rects.len() {
            for dir in Dir::ALL {
                let got = check_move(&rects, from, dir)?;
                let edge = at_edge(rects[from], area, dir);
                prop_assert_eq!(got.is_none(), edge, "{:?} going {:?} in {:?}", rects[from], dir, area);
                if let Some(to) = got {
                    let (overlap_last, gap, _) = rank(rects[from], rects[to], dir);
                    prop_assert!(!overlap_last && gap == 0, "{:?} to {:?}", rects[from], rects[to]);
                }
            }
        }
    }

    /// On any rectangles, a move lands on the best candidate beyond the
    /// edge, or stays when there is none.
    #[test]
    fn p4_tui_03_the_beam_rule_picks_by_overlap_then_edge_then_centre(
        rects in prop::collection::vec(rect(), 1..30),
        from in 0usize..30,
    ) {
        let from = from % rects.len();
        for dir in Dir::ALL {
            check_move(&rects, from, dir)?;
        }
    }
}

#[test]
fn p4_tui_03_the_beam_rule_prefers_overlap_to_a_nearer_edge() {
    let cursor = Rect::new(10, 10, 4, 4);
    // Right: a near tile above the beam, a farther one in it.
    let near = Rect::new(15, 0, 2, 4);
    let far = Rect::new(20, 12, 2, 2);
    assert_eq!(beam(cursor, Dir::Right, [(0, near), (1, far)]), Some(1));
    // Without the one in the beam, the nearest edge.
    assert_eq!(beam(cursor, Dir::Right, [(0, near)]), Some(0));
    // In the beam at the same edge: the centre nearest the cursor's.
    let top = Rect::new(14, 8, 2, 3);
    let middle = Rect::new(14, 11, 2, 2);
    assert_eq!(beam(cursor, Dir::Right, [(0, top), (1, middle)]), Some(1));
    // Nothing beyond the edge: the cursor stays; a tile straddling the
    // edge is not beyond it.
    assert_eq!(beam(cursor, Dir::Left, [(0, near), (1, far)]), None);
    assert_eq!(
        beam(cursor, Dir::Down, [(0, Rect::new(12, 13, 4, 4))]),
        None
    );
    // An empty rectangle is no tile.
    assert_eq!(beam(cursor, Dir::Up, [(0, Rect::new(10, 2, 4, 0))]), None);
}

// ------------------------------------------------------------ the fixtures

/// One entry of a random tree: a file of some bytes, or a folder.
#[derive(Debug, Clone)]
enum T {
    F(u64),
    D(Vec<T>),
}

fn tree_strategy() -> impl Strategy<Value = Vec<T>> {
    let leaf = prop_oneof![
        6 => (1u64 << 10..1u64 << 30).prop_map(T::F),
        1 => (0u64..4096).prop_map(T::F),
    ];
    let node = leaf.prop_recursive(4, 160, 9, |inner| {
        prop::collection::vec(inner, 0..9).prop_map(T::D)
    });
    prop::collection::vec(node, 1..12)
}

/// `kids` as ncdu's export writes a folder's children.
fn json(kids: &[T], out: &mut String) {
    for (i, kid) in kids.iter().enumerate() {
        match kid {
            T::F(bytes) => {
                let _ = write!(out, r#",{{"name":"f{i}","asize":{bytes},"dsize":{bytes}}}"#);
            }
            T::D(kids) => {
                let _ = write!(out, r#",[{{"name":"d{i}"}}"#);
                json(kids, out);
                out.push(']');
            }
        }
    }
}

/// A scan of `kids` under `/scan`.
fn scan(kids: &[T]) -> (Arena, RollUp) {
    let mut text = String::from(r#"[1,2,{"progname":"ncdu"},[{"name":"/scan"}"#);
    json(kids, &mut text);
    text.push_str("]]");
    let mut arena = import(text.as_bytes(), Some(ImportFormat::Ncdu))
        .unwrap()
        .arena;
    let rollup = roll_up(&mut arena);
    (arena, rollup)
}

fn viewer<'a>(view: &'a ArenaView<'a>, opts: PaintOpts<'a>) -> App<'a> {
    App::new(
        view,
        opts,
        Depth::TrueColor,
        Some(FsPath::new("/Users/alex")),
        status(view),
        None,
    )
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn char(c: char) -> Event {
    key(KeyCode::Char(c))
}

/// A terminal of `w`×`h` with `app`'s first frame.
fn first_frame(app: &App<'_>, w: u16, h: u16) -> Terminal<TestBackend> {
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal
        .draw(|f| app.draw(f.area(), f.buffer_mut()))
        .unwrap();
    terminal
}

/// Feeds `event` to `app` and draws the frame it asks for.
fn press(app: &mut App<'_>, terminal: &mut Terminal<TestBackend>, event: &Event) -> Flow {
    let flow = app.on(event);
    if flow == Flow::Redraw {
        terminal
            .draw(|f| app.draw(f.area(), f.buffer_mut()))
            .unwrap();
    }
    flow
}

/// The id of the entry at `path` (`/`-separated, from the root).
fn id_of(view: &dyn TreeView, path: &str) -> u32 {
    (0..view.len())
        .find(|&id| view.path(id) == path.as_bytes())
        .unwrap_or_else(|| panic!("no {path}"))
}

fn name(view: &dyn TreeView, id: u32) -> String {
    String::from_utf8_lossy(&view.path(id)).into_owned()
}

// --------------------------------------------------- moves in the viewer

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// In the viewer, on random trees at random sizes, with and without
    /// `--pool-files` and a `--depth` cap, every key leaves the cursor
    /// under the map's root and shown on the map, and every arrow lands on
    /// another tile of the cursor's block or stays.
    #[test]
    fn p4_tui_03_every_move_in_the_viewer_lands_on_a_drawn_tile_or_stays(
        kids in tree_strategy(),
        (w, h) in (80u16..170, 24u16..50),
        (pool_files, depth, min_tile) in (any::<bool>(), prop::option::of(0u32..4), 1u16..5),
        keys in prop::collection::vec(0usize..16, 1..60),
    ) {
        const KEYS: [KeyCode; 16] = [
            KeyCode::Up, KeyCode::Down, KeyCode::Left, KeyCode::Right,
            KeyCode::Char('h'), KeyCode::Char('j'), KeyCode::Char('k'), KeyCode::Char('l'),
            KeyCode::Enter, KeyCode::Backspace, KeyCode::Esc, KeyCode::Char('z'),
            KeyCode::Char('Z'), KeyCode::Char('0'), KeyCode::Char('G'), KeyCode::PageUp,
        ];
        let tree = scan(&kids);
        let view = ArenaView::new(&tree.0, &tree.1, PathBuf::from("/scan"));
        let (mocha, classes) = (Pack::named("mocha").unwrap(), Classes::vendored());
        let opts = PaintOpts {
            pool_files,
            depth,
            min_tile,
            ..opts(&mocha, &classes, ColorBy::Type)
        };
        let mut app = viewer(&view, opts);
        let mut terminal = first_frame(&app, w, h);
        for k in keys {
            let before = app.selected();
            let map = app.map();
            let mark = map.mark(&view, before);
            let flow = press(&mut app, &mut terminal, &key(KEYS[k]));
            let after = app.selected();
            let root = app.cursor().zoom().root();
            let last = view.node(root).last_id;
            prop_assert!(root <= after && after <= last, "{after} outside {root}..={last}");
            let now = app.map();
            prop_assert!(now.root() == root);
            let shown = now.mark(&view, after);
            if let Mark::Tile(i) = shown {
                let r = now.tiles()[i].rect;
                prop_assert!(r.w > 0 && r.h > 0);
            }
            if k < 8 {
                // An arrow: on the same map, another tile of the block.
                if after == before {
                    prop_assert_eq!(flow, Flow::Wait);
                } else {
                    prop_assert_eq!(flow, Flow::Redraw);
                    prop_assert!(Rc::ptr_eq(&map, &now));
                    let parent = view.node(before).parent;
                    prop_assert_eq!(view.node(after).parent, parent);
                    let Mark::Tile(i) = shown else {
                        return Err(TestCaseError::fail("moved to the whole map"));
                    };
                    prop_assert!(map.block(parent).contains(&i));
                    prop_assert!(mark != shown);
                }
            }
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// In pixels, on random trees at random sizes, every arrow lands on
    /// another tile of the cursor's block in the picture, the map shown,
    /// or stays; every key leaves the cursor marked on a tile of the
    /// picture of the map's root, or on the whole picture.
    #[test]
    fn p4_tui_03_every_move_in_pixels_lands_on_a_tile_of_the_picture_or_stays(
        kids in tree_strategy(),
        (w, h) in (80u16..170, 24u16..50),
        (pool_files, depth) in (any::<bool>(), prop::option::of(0u32..4)),
        keys in prop::collection::vec(0usize..12, 1..40),
    ) {
        const KEYS: [KeyCode; 12] = [
            KeyCode::Up, KeyCode::Down, KeyCode::Left, KeyCode::Right,
            KeyCode::Up, KeyCode::Down, KeyCode::Left, KeyCode::Right,
            KeyCode::Enter, KeyCode::Backspace, KeyCode::Char('z'), KeyCode::Char('Z'),
        ];
        let tree = scan(&kids);
        let view = ArenaView::new(&tree.0, &tree.1, PathBuf::from("/scan"));
        let (mocha, classes) = (Pack::named("mocha").unwrap(), Classes::vendored());
        let opts = PaintOpts {
            pool_files,
            depth,
            ..opts(&mocha, &classes, ColorBy::Type)
        };
        let mut app = viewer(&view, opts);
        // Small cells keep the pictures small.
        app.draw_in_pixels_unencoded((4, 8));
        let mut terminal = first_frame(&app, w, h);
        for k in keys {
            let before = app.selected();
            let map = in_picture(&app);
            let mark = map.mark(&view, before);
            let flow = press(&mut app, &mut terminal, &key(KEYS[k]));
            let after = app.selected();
            let now = in_picture(&app);
            prop_assert_eq!(now.root(), app.cursor().zoom().root());
            let shown = now.mark(&view, after);
            let rect = now.rect(shown);
            prop_assert!(rect.w > 0 && rect.h > 0);
            let area = now.area();
            prop_assert!(rect.x + rect.w <= area.w && rect.y + rect.h <= area.h);
            if k >= 8 {
                continue;
            }
            // An arrow, by the beam rule on the picture's rectangles.
            let dir = [Dir::Up, Dir::Down, Dir::Left, Dir::Right][k % 4];
            let parent = view.node(before).parent;
            if after == before {
                prop_assert_eq!(flow, Flow::Wait);
                // Nothing beyond the edge, or a pool whose largest entry
                // is the cursor's own.
                if before != app.cursor().zoom().root() {
                    let candidates = map
                        .block(parent)
                        .iter()
                        .copied()
                        .filter(|&i| mark != Mark::Tile(i))
                        .map(|i| (i, map.tiles()[i].rect));
                    if let Some(i) = beam(map.rect(mark), dir, candidates) {
                        prop_assert!(
                            matches!(map.tiles()[i].kind, DrawnKind::Small | DrawnKind::Files),
                            "stayed with {:?} beyond",
                            map.tiles()[i]
                        );
                    }
                }
            } else {
                prop_assert_eq!(flow, Flow::Redraw);
                prop_assert!(Rc::ptr_eq(&map, &now));
                prop_assert_eq!(view.node(after).parent, parent);
                let Mark::Tile(i) = shown else {
                    return Err(TestCaseError::fail("moved to the whole picture"));
                };
                prop_assert!(map.block(parent).contains(&i));
                prop_assert!(mark != shown);
                prop_assert!(beyond(map.rect(mark), map.tiles()[i].rect, dir));
            }
        }
    }
}

// ---------------------------------------------------------------- the path

#[test]
fn p4_tui_03_the_path_goes_up_and_back_down_and_ends_where_the_cursor_moves() {
    let tree = mockup();
    let view = ArenaView::new(&tree.0, &tree.1, PathBuf::from("/Users/alex/Library"));
    let size = SizeKind::Allocated;
    let mut path = Path::largest(&view, size);
    let wal = id_of(&view, "Caches/com.apple.Safari/Cache.db-wal");
    let safari = id_of(&view, "Caches/com.apple.Safari");
    let caches = id_of(&view, "Caches");
    assert_eq!(path.cursor(), wal);
    assert_eq!(path.ids(), [0, caches, safari, wal]);
    assert_eq!(path, Path::to(&view, wal));
    // Up remembers the way down; down takes it back.
    assert!(path.up() && path.up());
    assert_eq!((path.cursor(), path.depth()), (caches, 1));
    assert_eq!(path.ancestors(), [0]);
    assert!(path.down(&view, size));
    assert_eq!(path.cursor(), safari);
    // Moving sideways ends the path there; down then takes the largest
    // child, the siblings pane's first row.
    let homebrew = id_of(&view, "Caches/Homebrew");
    path.select(homebrew);
    assert_eq!(path.ids(), [0, caches, homebrew]);
    assert!(path.down(&view, size));
    let downloads = id_of(&view, "Caches/Homebrew/downloads");
    assert_eq!(path.cursor(), downloads);
    assert_eq!(siblings(&view, homebrew, size)[0], downloads);
    // A file has nowhere down to go; the root nowhere up.
    while path.down(&view, size) {}
    assert!(!view.node(path.cursor()).is_dir());
    while path.up() {}
    assert_eq!(path.cursor(), 0);
    assert!(!path.up());
    // Jumping onto the path keeps the way down; elsewhere starts a new one.
    path.jump(&view, caches);
    assert_eq!(path.ids().len(), 5);
    path.jump(&view, safari);
    assert_eq!(path.ids(), [0, caches, safari]);
}

#[test]
fn p4_tui_03_siblings_are_largest_first_and_equals_keep_their_order() {
    let kids = vec![T::F(10), T::F(30), T::F(10), T::D(vec![T::F(30)]), T::F(5)];
    let tree = scan(&kids);
    let view = ArenaView::new(&tree.0, &tree.1, PathBuf::from("/scan"));
    let names: Vec<String> = siblings(&view, 0, SizeKind::Apparent)
        .into_iter()
        .map(|id| name(&view, id))
        .collect();
    // Files by name, then folders: f1 and d3 weigh 30, f0 and f2 10.
    assert_eq!(names, ["f1", "d3", "f0", "f2", "f4"]);
    assert_eq!(
        siblings(&view, id_of(&view, "f0"), SizeKind::Apparent),
        Vec::<u32>::new()
    );
}

// ---------------------------------------------------------------- the zoom

#[test]
fn p4_tui_03_the_zoom_stack_pushes_pops_and_resets() {
    let mut zoom = Zoom::default();
    assert_eq!((zoom.root(), zoom.depth()), (0, 0));
    assert_eq!(zoom.pop(), None);
    assert_eq!(zoom.reset(), None);
    zoom.push(5, 7);
    zoom.push(9, 11);
    assert_eq!((zoom.root(), zoom.depth()), (9, 2));
    assert_eq!(zoom.pop(), Some((5, 11)));
    assert_eq!(zoom.root(), 5);
    zoom.push(9, 12);
    assert_eq!(zoom.reset(), Some((0, 7)));
    assert_eq!((zoom.root(), zoom.depth()), (0, 0));
}

#[test]
fn p4_tui_03_z_zooms_to_the_cursor_and_back_with_maps_painted_once() {
    let tree = mockup();
    let view = ArenaView::new(&tree.0, &tree.1, PathBuf::from("/Users/alex/Library"));
    let (mocha, classes) = (Pack::named("mocha").unwrap(), classes());
    let mut app = viewer(&view, opts(&mocha, &classes, ColorBy::Type));
    let mut terminal = first_frame(&app, 120, 40);
    let wal = id_of(&view, "Caches/com.apple.Safari/Cache.db-wal");
    let safari = id_of(&view, "Caches/com.apple.Safari");
    let caches = id_of(&view, "Caches");
    let outer = app.map();
    assert_eq!(outer.root(), 0);
    // z on a file zooms to its folder; the cursor stays.
    assert_eq!(press(&mut app, &mut terminal, &char('z')), Flow::Redraw);
    assert_eq!((app.cursor().zoom().root(), app.selected()), (safari, wal));
    let safari_map = app.map();
    assert_eq!(safari_map.root(), safari);
    assert!(matches!(safari_map.mark(&view, wal), Mark::Tile(_)));
    // The title names the folder the map is of.
    let text = snapshot(terminal.backend().buffer());
    assert!(
        text.contains("treemap · ~/Library/Caches/com.apple.Safari · color"),
        "{text}"
    );
    // z on a file of the map's root: nothing to zoom to.
    assert_eq!(app.on(&char('z')), Flow::Wait);
    // ⌫ to the map's root, then ⌫ zooms out to reach its parent.
    assert_eq!(
        press(&mut app, &mut terminal, &key(KeyCode::Backspace)),
        Flow::Redraw
    );
    assert_eq!(app.selected(), safari);
    assert_eq!(
        press(&mut app, &mut terminal, &key(KeyCode::Backspace)),
        Flow::Redraw
    );
    assert_eq!((app.cursor().zoom().root(), app.selected()), (0, caches));
    // The outer map was kept, not painted again.
    assert!(Rc::ptr_eq(&outer, &app.map()));
    // z on a folder zooms to it and the cursor goes in, along the path.
    assert_eq!(press(&mut app, &mut terminal, &char('z')), Flow::Redraw);
    assert_eq!(
        (app.cursor().zoom().root(), app.selected()),
        (caches, safari)
    );
    assert_eq!(press(&mut app, &mut terminal, &char('z')), Flow::Redraw);
    assert_eq!((app.cursor().zoom().root(), app.selected()), (safari, wal));
    assert!(Rc::ptr_eq(&safari_map, &app.map()));
    assert_eq!(app.cursor().kept(), 3);
    // Z puts back the root and the cursor z found; 0 all the way.
    assert_eq!(press(&mut app, &mut terminal, &char('Z')), Flow::Redraw);
    assert_eq!(
        (app.cursor().zoom().root(), app.selected()),
        (caches, safari)
    );
    assert_eq!(press(&mut app, &mut terminal, &char('z')), Flow::Redraw);
    assert_eq!(press(&mut app, &mut terminal, &char('0')), Flow::Redraw);
    assert_eq!((app.cursor().zoom().root(), app.selected()), (0, caches));
    assert_eq!(app.on(&char('0')), Flow::Wait);
    assert_eq!(app.on(&char('Z')), Flow::Wait);
    // ⏎ takes the remembered way down; at the scan's root ⌫ stays.
    assert_eq!(
        press(&mut app, &mut terminal, &key(KeyCode::Enter)),
        Flow::Redraw
    );
    assert_eq!(app.selected(), safari);
    for _ in 0..2 {
        press(&mut app, &mut terminal, &key(KeyCode::Backspace));
    }
    assert_eq!(app.selected(), 0);
    assert_eq!(app.on(&key(KeyCode::Backspace)), Flow::Wait);
    // A resize paints the maps again, at the new size.
    terminal.backend_mut().resize(160, 45);
    terminal
        .draw(|f| app.draw(f.area(), f.buffer_mut()))
        .unwrap();
    assert!(!Rc::ptr_eq(&outer, &app.map()));
    assert_eq!(app.cursor().kept(), 1);
}

#[test]
fn p4_tui_03_a_zoomed_map_keeps_the_whole_maps_colours() {
    let tree = mockup();
    let view = ArenaView::new(&tree.0, &tree.1, PathBuf::from("/Users/alex/Library"));
    let (mocha, classes) = (Pack::named("mocha").unwrap(), classes());
    for color_by in [ColorBy::Folder, ColorBy::Level] {
        let mut app = viewer(&view, opts(&mocha, &classes, color_by.clone()));
        let mut terminal = first_frame(&app, 120, 40);
        // Zoom to Caches/Homebrew: its tiles take Caches' colour under
        // folder, their own depth's under level.
        app.jump(id_of(&view, "Caches/Homebrew"));
        press(&mut app, &mut terminal, &char('z'));
        let map = app.map();
        let downloads = id_of(&view, "Caches/Homebrew/downloads");
        let Mark::Tile(i) = map.mark(&view, downloads) else {
            panic!("downloads has no tile");
        };
        assert_eq!(map.tiles()[i].kind, DrawnKind::Box { open: true });
        let bottle = id_of(
            &view,
            "Caches/Homebrew/downloads/llvm@19--19.1.2.arm64_sequoia.bottle.tar.gz",
        );
        let Mark::Tile(i) = map.mark(&view, bottle) else {
            panic!("the bottle has no tile");
        };
        let rect = map.tiles()[i].rect;
        let cell = map
            .cells
            .get(rect.x + rect.w / 2, rect.y.div_ceil(2))
            .unwrap();
        // Caches is the largest top-level folder; the bottle is at depth 4.
        let expected = match color_by {
            ColorBy::Folder => mocha.cycle[0],
            _ => mocha.cycle[4],
        };
        assert!(
            cell.bg == expected || cell.fg == expected,
            "{color_by:?}: {cell:?}"
        );
    }
}

#[test]
fn p4_tui_03_the_viewers_map_names_every_tile_it_draws() {
    let tree = mockup();
    let view = ArenaView::new(&tree.0, &tree.1, PathBuf::from("/Users/alex/Library"));
    let (mocha, classes) = (Pack::named("mocha").unwrap(), classes());
    let opts = opts(&mocha, &classes, ColorBy::Type);
    let class_of = classes_of(&view, &classes);
    for (w, h) in [(78, 13), (112, 40), (40, 10)] {
        let (cells, drawn) = paint_viewer(&view, &opts, w, h, &class_of, Base::default());
        // The same cells as the map without names.
        assert_eq!(
            cells,
            paint_unlabelled(&view, &opts, w, h, &class_of),
            "{w}×{h}"
        );
        // Each tile in its block: a child of its folder, or the folder's
        // pool; inside the map; no two in a block overlap.
        for (i, a) in drawn.iter().enumerate() {
            assert!(a.rect.w > 0 && a.rect.h > 0);
            assert!(a.rect.x + a.rect.w <= w && a.rect.y + a.rect.h <= h * 2);
            match a.kind {
                DrawnKind::Small | DrawnKind::Files => assert_eq!(a.id, a.parent),
                _ => assert_eq!(view.node(a.id).parent, a.parent),
            }
            for b in &drawn[i + 1..] {
                if a.parent == b.parent {
                    let apart = a.rect.x + a.rect.w <= b.rect.x
                        || b.rect.x + b.rect.w <= a.rect.x
                        || a.rect.y + a.rect.h <= b.rect.y
                        || b.rect.y + b.rect.h <= a.rect.y;
                    assert!(apart, "{a:?} overlaps {b:?}");
                }
            }
        }
        // Every child of an opened folder is shown in its block.
        let opened: Vec<u32> = std::iter::once(0)
            .chain(drawn.iter().filter_map(|d| match d.kind {
                DrawnKind::Box { open: true } | DrawnKind::Gutter => Some(d.id),
                _ => None,
            }))
            .collect();
        for dir in opened {
            let node = view.node(dir);
            for kid in children(&view, &node) {
                let own = drawn.iter().any(|d| d.id == kid && d.parent == dir);
                let pooled = drawn
                    .iter()
                    .any(|d| d.parent == dir && d.kind == DrawnKind::Small);
                assert!(own || pooled, "{} is not shown", name(&view, kid));
            }
        }
    }
}

// ------------------------------------------------------- keys and marks

#[test]
fn p4_tui_03_g_g_home_end_and_pages_walk_the_siblings_list() {
    // Sixty files halving every three: the last are too small to draw.
    let kids: Vec<T> = (0..60u32).map(|i| T::F(1u64 << (40 - i / 3))).collect();
    let tree = scan(&kids);
    let view = ArenaView::new(&tree.0, &tree.1, PathBuf::from("/scan"));
    let (mocha, classes) = (Pack::named("mocha").unwrap(), Classes::vendored());
    let mut app = viewer(&view, opts(&mocha, &classes, ColorBy::Type));
    let mut terminal = first_frame(&app, 80, 24);
    let order = siblings(&view, 0, SizeKind::Allocated);
    let at = |app: &App<'_>| order.iter().position(|&id| id == app.selected()).unwrap();
    assert_eq!(at(&app), 0);
    for (event, expected, flow) in [
        (key(KeyCode::PageDown), 9, Flow::Redraw),
        (key(KeyCode::PageDown), 18, Flow::Redraw),
        (key(KeyCode::PageUp), 9, Flow::Redraw),
        (char('G'), 59, Flow::Redraw),
        (key(KeyCode::PageDown), 59, Flow::Wait),
        (key(KeyCode::Home), 0, Flow::Redraw),
        (key(KeyCode::End), 59, Flow::Redraw),
        (char('g'), 0, Flow::Redraw),
        (key(KeyCode::PageUp), 0, Flow::Wait),
    ] {
        assert_eq!(press(&mut app, &mut terminal, &event), flow, "{event:?}");
        assert_eq!(at(&app), expected, "{event:?}");
    }
    // The last, too small for a tile, is marked on the pool.
    press(&mut app, &mut terminal, &char('G'));
    let map = app.map();
    let Mark::Tile(pool) = map.mark(&view, app.selected()) else {
        panic!("no mark");
    };
    assert_eq!(map.tiles()[pool].kind, DrawnKind::Small);
    // A move onto the pool lands on the largest entry in it.
    let largest_pooled = order
        .iter()
        .copied()
        .find(|&id| map.mark(&view, id) == Mark::Tile(pool))
        .unwrap();
    assert_ne!(largest_pooled, app.selected());
    let rects = || map.block(0).iter().map(|&i| (i, map.tiles()[i].rect));
    let (from, dir) = map
        .block(0)
        .iter()
        .filter(|&&i| i != pool)
        .find_map(|&i| {
            Dir::ALL.into_iter().find_map(|dir| {
                let others = rects().filter(|&(j, _)| j != i);
                (beam(map.tiles()[i].rect, dir, others) == Some(pool)).then_some((i, dir))
            })
        })
        .unwrap();
    app.jump(map.tiles()[from].id);
    let code = match dir {
        Dir::Up => KeyCode::Up,
        Dir::Down => KeyCode::Down,
        Dir::Left => KeyCode::Left,
        Dir::Right => KeyCode::Right,
    };
    assert_eq!(press(&mut app, &mut terminal, &key(code)), Flow::Redraw);
    assert_eq!(app.selected(), largest_pooled);
}

#[test]
fn p4_tui_03_esc_clears_the_filter_before_it_ascends() {
    let tree = mockup();
    let view = ArenaView::new(&tree.0, &tree.1, PathBuf::from("/Users/alex/Library"));
    let (mocha, classes) = (Pack::named("mocha").unwrap(), classes());
    let mut app = viewer(&view, opts(&mocha, &classes, ColorBy::Type));
    let wal = app.selected();
    app.filter = Some("*.db".to_owned());
    assert_eq!(app.on(&key(KeyCode::Esc)), Flow::Redraw);
    assert_eq!((app.filter.as_deref(), app.selected()), (None, wal));
    assert_eq!(app.on(&key(KeyCode::Esc)), Flow::Redraw);
    assert_eq!(app.selected(), id_of(&view, "Caches/com.apple.Safari"));
    // With Ctrl or Alt the keys are not the navigation's.
    for code in [KeyCode::Esc, KeyCode::Enter, KeyCode::Char('z')] {
        for modifiers in [KeyModifiers::CONTROL, KeyModifiers::ALT] {
            assert_eq!(
                app.on(&Event::Key(KeyEvent::new(code, modifiers))),
                Flow::Wait
            );
        }
    }
    // Shift-z and Shift-g are Z and G.
    let shifted = |c| Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::SHIFT));
    assert_eq!(app.on(&shifted('G')), Flow::Redraw);
    assert_eq!(app.on(&shifted('Z')), Flow::Wait);
}

/// The colours of the cells of `buf` on row `y`, columns `xs`.
fn fgs(buf: &Buffer, y: u16, xs: std::ops::Range<u16>) -> Vec<Shown> {
    xs.map(|x| buf[(x, y)].fg).collect()
}

#[test]
fn p4_tui_03_the_cursor_is_an_accent_outline_and_its_ancestors_borders_link() {
    let tree = mockup();
    let view = ArenaView::new(&tree.0, &tree.1, PathBuf::from("/Users/alex/Library"));
    let (mocha, classes) = (Pack::named("mocha").unwrap(), classes());
    let mut app = viewer(&view, opts(&mocha, &classes, ColorBy::Type));
    let mut terminal = first_frame(&app, 160, 45);
    let accent = crate::app::color(mocha.roles.accent, Depth::TrueColor);
    let link = crate::app::color(mocha.roles.link, Depth::TrueColor);
    // Up to com.apple.Safari, a box inside the Caches box: an outline
    // with its tag; Caches' border and title in the link colour.
    press(&mut app, &mut terminal, &key(KeyCode::Backspace));
    let map = app.map();
    let screen = crate::layout::layout(160, 45).map;
    let cells = |id: u32| {
        let Mark::Tile(i) = map.mark(&view, id) else {
            panic!("no tile");
        };
        let r = map.tiles()[i].rect;
        let top = r.y.div_ceil(2);
        let bottom = r.y.midpoint(r.h);
        (screen.x + r.x, screen.y + top, r.w, bottom - top)
    };
    let buf = terminal.backend().buffer().clone();
    let (x, y, w, h) = cells(app.selected());
    assert_eq!(buf[(x, y)].symbol(), "┏");
    assert_eq!(buf[(x + w - 1, y + h - 1)].symbol(), "┛");
    assert!(fgs(&buf, y + h - 1, x..x + w).iter().all(|&c| c == accent));
    let tag: String = (x + 1..x + w - 1).map(|cx| buf[(cx, y)].symbol()).collect();
    assert!(tag.starts_with(" com.apple.Safari · 3.6G "), "{tag}");
    assert_eq!(buf[(x + 2, y)].bg, accent);
    let (x, y, w, h) = cells(id_of(&view, "Caches"));
    assert!(fgs(&buf, y + h - 1, x..x + w).iter().all(|&c| c == link));
    let title: String = (x + 2..x + 16).map(|cx| buf[(cx, y)].symbol()).collect();
    assert!(title.starts_with(" Caches · "), "{title}");
    assert!(fgs(&buf, y, x + 3..x + 9).iter().all(|&c| c == link));
    // The other depth-1 boxes keep overlay1.
    let overlay1 = crate::app::color(mocha.roles.overlay1, Depth::TrueColor);
    let (x, y, w, _) = cells(id_of(&view, "Application Support"));
    assert!(
        fgs(&buf, y, x + 1..x + 2).iter().all(|&c| c == overlay1),
        "{:?}",
        fgs(&buf, y, x..x + w)
    );
    // Without UTF-8 the outline is ASCII.
    let ascii = PaintOpts {
        glyphs: Glyphs::Ascii,
        ..opts(&mocha, &classes, ColorBy::Type)
    };
    let mut app = viewer(&view, ascii);
    let mut terminal = first_frame(&app, 160, 45);
    press(&mut app, &mut terminal, &key(KeyCode::Backspace));
    let text = snapshot(terminal.backend().buffer());
    let screen_text = text.split("--- styles").next().unwrap();
    assert!(screen_text.is_ascii());
    assert!(
        screen_text.contains("# com.apple.Safari - 3.6G #"),
        "{screen_text}"
    );
}

// ------------------------------------------------------- the map in pixels

/// A cell of the pictures, in pixels.
const CELL: (u16, u16) = (10, 20);

/// The tiles of the picture `app` shows, as the map the cursor moves on.
fn in_picture(app: &App<'_>) -> Rc<Map> {
    let shown = app.shown().unwrap();
    let (_, tiles) = shown.as_ref().as_ref().unwrap();
    Rc::clone(tiles)
}

fn rgb(color: Color) -> (u8, u8, u8) {
    match color {
        Color::Rgb(r, g, b) => (r, g, b),
        other => panic!("{other:?} is not RGB"),
    }
}

/// The hue of `rgb`, in degrees.
fn hue((r8, g8, b8): (u8, u8, u8)) -> f64 {
    let (top, bottom) = (r8.max(g8).max(b8), r8.min(g8).min(b8));
    let d = f64::from(top - bottom);
    let (r, g, b) = (f64::from(r8), f64::from(g8), f64::from(b8));
    let sector = if top == bottom {
        0.0
    } else if top == r8 {
        ((g - b) / d).rem_euclid(6.0)
    } else if top == g8 {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    sector * 60.0
}

/// Which of `cycle`'s colours a tile of the picture is shaded in: the
/// nearest in hue to its pixel by the bottom-right corner, where the
/// shading darkens the colour without turning its hue and no label
/// reaches.
fn shade_of(raster: &duscope_treemap::pixels::Raster, r: Rect, cycle: &[Color]) -> usize {
    let px = raster
        .pixel(u32::from(r.x + r.w - 2), u32::from(r.y + r.h - 2))
        .unwrap();
    let at = hue(px);
    let off = |c: &Color| {
        let d = (hue(rgb(*c)) - at).abs();
        d.min(360.0 - d)
    };
    (0..cycle.len())
        .min_by(|&a, &b| off(&cycle[a]).total_cmp(&off(&cycle[b])))
        .unwrap()
}

// One scene, checked step by step.
#[allow(clippy::too_many_lines)]
#[test]
fn p4_tui_03_in_pixels_the_cursor_is_marked_in_the_picture_and_moves_on_its_tiles() {
    let tree = mockup();
    let view = ArenaView::new(&tree.0, &tree.1, PathBuf::from("/Users/alex/Library"));
    let (mocha, classes) = (Pack::named("mocha").unwrap(), classes());
    let mut app = viewer(&view, opts(&mocha, &classes, ColorBy::Type));
    app.draw_in_pixels_unencoded(CELL);
    let mut terminal = first_frame(&app, 160, 45);
    let (accent, link, crust) = (
        rgb(mocha.roles.accent),
        rgb(mocha.roles.link),
        rgb(mocha.roles.crust),
    );
    // Up to com.apple.Safari, a folder inside Caches.
    press(&mut app, &mut terminal, &key(KeyCode::Backspace));
    let tiles = in_picture(&app);
    let shown = app.shown().unwrap();
    let (picture, _) = shown.as_ref().as_ref().unwrap();
    let plain = picture.raster();
    let marked = app.marked().unwrap();
    let rect_of = |map: &Map, id: u32| {
        let Mark::Tile(i) = map.mark(&view, id) else {
            panic!("{} has no tile", name(&view, id));
        };
        map.tiles()[i].rect
    };
    let px = |r: &duscope_treemap::pixels::Raster, x: u16, y: u16| {
        r.pixel(u32::from(x), u32::from(y)).unwrap()
    };
    // An accent outline 3 pixels thick (an eighth of the cell's 20, at
    // least 2) just inside the folder's tile, a line of crust inside it.
    let safari = rect_of(&tiles, app.selected());
    assert_eq!(
        tiles.tiles()[tiles.own(app.selected()).unwrap()].kind,
        DrawnKind::Box { open: true }
    );
    let (mid_x, mid_y) = (safari.x + safari.w / 2, safari.y + safari.h / 2);
    for d in 0..3 {
        assert_eq!(px(&marked, mid_x, safari.y + d), accent);
        assert_eq!(px(&marked, mid_x, safari.y + safari.h - 1 - d), accent);
        assert_eq!(px(&marked, safari.x + d, mid_y), accent);
        assert_eq!(px(&marked, safari.x + safari.w - 1 - d, mid_y), accent);
    }
    assert_eq!(px(&marked, mid_x, safari.y + 3), crust);
    assert_eq!(px(&marked, safari.x + 3, mid_y), crust);
    // Caches, its parent, outlined in the link colour, a pixel thick:
    // along its edges where Safari's outline is not.
    let caches = rect_of(&tiles, id_of(&view, "Caches"));
    let in_safari = |(x, y): (u16, u16)| {
        (safari.x..safari.x + safari.w).contains(&x) && (safari.y..safari.y + safari.h).contains(&y)
    };
    let edge: Vec<(u16, u16)> = (caches.x + 2..caches.x + caches.w - 2)
        .flat_map(|x| [(x, caches.y), (x, caches.y + caches.h - 1)])
        .chain(
            (caches.y + 2..caches.y + caches.h - 2)
                .flat_map(|y| [(caches.x, y), (caches.x + caches.w - 1, y)]),
        )
        .filter(|&p| !in_safari(p))
        .collect();
    assert!(edge.len() > 100, "{caches:?} {safari:?}");
    for &(x, y) in &edge {
        assert_eq!(px(&marked, x, y), link, "{x},{y}");
    }
    assert!(edge.iter().any(|&(x, y)| px(plain, x, y) != link));
    // A pixel inside the edge is the picture's.
    let (x, y) = edge[0];
    let below = if y == caches.y { y + 1 } else { y - 1 };
    if !in_safari((x, below)) {
        assert_eq!(px(&marked, x, below), px(plain, x, below));
    }
    // Away from the marks the picture is as drawn.
    let other = rect_of(&tiles, id_of(&view, "Application Support"));
    let inside = (other.x + other.w / 2, other.y + other.h / 2);
    assert_eq!(
        px(&marked, inside.0, inside.1),
        px(plain, inside.0, inside.1)
    );
    // An arrow moves among the picture's tiles, by the beam rule on them,
    // and the marks move with it.
    let parent = view.node(app.selected()).parent;
    let own = tiles.own(app.selected());
    let candidates: Vec<(usize, Rect)> = tiles
        .block(parent)
        .iter()
        .copied()
        .filter(|&i| own != Some(i))
        .map(|i| (i, tiles.tiles()[i].rect))
        .collect();
    let (dir, code, to) = [
        (Dir::Right, KeyCode::Right),
        (Dir::Down, KeyCode::Down),
        (Dir::Left, KeyCode::Left),
        (Dir::Up, KeyCode::Up),
    ]
    .into_iter()
    .find_map(|(dir, code)| beam(safari, dir, candidates.iter().copied()).map(|to| (dir, code, to)))
    .expect("com.apple.Safari has a neighbour in the picture");
    assert_eq!(
        press(&mut app, &mut terminal, &key(code)),
        Flow::Redraw,
        "{dir:?}"
    );
    assert_eq!(app.selected(), tiles.tiles()[to].id, "{dir:?}");
    let moved = app.marked().unwrap();
    let there = tiles.tiles()[to].rect;
    assert_eq!(px(&moved, there.x + there.w / 2, there.y), accent);
    // Safari's outline is gone, but where Caches' is.
    let on_caches = |(x, y): (u16, u16)| {
        x == caches.x
            || y == caches.y
            || x == caches.x + caches.w - 1
            || y == caches.y + caches.h - 1
    };
    let was: Vec<(u16, u16)> = (safari.x..safari.x + safari.w)
        .flat_map(|x| [(x, safari.y), (x, safari.y + safari.h - 1)])
        .chain(
            (safari.y..safari.y + safari.h)
                .flat_map(|y| [(safari.x, y), (safari.x + safari.w - 1, y)]),
        )
        .filter(|&p| !on_caches(p))
        .filter(|&(x, y)| {
            !((there.x..there.x + there.w).contains(&x)
                && (there.y..there.y + there.h).contains(&y))
        })
        .collect();
    assert!(!was.is_empty());
    for (x, y) in was {
        assert_eq!(px(&moved, x, y), px(plain, x, y), "{x},{y}");
    }
}

#[test]
fn p4_tui_03_a_zoomed_picture_keeps_the_whole_maps_colours() {
    let tree = mockup();
    let view = ArenaView::new(&tree.0, &tree.1, PathBuf::from("/Users/alex/Library"));
    let (mocha, classes) = (Pack::named("mocha").unwrap(), classes());
    let homebrew = id_of(&view, "Caches/Homebrew");
    let bottle = id_of(
        &view,
        "Caches/Homebrew/downloads/llvm@19--19.1.2.arm64_sequoia.bottle.tar.gz",
    );
    for color_by in [ColorBy::Folder, ColorBy::Level] {
        let mut app = viewer(&view, opts(&mocha, &classes, color_by.clone()));
        app.draw_in_pixels_unencoded(CELL);
        let mut terminal = first_frame(&app, 120, 40);
        // Each leaf tile's colour on the whole map, then zoomed to
        // Caches/Homebrew.
        let shades = |app: &App<'_>| {
            let shown = app.shown().unwrap();
            let (picture, tiles) = shown.as_ref().as_ref().unwrap();
            tiles
                .tiles()
                .iter()
                .filter(|t| t.kind == DrawnKind::Leaf && t.rect.w >= 4 && t.rect.h >= 4)
                .map(|t| (t.id, shade_of(picture.raster(), t.rect, &mocha.cycle)))
                .collect::<std::collections::HashMap<u32, usize>>()
        };
        let whole = shades(&app);
        app.jump(homebrew);
        press(&mut app, &mut terminal, &char('z'));
        assert_eq!(app.cursor().zoom().root(), homebrew);
        let zoomed = shades(&app);
        let both: Vec<u32> = zoomed
            .keys()
            .copied()
            .filter(|id| whole.contains_key(id))
            .collect();
        assert!(both.len() >= 3, "{color_by:?}: {both:?}");
        for id in both {
            assert_eq!(
                zoomed[&id],
                whole[&id],
                "{color_by:?}: {} takes another colour zoomed",
                name(&view, id)
            );
        }
        // Caches is the largest top-level folder; the bottle is at depth 4.
        let expected = match color_by {
            ColorBy::Folder => 0,
            _ => 4,
        };
        assert_eq!(zoomed[&bottle], expected, "{color_by:?}");
    }
}

#[test]
fn p4_tui_03_pictures_are_drawn_once_per_root_and_kept() {
    let tree = mockup();
    let view = ArenaView::new(&tree.0, &tree.1, PathBuf::from("/Users/alex/Library"));
    let (mocha, classes) = (Pack::named("mocha").unwrap(), classes());
    let mut app = viewer(&view, opts(&mocha, &classes, ColorBy::Type));
    app.draw_in_pixels_unencoded(CELL);
    let mut terminal = first_frame(&app, 120, 40);
    let whole = app.shown().unwrap();
    // z draws the zoomed folder's picture; Z and 0 find the whole map's
    // as it was drawn, and z the folder's again.
    let safari = id_of(&view, "Caches/com.apple.Safari");
    press(&mut app, &mut terminal, &char('z'));
    assert_eq!(app.cursor().zoom().root(), safari);
    let inner = app.shown().unwrap();
    assert!(!Rc::ptr_eq(&whole, &inner));
    press(&mut app, &mut terminal, &char('Z'));
    assert!(Rc::ptr_eq(&whole, &app.shown().unwrap()));
    press(&mut app, &mut terminal, &char('z'));
    assert!(Rc::ptr_eq(&inner, &app.shown().unwrap()));
    press(&mut app, &mut terminal, &char('0'));
    assert!(Rc::ptr_eq(&whole, &app.shown().unwrap()));
    assert_eq!(app.pictures_kept(), 2);
    // The scan root's picture and the three other roots' visited last are
    // kept: zooming to five folders in turn keeps the root's.
    let folders: Vec<u32> = (0..view.len())
        .filter(|&id| id != 0 && view.node(id).is_dir() && view.node(id).last_id > id)
        .take(5)
        .collect();
    assert_eq!(folders.len(), 5);
    for &folder in &folders {
        app.jump(folder);
        press(&mut app, &mut terminal, &char('z'));
        press(&mut app, &mut terminal, &char('0'));
    }
    assert_eq!(app.pictures_kept(), KEPT_PICTURES);
    assert!(Rc::ptr_eq(&whole, &app.shown().unwrap()));
    // A resize draws them again.
    terminal.backend_mut().resize(160, 45);
    terminal
        .draw(|f| app.draw(f.area(), f.buffer_mut()))
        .unwrap();
    assert!(!Rc::ptr_eq(&whole, &app.shown().unwrap()));
    assert_eq!(app.pictures_kept(), 1);
}

// --------------------------------------------------- scripted navigation

/// The script: the keys pressed in order, each named for the snapshot.
const SCRIPT: [(&str, KeyCode); 9] = [
    ("→", KeyCode::Right),
    ("⌫", KeyCode::Backspace),
    ("↓", KeyCode::Down),
    ("⏎", KeyCode::Enter),
    ("z", KeyCode::Char('z')),
    ("G", KeyCode::Char('G')),
    ("Z", KeyCode::Char('Z')),
    ("⌫ at depth 1, then z", KeyCode::Char('z')),
    ("0", KeyCode::Char('0')),
];

#[test]
fn p4_tui_03_scripted_navigation_snapshots() {
    let tree = mockup();
    let view = ArenaView::new(&tree.0, &tree.1, PathBuf::from("/Users/alex/Library"));
    let (mocha, classes) = (Pack::named("mocha").unwrap(), classes());
    for (w, h) in [(80, 24), (120, 40)] {
        let mut app = viewer(&view, opts(&mocha, &classes, ColorBy::Type));
        let mut terminal = first_frame(&app, w, h);
        let mut out = format!("=== start: {}\n", name(&view, app.selected()));
        out.push_str(&snapshot(terminal.backend().buffer()));
        for (label, code) in SCRIPT {
            if label.starts_with("⌫ at depth 1") {
                while view.node(app.selected()).depth > 1 {
                    press(&mut app, &mut terminal, &key(KeyCode::Backspace));
                }
            }
            let flow = press(&mut app, &mut terminal, &key(code));
            let _ = writeln!(
                out,
                "=== {label}: {:?}, {}{}",
                flow,
                name(&view, app.selected()),
                match app.cursor().zoom().root() {
                    0 => String::new(),
                    root => format!(" (zoomed to {})", name(&view, root)),
                }
            );
            out.push_str(&snapshot(terminal.backend().buffer()));
        }
        golden(&format!("view_nav_mockup_{w}x{h}.snap"), &out);
    }
}

// ------------------------------------------------------------ the budget

/// `count` folders with children, spread over `view`'s ids.
fn folders(view: &dyn TreeView, count: usize) -> Vec<u32> {
    let mut folders = Vec::new();
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    while folders.len() < count {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let id = u32::try_from(seed % u64::from(view.len())).unwrap();
        let node = view.node(id);
        if id != 0 && node.is_dir() && node.last_id > id {
            folders.push(id);
        }
    }
    folders
}

/// A saved scan of T2 (`bench/.trees/T2`), in `env`'s index folder, made
/// by the release binary; `None`, and the test skipped, without
/// `DUSCOPE_TEST_PLATFORM=1`. A missing tree fails the test rather than
/// passing it unmeasured.
fn t2(env: &duscope_testkit::TestEnv) -> Option<duscope_core::index::Index> {
    use duscope_core::index::Index;
    use duscope_core::query::index_files;

    if std::env::var_os("DUSCOPE_TEST_PLATFORM").is_none_or(|v| v != "1") {
        eprintln!("skipped: set DUSCOPE_TEST_PLATFORM=1");
        return None;
    }
    let tree = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../bench/.trees/T2");
    let tree = std::fs::canonicalize(&tree).unwrap_or_else(|e| {
        panic!(
            "T2 is not built at {}: {e}: run `just bench build --trees T2`",
            tree.display()
        )
    });
    let out = env
        .cmd()
        .arg("scan")
        .arg(&tree)
        .args(["--save", "--treemap", "never"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let files = index_files(&env.index_dir());
    Some(Index::open(&files[0].1).unwrap())
}

/// `samples`' 50th, 90th and 99th percentiles and maximum, in µs.
fn percentiles(samples: &mut [std::time::Duration]) -> [u128; 4] {
    samples.sort_unstable();
    let at = |q: usize| samples[(samples.len() - 1) * q / 100].as_micros();
    [at(50), at(90), at(99), at(100)]
}

/// The descend cost on T2: `⏎` on folders all over a saved scan of the
/// benchmark tree, each the end of the path, so the descend reads its
/// children (its files by id, its subfolders in one query on `dirs`).
/// It runs with `DUSCOPE_TEST_PLATFORM=1`, in a release build with the
/// binary built (`cargo build --release -p duscope`), on T2 built at
/// `bench/.trees/T2` (`just bench build --trees T2`): a missing tree fails
/// the test rather than passing it unmeasured. It reports the frame after
/// each descend, a zoom's first frame (the map painted for the new root)
/// and a zoom back out (the map kept) beside it.
#[test]
#[ignore = "needs bench/.trees/T2 and the release binary; DUSCOPE_TEST_PLATFORM=1"]
fn p4_tui_03_a_descend_costs_at_most_2_ms_on_t2() {
    use std::time::{Duration, Instant};

    use duscope_testkit::TestEnv;

    let env = TestEnv::new();
    let Some(index) = t2(&env) else {
        return;
    };
    let (mocha, classes) = (Pack::named("mocha").unwrap(), Classes::vendored());
    let start = Instant::now();
    let mut app = App::new(
        &index,
        opts(&mocha, &classes, ColorBy::Type),
        Depth::TrueColor,
        None,
        String::new(),
        None,
    );
    let opened = start.elapsed();
    let start = Instant::now();
    let mut terminal = first_frame(&app, 120, 40);
    let first = start.elapsed();
    let folders = folders(&index, 400);
    let (mut descends, mut frames) = (Vec::new(), Vec::new());
    for &folder in &folders {
        app.jump(folder);
        let start = Instant::now();
        let flow = app.on(&key(KeyCode::Enter));
        descends.push(start.elapsed());
        assert_eq!(flow, Flow::Redraw);
        assert_eq!(index.node(app.selected()).parent, folder);
        let start = Instant::now();
        terminal
            .draw(|f| app.draw(f.area(), f.buffer_mut()))
            .unwrap();
        frames.push(start.elapsed());
    }
    let (mut zooms, mut outs) = (Vec::new(), Vec::new());
    for &folder in folders.iter().take(40) {
        app.jump(folder);
        let start = Instant::now();
        press(&mut app, &mut terminal, &char('z'));
        zooms.push(start.elapsed());
        let start = Instant::now();
        press(&mut app, &mut terminal, &char('Z'));
        outs.push(start.elapsed());
    }
    let show = |what: &str, samples: &mut Vec<Duration>| {
        let [p50, p90, p99, max] = percentiles(samples);
        eprintln!(
            "{what}: {} samples, p50 {p50} µs, p90 {p90} µs, p99 {p99} µs, max {max} µs",
            samples.len()
        );
        [p50, p90, p99, max]
    };
    eprintln!(
        "T2: {} entries; the viewer opened in {} ms, its first frame in {} ms",
        index.len(),
        opened.as_millis(),
        first.as_millis()
    );
    let [p50, _, p99, max] = show("descend", &mut descends);
    show("frame after a descend", &mut frames);
    show("zoom in, its first frame", &mut zooms);
    show("zoom out, the map kept", &mut outs);
    assert!(
        p50 <= 2000 && p99 <= 2000,
        "descend p50 {p50} µs, p99 {p99} µs"
    );
    assert!(max <= 2000, "descend max {max} µs");
}

/// With `--treemap pixels` on T2, at 200×60 (a map of 152×55 cells), for
/// each protocol and a cell of 10×20 and of 16×32 pixels: the frame after
/// `z` (the folder's picture drawn), after `Z` (the whole map's picture
/// kept, the marks encoded) and after an arrow (the marks encoded). In a
/// build without the `pixels` feature (`cargo test --release -p
/// duscope-tui --features duscope-treemap/pixels` has it) the pictures are
/// drawn and marked but not encoded. Run as the descend test is. A zoom
/// back out costs under a fifth of the first frame, which draws the whole
/// map's picture: it is kept, not drawn again.
#[test]
#[ignore = "needs bench/.trees/T2 and the release binary; DUSCOPE_TEST_PLATFORM=1"]
fn p4_tui_03_in_pixels_a_zoom_out_keeps_the_picture_on_t2() {
    use std::time::{Duration, Instant};

    use duscope_testkit::TestEnv;
    use duscope_treemap::pixels::{Pixels, Protocol};

    let env = TestEnv::new();
    let Some(index) = t2(&env) else {
        return;
    };
    let (mocha, classes) = (Pack::named("mocha").unwrap(), Classes::vendored());
    let (w, h) = (200, 60);
    let ms = |samples: &mut Vec<Duration>| {
        let [p50, p90, _, max] = percentiles(samples);
        format!("p50 {p50} µs, p90 {p90} µs, max {max} µs")
    };
    for cell in [(10, 20), (16, 32)] {
        for protocol in [
            Some(Protocol::Kitty),
            Some(Protocol::Sixel),
            Some(Protocol::Iterm2),
            None,
        ] {
            let pixels = protocol.and_then(|p| Pixels::assume(p, cell));
            if protocol.is_some() && pixels.is_none() {
                continue;
            }
            let mut app = App::new(
                &index,
                opts(&mocha, &classes, ColorBy::Folder),
                Depth::TrueColor,
                None,
                String::new(),
                None,
            );
            match pixels {
                Some(pixels) => app.draw_in_pixels(pixels),
                None => app.draw_in_pixels_unencoded(cell),
            }
            let start = Instant::now();
            let mut terminal = first_frame(&app, w, h);
            let first = start.elapsed();
            let mut timed = |app: &mut App<'_>, event: &Event| {
                let start = Instant::now();
                press(app, &mut terminal, event);
                start.elapsed()
            };
            let (mut zooms, mut outs, mut arrows) = (Vec::new(), Vec::new(), Vec::new());
            for &folder in &folders(&index, 12) {
                app.jump(folder);
                zooms.push(timed(&mut app, &char('z')));
                outs.push(timed(&mut app, &char('Z')));
            }
            app.jump(index.child_dirs(0)[0].0);
            for code in [KeyCode::Right, KeyCode::Down, KeyCode::Left, KeyCode::Up].repeat(6) {
                arrows.push(timed(&mut app, &key(code)));
            }
            let name = protocol.map_or_else(|| "not encoded".to_owned(), |p| format!("{p:?}"));
            eprintln!(
                "{name}, cell {}×{}: first frame {} ms; z {}; Z {}; arrow {}",
                cell.0,
                cell.1,
                first.as_millis(),
                ms(&mut zooms),
                ms(&mut outs),
                ms(&mut arrows)
            );
            let [out, ..] = percentiles(&mut outs);
            assert!(
                out * 5 < first.as_micros(),
                "{name}: Z p50 {out} µs against a first frame of {first:?}"
            );
        }
    }
}
