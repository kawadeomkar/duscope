//! P4-TUI-01: the viewer on ratatui's `TestBackend`. The harness draws the
//! app into a test terminal, feeds it events and compares the buffer, its
//! colours included, with a golden file; the events quit, redraw or wait;
//! and the background probe reads the terminal's answers.

use std::convert::Infallible;
use std::fmt::Write as _;
use std::path::PathBuf;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use duscope_core::arena::Arena;
use duscope_core::interchange::import::{ImportFormat, import};
use duscope_core::query::{ArenaView, SizeKind};
use duscope_core::rollup::{RollUp, roll_up};
use duscope_core::rules::{Class, Classes};
use duscope_testkit::golden;
use duscope_treemap::cells::Depth;
use duscope_treemap::packs::Pack;
use duscope_treemap::paint::{ColorBy, Glyphs, PaintOpts};
use ratatui_core::backend::TestBackend;
use ratatui_core::terminal::Terminal;

use crate::term::parse_background;
use crate::{App, Background, Flow, run, snapshot};

/// The tree every buffer test draws: a few directories of different
/// classes, each holding one file, read from an ncdu export.
fn tree() -> (Arena, RollUp) {
    let mib = 1u64 << 20;
    let mut json = String::from(r#"[1,2,{"progname":"ncdu"},[{"name":"/Users/alex"}"#);
    for (dir, file, size) in [
        ("Library", "store.db", 79 * mib),
        ("dev", "main.rs", 54 * mib),
        ("Music", "track.mp3", 21 * mib),
        ("Documents", "notes.pdf", 12 * mib),
        ("Downloads", "setup.zip", 9 * mib),
    ] {
        let _ = write!(
            json,
            r#",[{{"name":"{dir}"}},{{"name":"{file}","asize":{size},"dsize":{size}}}]"#
        );
    }
    json.push_str("]]");
    let mut arena = import(json.as_bytes(), Some(ImportFormat::Ncdu))
        .unwrap()
        .arena;
    let rollup = roll_up(&mut arena);
    (arena, rollup)
}

/// The view of `tree`'s arena.
fn view(tree: &(Arena, RollUp)) -> ArenaView<'_> {
    ArenaView::new(&tree.0, &tree.1, PathBuf::from("/Users/alex"))
}

fn opts<'a>(pack: &'a Pack, classes: &'a Classes) -> PaintOpts<'a> {
    PaintOpts {
        pack,
        color_by: ColorBy::Type,
        glyphs: Glyphs::Unicode,
        depth: None,
        min_tile: 1,
        pool_files: false,
        size: SizeKind::Allocated,
        now: 0,
        classes,
    }
}

fn app<'a>(view: &'a ArenaView<'a>, pack: &'a Pack, classes: &'a Classes) -> App<'a> {
    App::new(
        view,
        opts(pack, classes),
        Depth::TrueColor,
        vec![
            (Class::Data, 45.0),
            (Class::Code, 31.0),
            (Class::Media, 12.0),
        ],
        "175 MiB · 5 files · 6 dirs".to_owned(),
        None,
    )
}

fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
    Event::Key(KeyEvent::new(code, modifiers))
}

fn char(c: char) -> Event {
    key(KeyCode::Char(c), KeyModifiers::NONE)
}

/// Runs `app` on a `w`×`h` test terminal, fed `events` in order.
fn run_on(app: &mut App<'_>, w: u16, h: u16, events: Vec<Event>) -> Terminal<TestBackend> {
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    let mut events = events.into_iter();
    run(&mut terminal, app, || -> Result<Event, Infallible> {
        Ok(events.next().expect("the viewer asked past the last event"))
    })
    .unwrap();
    assert_eq!(events.next(), None, "the viewer quit early");
    terminal
}

#[test]
fn p4_tui_01_the_harness_snapshots_a_frame_with_its_styles() {
    let (tree, classes) = (tree(), Classes::vendored());
    let view = view(&tree);
    let mocha = Pack::named("mocha").unwrap();
    let mut app = app(&view, &mocha, &classes);
    // A key without a binding and a resize to the same size draw nothing
    // new; q quits.
    let events = vec![char('x'), Event::Resize(100, 30), char('q')];
    let terminal = run_on(&mut app, 100, 30, events);
    golden(
        "view_buffer_tree_100x30.snap",
        &snapshot(terminal.backend().buffer()),
    );
}

