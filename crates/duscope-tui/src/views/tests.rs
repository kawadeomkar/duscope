//! P4-TUI-05: the views `1`–`4`, the sort, the labels, the colour modes,
//! the packs, the mask, the panes' focus, `?`, `y` and `o`. The canvas's
//! example tree, as the earlier packages draw it, is what the snapshots
//! show; the checks read the screen, the tiles and the key map.

use std::cell::RefCell;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use duscope_core::arena::Arena;
use duscope_core::interchange::import::{ImportFormat, import};
use duscope_core::query::{ArenaView, TreeView};
use duscope_core::rollup::{RollUp, roll_up};
use duscope_testkit::golden;
use duscope_treemap::cells::{Color, Depth};
use duscope_treemap::packs::Pack;
use duscope_treemap::paint::{ColorBy, Glyphs, LsColors};
use ratatui_core::backend::TestBackend;
use ratatui_core::buffer::Buffer;
use ratatui_core::style::Color as Shown;
use ratatui_core::terminal::Terminal;

use crate::app::{clipboard_allowed, osc52};
use crate::cursor::tests::{char, first_frame, key, press};
use crate::keys::ALL;
use crate::panes::tests::{classes, mockup, opts, status};
use crate::widgets::Canvas;
use crate::zoom::Map;
use crate::{App, Effects, Flow, snapshot};