#[test]
fn p4_tui_01_a_resize_draws_at_the_new_size() {
    let (tree, classes) = (tree(), Classes::vendored());
    let view = view(&tree);
    let mocha = Pack::named("mocha").unwrap();
    let app = app(&view, &mocha, &classes);
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    for (w, h) in [(100, 30), (140, 45), (80, 24)] {
        terminal.backend_mut().resize(w, h);
        terminal
            .draw(|f| app.draw(f.area(), f.buffer_mut()))
            .unwrap();
        let text = snapshot(terminal.backend().buffer());
        let rows: Vec<&str> = text.lines().take(usize::from(h)).collect();
        assert_eq!(rows.len(), usize::from(h));
        assert!(
            rows.iter().all(|r| r.chars().count() == usize::from(w)),
            "{w}×{h}"
        );
        // The map from the top row, the key line on the last.
        assert!(rows[0].starts_with('┌'), "{w}×{h}:\n{text}");
        assert_eq!(rows[usize::from(h) - 1].trim_end(), "q quit", "{w}×{h}");
    }
    // Made smaller than 80×24 while it runs, it asks for room.
    terminal.backend_mut().resize(79, 24);
    terminal
        .draw(|f| app.draw(f.area(), f.buffer_mut()))
        .unwrap();
    let text = snapshot(terminal.backend().buffer());
    assert!(
        text.starts_with("duscope view needs at least 80×24; this terminal is 79×24"),
        "{text}"
    );
}

#[test]
fn p4_tui_01_q_and_ctrl_c_quit_a_resize_redraws_the_rest_waits() {
    let (tree, classes) = (tree(), Classes::vendored());
    let view = view(&tree);
    let mocha = Pack::named("mocha").unwrap();
    let mut app = app(&view, &mocha, &classes);
    assert_eq!(app.on(&char('q')), Flow::Quit);
    assert_eq!(
        app.on(&key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        Flow::Quit
    );
    assert_eq!(app.on(&Event::Resize(120, 40)), Flow::Redraw);
    for event in [
        char('x'),
        char('Q'),
        key(KeyCode::Char('q'), KeyModifiers::ALT),
        key(KeyCode::Char('c'), KeyModifiers::NONE),
        key(KeyCode::Esc, KeyModifiers::NONE),
        Event::FocusGained,
    ] {
        assert_eq!(app.on(&event), Flow::Wait, "{event:?}");
    }
}

#[test]
fn p4_tui_01_a_late_background_answer_is_not_keys() {
    let (tree, classes) = (tree(), Classes::vendored());
    let view = view(&tree);
    let mocha = Pack::named("mocha").unwrap();
    let mut app = app(&view, &mocha, &classes);
    let open = key(KeyCode::Char(']'), KeyModifiers::ALT);
    // `ESC ] 11 ; rgb:…` after the probe stopped waiting, as crossterm
    // reads it: Alt-], the characters, then ST (Alt-\) or BEL (Ctrl-G).
    for end in [
        key(KeyCode::Char('\\'), KeyModifiers::ALT),
        key(KeyCode::Char('g'), KeyModifiers::CONTROL),
    ] {
        assert_eq!(app.on(&open), Flow::Wait);
        for c in "11;rgba:fafa/0b0b/c3c3/ffff".chars() {
            assert_eq!(app.on(&char(c)), Flow::Wait, "{c}");
        }
        assert_eq!(app.on(&end), Flow::Wait);
        assert_eq!(app.on(&char('q')), Flow::Quit);
    }
    // A key no answer holds ends it and counts: an Alt-] typed by hand
    // does not swallow the q after it, nor Ctrl-C.
    assert_eq!(app.on(&open), Flow::Wait);
    assert_eq!(app.on(&char('q')), Flow::Quit);
    assert_eq!(app.on(&open), Flow::Wait);
    assert_eq!(
        app.on(&key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        Flow::Quit
    );
}

#[test]
fn p4_tui_01_the_background_answer_names_dark_or_light() {
    let cases: [(&[u8], Option<Background>); 9] = [
        (b"\x1b]11;rgb:ffff/ffff/ffff\x1b\\", Some(Background::Light)),
        (b"\x1b]11;rgb:0000/0000/0000\x07", Some(Background::Dark)),
        // Mocha's base, Solarized light's, two digits a channel, rgba.
        (b"\x1b]11;rgb:1e1e/1e1e/2e2e\x1b\\", Some(Background::Dark)),
        (b"\x1b]11;rgb:fd/f6/e3\x07", Some(Background::Light)),
        (
            b"\x1b]11;rgba:ffff/ffff/ffff/ffff\x07",
            Some(Background::Light),
        ),
        // After other bytes, and before DA1's answer.
        (
            b"x\x1b]11;rgb:f/f/f\x07\x1b[?62;22c",
            Some(Background::Light),
        ),
        // DA1 alone, a cut answer, a colour name.
        (b"\x1b[?62;22c", None),
        (b"\x1b]11;rgb:ffff/ff", None),
        (b"\x1b]11;white\x07", None),
    ];
    for (bytes, expected) in cases {
        assert_eq!(
            parse_background(bytes),
            expected,
            "{}",
            bytes.escape_ascii()
        );
    }
}

#[test]
fn p4_tui_01_the_device_attributes_answer_ends_the_wait() {
    use crate::term::answered_da1;
    assert!(answered_da1(b"\x1b[?62;22c"));
    assert!(answered_da1(b"\x1b]11;rgb:0/0/0\x07\x1b[?1;2c"));
    assert!(!answered_da1(b"\x1b[?62;22"));
    assert!(!answered_da1(b"\x1b]11;rgb:0/0/0\x07"));
    assert!(!answered_da1(b"\x1b[?62;22R"));
}