fn id_of(view: &dyn TreeView, path: &str) -> u32 {
    (0..view.len())
        .find(|&id| view.path(id) == path.as_bytes())
        .unwrap_or_else(|| panic!("no {path}"))
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

fn mockup_view(tree: &(Arena, RollUp)) -> ArenaView<'_> {
    ArenaView::new(&tree.0, &tree.1, PathBuf::from("/Users/alex/Library"))
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

/// The rows of the map's pane at 120 columns, the right column left out.
fn map_text(buf: &Buffer) -> String {
    let mut out = String::new();
    for y in 3..buf.area.height.saturating_sub(1) {
        for x in 0..84.min(buf.area.width) {
            out.push_str(buf[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

fn row(buf: &Buffer, y: u16) -> String {
    let mut out = String::new();
    for x in 0..buf.area.width {
        out.push_str(buf[(x, y)].symbol());
    }
    out
}

/// Runs `script` on `app` drawn at `w`×`h`, then returns the terminal.
fn play(app: &mut App<'_>, w: u16, h: u16, script: &[Event]) -> Terminal<TestBackend> {
    let mut terminal = first_frame(app, w, h);
    for event in script {
        press(app, &mut terminal, event);
    }
    terminal
}

const SIZES: [(u16, u16); 3] = [(160, 45), (120, 40), (80, 24)];

#[test]
fn p4_tui_05_the_three_tables_are_the_design_systems_at_each_size() {
    let tree = mockup();
    let view = mockup_view(&tree);
    let (mocha, classes) = (Pack::named("mocha").unwrap(), classes());
    for (tab, name) in [('2', "list"), ('3', "largest"), ('4', "types")] {
        for (w, h) in SIZES {
            let mut app = viewer(&view, &mocha, &classes);
            let mut terminal = first_frame(&app, w, h);
            assert_eq!(press(&mut app, &mut terminal, &char(tab)), Flow::Redraw);
            let buf = terminal.backend().buffer();
            golden(&format!("view_{name}_mockup_{w}x{h}.snap"), &snapshot(buf));
            let text = text_of(buf);
            // The tab in the header is the active one, bold in the text
            // colour on surface1; the table stands where the map did.
            let header = row(buf, 0);
            let label = format!("{tab} {name}");
            let x = header.find(&label).unwrap();
            let x = u16::try_from(header[..x].chars().count()).unwrap();
            assert_eq!(buf[(x, 0)].bg, Shown::Rgb(0x45, 0x47, 0x5a), "{header}");
            assert!(row(buf, 2).contains(&format!("╭─ {name} · ")), "{text}");
            // The design system's table: the header in the title colour, a
            // double rule, a row marked ▸ in the text colour, bold.
            let (rule_y, _) = (0..h)
                .map(|y| (y, row(buf, y)))
                .find(|(_, r)| r.contains("═╪═"))
                .unwrap_or_else(|| panic!("no rule:\n{text}"));
            let head = row(buf, rule_y - 1);
            let x = u16::try_from(head.chars().position(char::is_alphabetic).unwrap()).unwrap();
            assert_eq!(buf[(x, rule_y - 1)].fg, Shown::Rgb(0xb4, 0xbe, 0xfe));
            assert!(text.contains('▸'), "{text}");
            // The right column stays, as the map's does.
            if w >= 110 {
                assert!(text.contains("selected"), "{text}");
                assert!(text.contains("in com.apple.Safari"), "{text}");
                assert!(text.contains("by type"), "{text}");
            } else {
                assert!(text.contains("selected"), "{text}");
            }
        }
    }
}

#[test]
fn p4_tui_05_the_list_follows_the_cursors_folder_and_the_arrows_walk_it() {
    let tree = mockup();
    let view = mockup_view(&tree);
    let (mocha, classes) = (Pack::named("mocha").unwrap(), classes());
    let mut app = viewer(&view, &mocha, &classes);
    let mut terminal = play(&mut app, 120, 40, &[char('2')]);
    let buf = terminal.backend().buffer();
    let text = text_of(buf);
    assert!(
        row(buf, 2).contains("list · ~/Library/Caches/com.apple.Safari"),
        "{text}"
    );
    // The largest first, the cursor on the first.
    let wal = id_of(&view, "Caches/com.apple.Safari/Cache.db-wal");
    assert_eq!(app.selected(), wal);
    // ↓ walks down the list, sorted as the siblings pane is.
    assert_eq!(
        press(&mut app, &mut terminal, &key(KeyCode::Down)),
        Flow::Redraw
    );
    assert_eq!(
        app.selected(),
        id_of(&view, "Caches/com.apple.Safari/Cache.db")
    );
    assert_eq!(
        press(&mut app, &mut terminal, &key(KeyCode::Up)),
        Flow::Redraw
    );
    assert_eq!(app.selected(), wal);
    // ← and → are the map's, nothing on a list.
    assert_eq!(
        press(&mut app, &mut terminal, &key(KeyCode::Left)),
        Flow::Wait
    );
    // ⌫ goes up the path, the list then shows the parent folder.
    assert_eq!(
        press(&mut app, &mut terminal, &key(KeyCode::Backspace)),
        Flow::Redraw
    );
    assert!(row(terminal.backend().buffer(), 2).contains("list · ~/Library/Caches "));
    // ⏎ goes back down.
    assert_eq!(
        press(&mut app, &mut terminal, &key(KeyCode::Enter)),
        Flow::Redraw
    );
    // G walks to the last; g back.
    assert_eq!(press(&mut app, &mut terminal, &char('G')), Flow::Redraw);
    assert_ne!(app.selected(), wal);
    assert_eq!(press(&mut app, &mut terminal, &char('g')), Flow::Redraw);
    assert_eq!(app.selected(), wal);
}

#[test]
fn p4_tui_05_the_largest_view_ranks_the_files_and_enter_shows_the_map() {
    let tree = mockup();
    let view = mockup_view(&tree);
    let (mocha, classes) = (Pack::named("mocha").unwrap(), classes());
    let mut app = viewer(&view, &mocha, &classes);
    let mut terminal = play(&mut app, 120, 40, &[char('3')]);
    // The biggest file first: the selection the viewer opens on.
    let wal = id_of(&view, "Caches/com.apple.Safari/Cache.db-wal");
    assert_eq!(app.selected(), wal);
    let text = text_of(terminal.backend().buffer());
    assert!(text.contains("Cache.db-wal"), "{text}");
    // ↓ moves the cursor to the next largest file wherever it is; the
    // selected pane follows.
    assert_eq!(
        press(&mut app, &mut terminal, &key(KeyCode::Down)),
        Flow::Redraw
    );
    let next = app.selected();
    assert_ne!(next, wal);
    let size_of = |id: u32| view.node(id).allocated;
    assert!(size_of(next) <= size_of(wal));
    // ⏎ shows it on the map.
    assert_eq!(
        press(&mut app, &mut terminal, &key(KeyCode::Enter)),
        Flow::Redraw
    );
    assert_eq!(app.selected(), next);
    assert!(row(terminal.backend().buffer(), 2).contains("treemap · "));
}

#[test]
fn p4_tui_05_the_types_view_lists_the_classes_and_enter_filters_the_map() {
    let tree = mockup();
    let view = mockup_view(&tree);
    let (mocha, classes) = (Pack::named("mocha").unwrap(), classes());
    let mut app = viewer(&view, &mocha, &classes);
    let mut terminal = play(&mut app, 120, 40, &[char('4')]);
    let text = text_of(terminal.backend().buffer());
    for class in ["data", "archives", "binaries", "other"] {
        assert!(text.contains(class), "{class}\n{text}");
    }
    // ⏎ on the first row keeps that class on the map, and the map shows.
    assert_eq!(
        press(&mut app, &mut terminal, &key(KeyCode::Enter)),
        Flow::Redraw
    );
    let buf = terminal.backend().buffer();
    let title = row(buf, 2);
    assert!(title.contains("filter: type: data"), "{title}");
    // Files of other classes are gone from the cells; the pane still
    // frames the map.
    assert!(title.contains("treemap"), "{title}");
    assert_eq!(
        press(&mut app, &mut terminal, &key(KeyCode::Esc)),
        Flow::Redraw
    );
    assert!(!row(terminal.backend().buffer(), 2).contains("filter"));
}

#[test]
fn p4_tui_05_snapshots_of_the_colour_modes_and_c_cycles_them() {
    let tree = mockup();
    let view = mockup_view(&tree);
    let (mocha, classes) = (Pack::named("mocha").unwrap(), classes());
    let mut app = viewer(&view, &mocha, &classes);
    let mut terminal = first_frame(&app, 120, 40);
    let ls = LsColors {
        executable: None,
        suffixes: vec![(".dmg".to_owned(), Color::Rgb(255, 0, 0))],
    };
    app.with_ls_colors(ls);
    for (i, (mode, title)) in [
        ("folder", "top folder"),
        ("level", "depth"),
        ("age", "age"),
        ("lscolors", "LS_COLORS"),
        ("type", "file type"),
    ]
    .iter()
    .enumerate()
    {
        assert_eq!(
            press(&mut app, &mut terminal, &char('c')),
            Flow::Redraw,
            "{mode}"
        );
        let buf = terminal.backend().buffer();
        assert!(
            row(buf, 2).contains(&format!("color: {title}")),
            "{i} {mode}: {}",
            row(buf, 2)
        );
        if *mode != "type" {
            golden(
                &format!("view_color_{mode}_mockup_120x40.snap"),
                &snapshot(buf),
            );
        }
    }
}

#[test]
fn p4_tui_05_c_and_p_forget_the_maps_and_pictures_painted() {
    let tree = mockup();
    let view = mockup_view(&tree);
    let (mocha, classes) = (Pack::named("mocha").unwrap(), classes());
    let packs: Vec<Pack> = ["mocha", "dracula"]
        .iter()
        .map(|n| Pack::named(n).unwrap())
        .collect();
    let mut app = viewer(&view, &mocha, &classes);
    app.with_packs(&packs);
    app.draw_in_pixels_unencoded((10, 20));
    let mut terminal = first_frame(&app, 120, 40);
    let z = KeyCode::Char('z');
    press(&mut app, &mut terminal, &key(z));
    press(&mut app, &mut terminal, &char('Z'));
    assert!(app.cursor().kept() >= 2, "{}", app.cursor().kept());
    assert!(app.pictures_kept() >= 2, "{}", app.pictures_kept());
    press(&mut app, &mut terminal, &char('c'));
    assert_eq!(app.cursor().kept(), 1, "c repainted once for the frame");
    assert_eq!(app.pictures_kept(), 1);
    press(&mut app, &mut terminal, &key(z));
    press(&mut app, &mut terminal, &char('p'));
    assert_eq!(app.cursor().kept(), 1);
    assert_eq!(app.pictures_kept(), 1);
    // `p` went to Dracula: the status colour of the map's frame says so.
    let buf = terminal.backend().buffer();
    assert_eq!(buf[(3, 2)].fg, Shown::Rgb(0xbd, 0x93, 0xf9));
    let text = text_of(buf);
    assert!(text.contains("theme: dracula"), "{text}");
}

#[test]
fn p4_tui_05_snapshots_of_the_packs_p_cycles_through() {
    let tree = mockup();
    let view = mockup_view(&tree);
    let (mocha, classes) = (Pack::named("mocha").unwrap(), classes());
    let packs: Vec<Pack> = ["mocha", "dracula", "light"]
        .iter()
        .map(|n| Pack::named(n).unwrap())
        .collect();
    let mut app = viewer(&view, &mocha, &classes);
    app.with_packs(&packs);
    let mut terminal = first_frame(&app, 120, 40);
    for name in ["dracula", "light"] {
        assert_eq!(press(&mut app, &mut terminal, &char('p')), Flow::Redraw);
        let buf = terminal.backend().buffer();
        let text = text_of(buf);
        assert!(text.contains(&format!("theme: {name}")), "{text}");
        golden(
            &format!("view_pack_{name}_mockup_120x40.snap"),
            &snapshot(buf),
        );
    }
    // Round to the first pack again.
    assert_eq!(press(&mut app, &mut terminal, &char('p')), Flow::Redraw);
    assert!(text_of(terminal.backend().buffer()).contains("theme: mocha"));
}

#[test]
fn p4_tui_05_p_does_nothing_with_one_pack_or_no_colours() {
    let tree = mockup();
    let view = mockup_view(&tree);
    let (mocha, classes) = (Pack::named("mocha").unwrap(), classes());
    let mut app = viewer(&view, &mocha, &classes);
    let mut terminal = first_frame(&app, 120, 40);
    assert_eq!(press(&mut app, &mut terminal, &char('p')), Flow::Wait);
    let no_color = Pack::named("no-color").unwrap();
    let packs = vec![no_color.clone(), mocha.clone()];
    let mut app = App::new(
        &view,
        opts(&no_color, &classes, ColorBy::Type),
        Depth::None,
        Some(Path::new("/Users/alex")),
        status(&view),
        None,
    );
    app.with_packs(&packs);
    let mut terminal = first_frame(&app, 120, 40);
    assert_eq!(press(&mut app, &mut terminal, &char('p')), Flow::Wait);
}

#[test]
fn p4_tui_05_snapshots_of_the_sorts_and_the_siblings_pane_shows_them() {
    let tree = mockup();
    let view = mockup_view(&tree);
    let (mocha, classes) = (Pack::named("mocha").unwrap(), classes());
    let mut app = viewer(&view, &mocha, &classes);
    let mut terminal = first_frame(&app, 120, 40);
    for sort in ["name", "modified", "count", "size"] {
        assert_eq!(press(&mut app, &mut terminal, &char('s')), Flow::Redraw);
        let buf = terminal.backend().buffer();
        let text = text_of(buf);
        if sort == "size" {
            assert!(
                !text.contains("by name") && !text.contains("by count"),
                "{text}"
            );
        } else {
            assert!(text.contains(&format!("by {sort} · in ")), "{sort}\n{text}");
            golden(
                &format!("view_sort_{sort}_mockup_120x40.snap"),
                &snapshot(buf),
            );
        }
    }
    // The list reads the same sort.
    press(&mut app, &mut terminal, &char('s'));
    let terminal = play(&mut app, 120, 40, &[char('2')]);
    let text = text_of(terminal.backend().buffer());
    assert!(
        row(terminal.backend().buffer(), 2).contains("· by name"),
        "{text}"
    );
}

#[test]
fn p4_tui_05_t_labels_tiles_of_at_least_8_by_2_and_only_those() {
    let tree = mockup();
    let view = mockup_view(&tree);
    let (mocha, classes) = (Pack::named("mocha").unwrap(), classes());
    let mut app = viewer(&view, &mocha, &classes);
    let mut terminal = first_frame(&app, 120, 40);
    assert!(!map_text(terminal.backend().buffer()).contains("Cache.db"));
    let before = app.cursor().kept();
    assert_eq!(press(&mut app, &mut terminal, &char('t')), Flow::Redraw);
    assert!(app.cursor().kept() <= before.max(1));
    let buf = terminal.backend().buffer();
    golden("view_labels_mockup_120x40.snap", &snapshot(buf));
    // The largest file's tile carries its name and size; a name shorter
    // than the tile, within the map's pane.
    let map = app.map();
    let raw = (0..view.len())
        .find(|&id| view.name(id).as_ref() == b"Docker.raw")
        .unwrap();
    let tile = map.tiles().iter().find(|t| t.id == raw).unwrap();
    assert!(tile.rect.w >= 8 && tile.rect.h / 2 >= 2, "{tile:?}");
    let top = 3 + tile.rect.y.div_ceil(2);
    assert!(row(buf, top).contains("Docker.raw"), "{}", row(buf, top));
    assert!(row(buf, top + 1).contains("8.7G"), "{}", row(buf, top + 1));
    // A leaf narrower than 8 columns carries no label.
    let small = map
        .tiles()
        .iter()
        .find(|t| t.kind == duscope_treemap::paint::DrawnKind::Leaf && t.rect.w < 8)
        .unwrap();
    let name = String::from_utf8_lossy(&view.name(small.id)).into_owned();
    assert!(
        !map_text(buf).contains(&name) || view.node(small.id).is_dir(),
        "{name}"
    );
    // Off again.
    assert_eq!(press(&mut app, &mut terminal, &char('t')), Flow::Redraw);
    assert!(!map_text(terminal.backend().buffer()).contains("Cache.db-wal"));
}

#[test]
fn p4_tui_05_the_mask_hides_tiles_without_a_new_layout_and_esc_clears_it() {
    let tree = mockup();
    let view = mockup_view(&tree);
    let (mocha, classes) = (Pack::named("mocha").unwrap(), classes());
    let mut app = viewer(&view, &mocha, &classes);
    let mut terminal = first_frame(&app, 120, 40);
    let map_before: Vec<_> = app.map().tiles().to_vec();
    // `/` opens the prompt, typed on the key line.
    assert_eq!(press(&mut app, &mut terminal, &char('/')), Flow::Redraw);
    for c in "*.db".chars() {
        assert_eq!(press(&mut app, &mut terminal, &char(c)), Flow::Redraw);
    }
    let buf = terminal.backend().buffer();
    let key_row = row(buf, 39);
    assert!(key_row.contains(" >  *.db▌"), "{key_row}");
    assert!(!key_row.contains("quit"), "{key_row}");
    // Enter applies it: the same tiles, some painted out.
    assert_eq!(
        press(&mut app, &mut terminal, &key(KeyCode::Enter)),
        Flow::Redraw
    );
    let buf = terminal.backend().buffer();
    let map = app.map();
    assert_eq!(map.tiles(), map_before.as_slice());
    assert!(row(buf, 2).contains("filter: *.db"), "{}", row(buf, 2));
    golden("view_mask_glob_mockup_120x40.snap", &snapshot(buf));
    // The tile of a `.dmg` is painted out; one of the `.db` shows.
    let hidden = id_of(&view, "Caches/Homebrew/Cask/docker--4.35.0,172550.dmg");
    let shown_id = id_of(&view, "Caches/com.apple.Safari/Cache.db");
    let tile_at = |map: &Map, id: u32| -> (u16, u16) {
        let tile = map.tiles().iter().find(|t| t.id == id).unwrap();
        (
            1 + tile.rect.x + tile.rect.w / 2,
            3 + u16::midpoint(tile.rect.y, tile.rect.y + tile.rect.h) / 2,
        )
    };
    let base = Shown::Rgb(0x1e, 0x1e, 0x2e);
    let (x, y) = tile_at(&map, shown_id);
    assert_ne!(buf[(x, y)].bg, base, "the .db tile shows");
    let (x, y) = tile_at(&map, hidden);
    assert_eq!(
        (buf[(x, y)].bg, buf[(x, y)].symbol()),
        (base, " "),
        "the .dmg tile is painted out"
    );
    // The arrows leave the hidden tiles alone: moving from the mask's
    // leftover never lands on a dmg.
    for _ in 0..40 {
        press(&mut app, &mut terminal, &key(KeyCode::Right));
        press(&mut app, &mut terminal, &key(KeyCode::Down));
        assert_ne!(app.selected(), hidden);
    }
    // Esc clears the mask first; the map is as before.
    let cleared_row = |t: &Terminal<TestBackend>| row(t.backend().buffer(), 2);
    assert_eq!(
        press(&mut app, &mut terminal, &key(KeyCode::Esc)),
        Flow::Redraw
    );
    assert!(!cleared_row(&terminal).contains("filter"));
    let map = app.map();
    let (x, y) = tile_at(&map, hidden);
    assert_ne!(terminal.backend().buffer()[(x, y)].bg, base);
}

#[test]
fn p4_tui_05_the_prompt_takes_text_cancels_and_refuses_what_is_no_glob() {
    let tree = mockup();
    let view = mockup_view(&tree);
    let (mocha, classes) = (Pack::named("mocha").unwrap(), classes());
    let mut app = viewer(&view, &mocha, &classes);
    let mut terminal = first_frame(&app, 80, 24);
    press(&mut app, &mut terminal, &char('/'));
    golden(
        "view_prompt_mockup_80x24.snap",
        &snapshot(terminal.backend().buffer()),
    );
    // Keys typed into the prompt are its text, not the viewer's.
    press(&mut app, &mut terminal, &char('q'));
    assert_eq!(app.on(&char('s')), Flow::Redraw);
    let row_of = |t: &Terminal<TestBackend>| row(t.backend().buffer(), 23);
    press(&mut app, &mut terminal, &key(KeyCode::Backspace));
    let text = row_of(&terminal);
    assert!(text.contains(">  q▌"), "{text}");
    // Esc cancels, the map as it was; Ctrl-C quits from the prompt.
    assert_eq!(
        press(&mut app, &mut terminal, &key(KeyCode::Esc)),
        Flow::Redraw
    );
    assert!(!row_of(&terminal).contains('▌'));
    assert_eq!(app.filter, None);
    press(&mut app, &mut terminal, &char('/'));
    assert_eq!(
        app.on(&Event::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL
        ))),
        Flow::Quit
    );
    // A bad glob stays in the prompt, with why on the key line.
    let mut app = viewer(&view, &mocha, &classes);
    let mut terminal = first_frame(&app, 100, 30);
    press(&mut app, &mut terminal, &char('/'));
    for c in "[a".chars() {
        press(&mut app, &mut terminal, &char(c));
    }
    assert_eq!(
        press(&mut app, &mut terminal, &key(KeyCode::Enter)),
        Flow::Redraw
    );
    let text = row(terminal.backend().buffer(), 29);
    assert!(
        text.contains("[a") && text.contains("not a filter"),
        "{text}"
    );
    assert!(app.filter.is_none());
    // An empty prompt clears the mask.
    let mut app = viewer(&view, &mocha, &classes);
    let mut terminal = first_frame(&app, 100, 30);
    for c in "/*.db".chars() {
        press(&mut app, &mut terminal, &char(c));
    }
    press(&mut app, &mut terminal, &key(KeyCode::Enter));
    assert!(app.filter.is_some());
    // `/` reopens it on the same text; `⌫` deletes back to nothing.
    press(&mut app, &mut terminal, &char('/'));
    assert!(row(terminal.backend().buffer(), 29).contains("*.db"));
    for _ in 0..4 {
        press(&mut app, &mut terminal, &key(KeyCode::Backspace));
    }
    press(&mut app, &mut terminal, &key(KeyCode::Enter));
    assert!(app.filter.is_none());
}

#[test]
fn p4_tui_05_tab_focuses_the_panes_and_the_arrows_act_in_them() {
    let tree = mockup();
    let view = mockup_view(&tree);
    let (mocha, classes) = (Pack::named("mocha").unwrap(), classes());
    let mut app = viewer(&view, &mocha, &classes);
    let mut terminal = first_frame(&app, 120, 40);
    let accent = Shown::Rgb(0xf9, 0xe2, 0xaf);
    let tab = key(KeyCode::Tab);
    let back = Event::Key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT));
    // ⇥: the siblings pane, its frame in the accent colour; the arrows
    // walk it.
    assert_eq!(press(&mut app, &mut terminal, &tab), Flow::Redraw);
    let buf = terminal.backend().buffer();
    let x = 120 - 35;
    assert_eq!(buf[(x, 14)].fg, accent, "{}", row(buf, 14));
    golden("view_focus_siblings_mockup_120x40.snap", &snapshot(buf));
    let wal = app.selected();
    assert_eq!(
        press(&mut app, &mut terminal, &key(KeyCode::Down)),
        Flow::Redraw
    );
    assert_ne!(app.selected(), wal);
    assert_eq!(
        press(&mut app, &mut terminal, &key(KeyCode::Up)),
        Flow::Redraw
    );
    assert_eq!(app.selected(), wal);
    // The other arrows do nothing there.
    assert_eq!(
        press(&mut app, &mut terminal, &key(KeyCode::Right)),
        Flow::Wait
    );
    // ⇥ again: the legend. ↓ moves its marker; ⏎ filters the map to that
    // row; Esc clears the filter and keeps the focus.
    assert_eq!(press(&mut app, &mut terminal, &tab), Flow::Redraw);
    assert_eq!(
        press(&mut app, &mut terminal, &key(KeyCode::Down)),
        Flow::Redraw
    );
    assert_eq!(app.selected(), wal, "the legend's cursor is its own");
    let buf = terminal.backend().buffer();
    let marked: Vec<String> = (29..39)
        .map(|y| row(buf, y))
        .filter(|r| r.contains('▸'))
        .collect();
    assert_eq!(marked.len(), 1, "{marked:?}");
    assert_eq!(
        press(&mut app, &mut terminal, &key(KeyCode::Enter)),
        Flow::Redraw
    );
    let buf = terminal.backend().buffer();
    let title = row(buf, 2);
    assert!(title.contains("filter: type: "), "{title}");
    golden(
        "view_focus_legend_filter_mockup_120x40.snap",
        &snapshot(buf),
    );
    assert_eq!(
        press(&mut app, &mut terminal, &key(KeyCode::Esc)),
        Flow::Redraw
    );
    assert!(!row(terminal.backend().buffer(), 2).contains("filter"));
    assert_eq!(
        app.selected(),
        wal,
        "Esc cleared the filter, it did not ascend"
    );
    // ⇥ a third time: the map again; the arrows move on it. Shift-⇥ goes
    // back the other way round.
    assert_eq!(press(&mut app, &mut terminal, &tab), Flow::Redraw);
    assert_eq!(press(&mut app, &mut terminal, &back), Flow::Redraw);
    assert_eq!(press(&mut app, &mut terminal, &back), Flow::Redraw);
    let buf = terminal.backend().buffer();
    assert_eq!(buf[(x, 14)].fg, accent, "Shift-⇥ twice: the siblings");
    // The folded layout has one pane to look at: ⇥ does nothing there.
    let mut folded_app = viewer(&view, &mocha, &classes);
    let mut folded = first_frame(&folded_app, 100, 30);
    assert_eq!(press(&mut folded_app, &mut folded, &tab), Flow::Wait);
}

/// The legend's rows of `buf` from row `top` on, in the right column of a
/// 120-column screen, that carry the `▸` marker.
fn legend_marked(buf: &Buffer, top: u16) -> Vec<String> {
    (top..buf.area.height - 1)
        .map(|y| row(buf, y).chars().skip(85).collect::<String>())
        .filter(|r| r.contains('▸'))
        .collect()
}

#[test]
fn p4_tui_05_with_the_facts_over_the_legend_the_keys_act_on_the_map() {
    let tree = mockup();
    let view = mockup_view(&tree);
    let (mocha, classes) = (Pack::named("mocha").unwrap(), classes());
    let mut app = viewer(&view, &mocha, &classes);
    let mut terminal = first_frame(&app, 120, 40);
    let keys = |t: &Terminal<TestBackend>| row(t.backend().buffer(), 39);
    let tab = key(KeyCode::Tab);
    press(&mut app, &mut terminal, &tab);
    press(&mut app, &mut terminal, &tab);
    assert!(keys(&terminal).contains("⏎ filter"), "{}", keys(&terminal));
    // `i` covers the siblings and the legend: the key line is the map's,
    // and neither `⏎ filter` nor `⇥ pane` is offered where they do nothing.
    assert_eq!(press(&mut app, &mut terminal, &char('i')), Flow::Redraw);
    let line = keys(&terminal);
    assert!(line.contains("⌫ up   z zoom"), "{line}");
    assert!(!line.contains("filter") && !line.contains("pane"), "{line}");
    assert_eq!(press(&mut app, &mut terminal, &tab), Flow::Wait);
    // The arrows and ⏎ act on the map, not on the legend hidden under the
    // facts: no filter is set.
    press(&mut app, &mut terminal, &key(KeyCode::Down));
    press(&mut app, &mut terminal, &key(KeyCode::Enter));
    assert_eq!(app.filter, None);
    assert!(!row(terminal.backend().buffer(), 2).contains("filter"));
    // Closing the facts brings the legend back, and the focus with it, on
    // the row it was on.
    assert_eq!(press(&mut app, &mut terminal, &char('i')), Flow::Redraw);
    assert!(keys(&terminal).contains("⏎ filter"), "{}", keys(&terminal));
    let buf = terminal.backend().buffer();
    let marked = legend_marked(buf, 29);
    assert_eq!(marked.len(), 1, "{marked:?}");
    assert!(
        row(buf, 30)
            .chars()
            .skip(85)
            .collect::<String>()
            .contains('▸'),
        "the first row: {marked:?}"
    );
}

#[test]
fn p4_tui_05_enter_in_a_legend_a_resize_shortened_filters_by_a_row_it_lists() {
    let tree = mockup();
    let view = mockup_view(&tree);
    let (mocha, classes) = (Pack::named("mocha").unwrap(), classes());
    let mut app = viewer(&view, &mocha, &classes);
    let mut terminal = first_frame(&app, 120, 40);
    let tab = key(KeyCode::Tab);
    press(&mut app, &mut terminal, &tab);
    press(&mut app, &mut terminal, &tab);
    for _ in 0..3 {
        assert_eq!(
            press(&mut app, &mut terminal, &key(KeyCode::Down)),
            Flow::Redraw
        );
    }
    // 24 rows leave the legend two rows and `… n more`: the marker is on
    // the last row it lists, and ⏎ filters by that row.
    let mut terminal = first_frame(&app, 120, 24);
    let buf = terminal.backend().buffer();
    let text = text_of(buf);
    let marked = legend_marked(buf, 18);
    assert_eq!(marked.len(), 1, "{text}");
    assert!(text.contains("more"), "{text}");
    let label = marked[0]
        .split('■')
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap()
        .trim_end_matches('…')
        .to_owned();
    assert_eq!(
        press(&mut app, &mut terminal, &key(KeyCode::Enter)),
        Flow::Redraw
    );
    let title = row(terminal.backend().buffer(), 2);
    assert!(
        title.contains(&format!("filter: type: {label}")),
        "{label}: {title}"
    );
}

#[test]
fn p4_tui_05_question_mark_lists_exactly_the_bindings_in_the_key_map() {
    let tree = mockup();
    let view = mockup_view(&tree);
    let (mocha, classes) = (Pack::named("mocha").unwrap(), classes());
    for (w, h) in [(160, 45), (80, 24)] {
        let mut app = viewer(&view, &mocha, &classes);
        let mut terminal = first_frame(&app, w, h);
        assert_eq!(press(&mut app, &mut terminal, &char('?')), Flow::Redraw);
        let buf = terminal.backend().buffer();
        let text = text_of(buf);
        golden(&format!("view_help_mockup_{w}x{h}.snap"), &snapshot(buf));
        assert!(row(buf, 2).contains("keys"), "{text}");
        // Every binding of the map, its keys and its words, each once, and
        // nothing else between the frame's borders.
        let mut expected: Vec<String> = ALL
            .iter()
            .map(|b| format!("{:<11} {}", b.keys(false), b.what))
            .collect();
        let mut shown: Vec<String> = Vec::new();
        let body: Vec<String> = (3..h - 2).map(|y| row(buf, y)).collect();
        let half = w / 2;
        for line in &body {
            // The two columns of the frame's inside.
            let left: String = line.chars().skip(1).take(usize::from(half) - 1).collect();
            let right: String = line
                .chars()
                .skip(usize::from(half))
                .take(usize::from(half) - 1)
                .collect();
            for cell in [left, right] {
                let cell = cell.trim_end().trim_end_matches('│').trim_end().to_owned();
                if !cell.is_empty() {
                    shown.push(cell);
                }
            }
        }
        expected.sort();
        shown.sort();
        assert_eq!(shown, expected, "{w}x{h}\n{text}");
        // `?` again, or Esc, closes it; the other keys are not the map's
        // while it is open.
        assert_eq!(press(&mut app, &mut terminal, &char('s')), Flow::Wait);
        assert_eq!(press(&mut app, &mut terminal, &char('?')), Flow::Redraw);
        assert!(!text_of(terminal.backend().buffer()).contains("this help"));
        press(&mut app, &mut terminal, &char('?'));
        assert_eq!(
            press(&mut app, &mut terminal, &key(KeyCode::Esc)),
            Flow::Redraw
        );
        assert!(!text_of(terminal.backend().buffer()).contains("this help"));
        assert_eq!(press(&mut app, &mut terminal, &char('?')), Flow::Redraw);
        assert_eq!(app.on(&char('q')), Flow::Quit);
    }
}

#[test]
fn p4_tui_05_without_utf_8_the_help_and_the_tables_keep_to_ascii() {
    let tree = mockup();
    let view = mockup_view(&tree);
    let (mocha, classes) = (Pack::named("mocha").unwrap(), classes());
    let opts_ascii = duscope_treemap::paint::PaintOpts {
        glyphs: Glyphs::Ascii,
        ..opts(&mocha, &classes, ColorBy::Type)
    };
    let mut app = App::new(
        &view,
        opts_ascii,
        Depth::TrueColor,
        Some(Path::new("/Users/alex")),
        status(&view),
        None,
    );
    let mut terminal = first_frame(&app, 120, 40);
    for key_press in ['2', '3', '4', '1', '?'] {
        press(&mut app, &mut terminal, &char(key_press));
        let snap = snapshot(terminal.backend().buffer());
        let screen = snap.split("--- styles").next().unwrap();
        assert!(screen.is_ascii(), "{key_press}:\n{screen}");
    }
}

#[test]
fn p4_tui_05_y_copies_with_osc_52_and_falls_back_to_the_key_line() {
    let tree = mockup();
    let view = mockup_view(&tree);
    let (mocha, classes) = (Pack::named("mocha").unwrap(), classes());
    // A terminal that takes it: the sequence, nothing else, and `copied`
    // with the path on the key line until the next key.
    let bytes = Rc::new(RefCell::new(Vec::<u8>::new()));
    let sink = Rc::clone(&bytes);
    let mut app = viewer(&view, &mocha, &classes);
    app.with_effects(Effects {
        write: Box::new(move |b| {
            sink.borrow_mut().extend_from_slice(b);
            Ok(())
        }),
        ..Effects::default()
    });
    let mut terminal = first_frame(&app, 120, 40);
    assert_eq!(press(&mut app, &mut terminal, &char('y')), Flow::Redraw);
    let path = duscope_core::query::abs_path(&view, app.selected());
    assert_eq!(*bytes.borrow(), osc52(&path));
    assert_eq!(
        osc52(b"/a"),
        b"\x1b]52;c;L2E=\x1b\\",
        "OSC 52, the clipboard, base64"
    );
    let key_row = row(terminal.backend().buffer(), 39);
    assert!(
        key_row.starts_with(" copied /Users/alex/Library/Caches/com.apple.Safari/Cache.db-wal"),
        "{key_row}"
    );
    // The next key, bound or not, brings the keys back.
    assert_eq!(press(&mut app, &mut terminal, &char('x')), Flow::Redraw);
    assert!(row(terminal.backend().buffer(), 39).contains("q quit"));
    assert_eq!(press(&mut app, &mut terminal, &char('x')), Flow::Wait);

    // Where the terminal does not take it: the path, no bytes.
    for (name, value) in [
        ("TERM_PROGRAM", "Apple_Terminal"),
        ("TMUX", "/tmp/tmux/1,2,3"),
        ("TERM", "linux"),
        ("TERM", "screen-256color"),
    ] {
        let bytes = Rc::new(RefCell::new(Vec::<u8>::new()));
        let sink = Rc::clone(&bytes);
        let mut app = viewer(&view, &mocha, &classes);
        app.with_effects(Effects {
            write: Box::new(move |b| {
                sink.borrow_mut().extend_from_slice(b);
                Ok(())
            }),
            env: Box::new(move |var| (var == name).then(|| value.into())),
            ..Effects::default()
        });
        let mut terminal = first_frame(&app, 80, 24);
        press(&mut app, &mut terminal, &char('y'));
        assert!(bytes.borrow().is_empty(), "{name}={value}");
        let key_row = row(terminal.backend().buffer(), 23);
        assert!(key_row.starts_with(" path "), "{name}: {key_row}");
        // A path that does not fit keeps its end.
        assert!(key_row.trim_end().ends_with("Cache.db-wal"), "{key_row}");
    }
    assert!(clipboard_allowed(
        &(Box::new(|_: &str| None) as Box<dyn Fn(&str) -> _>)
    ));
}

#[test]
fn p4_tui_05_o_opens_the_selection_through_the_opener_it_was_given() {
    let tree = mockup();
    let view = mockup_view(&tree);
    let (mocha, classes) = (Pack::named("mocha").unwrap(), classes());
    let opened = Rc::new(RefCell::new(Vec::<PathBuf>::new()));
    let log = Rc::clone(&opened);
    let mut app = viewer(&view, &mocha, &classes);
    app.with_effects(Effects {
        open: Box::new(move |path| {
            log.borrow_mut().push(path.to_owned());
            Ok(())
        }),
        ..Effects::default()
    });
    let mut terminal = first_frame(&app, 120, 40);
    assert_eq!(press(&mut app, &mut terminal, &char('o')), Flow::Wait);
    assert_eq!(
        *opened.borrow(),
        [PathBuf::from(
            "/Users/alex/Library/Caches/com.apple.Safari/Cache.db-wal"
        )]
    );
    // A failed start says why on the key line, and the key line recovers.
    let mut app = viewer(&view, &mocha, &classes);
    app.with_effects(Effects {
        open: Box::new(|_| Err(std::io::Error::other("no opener"))),
        ..Effects::default()
    });
    let mut terminal = first_frame(&app, 120, 40);
    assert_eq!(press(&mut app, &mut terminal, &char('o')), Flow::Redraw);
    assert!(row(terminal.backend().buffer(), 39).contains("could not open it: no opener"));
    assert_eq!(press(&mut app, &mut terminal, &char('x')), Flow::Redraw);
    assert!(!row(terminal.backend().buffer(), 39).contains("could not open"));
}

#[test]
fn p4_tui_05_the_views_read_a_mask_and_the_zoom_root() {
    let tree = mockup();
    let view = mockup_view(&tree);
    let (mocha, classes) = (Pack::named("mocha").unwrap(), classes());
    let mut app = viewer(&view, &mocha, &classes);
    let mut terminal = first_frame(&app, 120, 40);
    // `/*.db` then `2`: the list keeps its Cache.db, the wal filtered out.
    for c in "/*.db".chars() {
        press(&mut app, &mut terminal, &char(c));
    }
    press(&mut app, &mut terminal, &key(KeyCode::Enter));
    press(&mut app, &mut terminal, &char('2'));
    let buf = terminal.backend().buffer();
    let table = map_text(buf);
    assert!(table.contains("Cache.db"), "{table}");
    assert!(!table.contains("Cache.db-wal"), "{table}");
    assert!(row(buf, 2).contains("filter: *.db"), "{}", row(buf, 2));
    // In the largest view the mask rules the files too.
    press(&mut app, &mut terminal, &char('3'));
    let table = map_text(terminal.backend().buffer());
    assert!(!table.contains("Cache.db-wal"), "{table}");
    let db = id_of(&view, "Caches/com.apple.Safari/Cache.db");
    press(&mut app, &mut terminal, &key(KeyCode::Down));
    assert_eq!(app.selected(), db, "the first of the files the mask keeps");
    // Esc clears the filter, which the tables were reading.
    press(&mut app, &mut terminal, &key(KeyCode::Esc));
    let table = map_text(terminal.backend().buffer());
    assert!(table.contains("Cache.db-wal"), "{table}");
    // The largest view and the types view read under the map's root.
    let mut app = viewer(&view, &mocha, &classes);
    let mut terminal = first_frame(&app, 120, 40);
    press(&mut app, &mut terminal, &char('z'));
    let root = app.cursor().zoom().root();
    assert_ne!(root, 0);
    press(&mut app, &mut terminal, &char('3'));
    let buf = terminal.backend().buffer();
    let path = String::from_utf8_lossy(&view.path(root)).into_owned();
    assert!(row(buf, 2).contains(&path), "{}", row(buf, 2));
    press(&mut app, &mut terminal, &char('4'));
    assert!(row(terminal.backend().buffer(), 2).contains(&path));
}

#[test]
fn p4_tui_05_a_file_key_line_names_the_focus_and_the_sort() {
    let tree = mockup();
    let view = mockup_view(&tree);
    let (mocha, classes) = (Pack::named("mocha").unwrap(), classes());
    let mut app = viewer(&view, &mocha, &classes);
    let mut terminal = first_frame(&app, 120, 40);
    let keys = |t: &Terminal<TestBackend>| row(t.backend().buffer(), 39);
    assert!(
        keys(&terminal).contains("⏎ open   ⌫ up   z zoom   ? help   q quit"),
        "{}",
        keys(&terminal)
    );
    press(&mut app, &mut terminal, &key(KeyCode::Tab));
    let text = keys(&terminal);
    assert!(text.contains("⇥ pane   s sort size"), "{text}");
    press(&mut app, &mut terminal, &char('s'));
    assert!(
        keys(&terminal).contains("s sort name"),
        "{}",
        keys(&terminal)
    );
    // The mode is drawn in the text colour.
    let buf = terminal.backend().buffer();
    let line = row(buf, 39);
    let at = line.find("name").unwrap();
    let x = u16::try_from(line[..at].chars().count()).unwrap();
    assert_eq!(buf[(x, 39)].fg, Shown::Rgb(0xcd, 0xd6, 0xf4));
    press(&mut app, &mut terminal, &key(KeyCode::Tab));
    assert!(
        keys(&terminal).contains("c color type"),
        "{}",
        keys(&terminal)
    );
    press(&mut app, &mut terminal, &key(KeyCode::Tab));
    press(&mut app, &mut terminal, &char('4'));
    assert!(keys(&terminal).contains("⏎ filter"), "{}", keys(&terminal));
}

#[test]
fn p4_tui_05_canvas_cut_left_keeps_the_end_of_a_path() {
    let mocha = Pack::named("mocha").unwrap();
    let canvas = Canvas::new(1, 1, &mocha, Glyphs::Unicode);
    assert_eq!(canvas.cut_left("short", 10), "short");
    assert_eq!(canvas.cut_left("/Users/alex/Cache.db", 10), "…/Cache.db");
    let canvas = Canvas::new(1, 1, &mocha, Glyphs::Ascii);
    assert_eq!(canvas.cut_left("/Users/alex/Cache.db", 10), "~/Cache.db");
}

#[test]
fn p4_tui_05_age_keeps_the_ramp_on_a_tree_with_real_times() {
    let json = r#"[1,2,{"progname":"ncdu"},[{"name":"/x"},
        {"name":"new.txt","asize":100,"dsize":100,"mtime":1000000},
        {"name":"old.txt","asize":100,"dsize":100,"mtime":100}]]"#;
    let mut arena = import(json.as_bytes(), Some(ImportFormat::Ncdu))
        .unwrap()
        .arena;
    let rollup = roll_up(&mut arena);
    let view = ArenaView::new(&arena, &rollup, PathBuf::from("/x"));
    let (mocha, classes) = (Pack::named("mocha").unwrap(), classes());
    let mut paint = opts(&mocha, &classes, ColorBy::Age);
    paint.now = 1_000_000_000_000_000 + 86_400_000_000_000 / 2;
    let mut app = App::new(&view, paint, Depth::TrueColor, None, status(&view), None);
    let mut terminal = first_frame(&app, 120, 40);
    press(&mut app, &mut terminal, &char('c'));
    press(&mut app, &mut terminal, &char('c'));
    press(&mut app, &mut terminal, &char('c'));
    press(&mut app, &mut terminal, &char('c'));
    press(&mut app, &mut terminal, &char('c'));
    let text = text_of(terminal.backend().buffer());
    assert!(text.contains("by age"), "{text}");
    assert!(text.contains("< 1 day"), "{text}");
    let _ = writeln!(String::new(), "{text}");
}

/// The tables and the mask on T2 (a million entries), as the descend test
/// measures: each table's first frame, a mask's tiles read for the map,
/// and the keys that only repaint. Reported rather than asserted.
#[test]
#[ignore = "needs bench/.trees/T2 and the release binary; DUSCOPE_TEST_PLATFORM=1"]
fn p4_tui_05_the_tables_and_a_mask_on_t2() {
    use std::time::Instant;

    use duscope_core::rules::Classes;
    use duscope_testkit::TestEnv;

    use crate::cursor::tests::t2;

    let env = TestEnv::new();
    let Some(index) = t2(&env) else {
        return;
    };
    let (mocha, classes) = (Pack::named("mocha").unwrap(), Classes::vendored());
    let mut app = App::new(
        &index,
        opts(&mocha, &classes, ColorBy::Type),
        Depth::TrueColor,
        None,
        String::new(),
        None,
    );
    let mut terminal = first_frame(&app, 120, 40);
    let mut step = |what: &str, events: &[Event]| {
        let start = Instant::now();
        for event in events {
            press(&mut app, &mut terminal, event);
        }
        eprintln!("{what}: {} ms", start.elapsed().as_millis());
    };
    step("the list", &[char('2')]);
    step("the largest files", &[char('3')]);
    step("the classes", &[char('4')]);
    step("back to the map", &[char('1')]);
    let typed: Vec<Event> = "/*.txt"
        .chars()
        .map(char)
        .chain([key(KeyCode::Enter)])
        .collect();
    step("mask, its tiles read", &typed);
    step("mask cleared", &[key(KeyCode::Esc)]);
    step("the colour mode, map and legend again", &[char('c')]);
    step("sort, one folder", &[char('s')]);
    step("the help", &[char('?')]);
}
