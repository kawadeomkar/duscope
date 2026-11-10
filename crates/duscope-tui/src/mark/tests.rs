//! P4-TUI-06: marks and the mark pane, on the canvas's example tree: what
//! `d` keeps, the pane's table beside `clean`'s, what a plan with a refused
//! item says, the freed tiles and the reload; and on a small tree on disk,
//! saved: a mark planned as `clean` plans its path. Nothing here moves a file:
//! `clean_execute` only runs through the binary, in `duscope/tests/pty`,
//! where the test environment guards the real Trash.

use std::cell::Cell;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crossterm::event::KeyCode;
use duscope_core::arena::Arena;
use duscope_core::clean::CleanOutcome;
use duscope_core::envelope::{CleanPlan, ErrorObject, PlanItem};
use duscope_core::index::{Index, IndexWriter, ScanId, ScanMeta, index_path};
use duscope_core::interchange::import::{ImportFormat, import};
use duscope_core::query::{ArenaView, SizeKind, TreeView};
use duscope_core::rollup::{RollUp, roll_up};
use duscope_testkit::{TestEnv, golden};
use duscope_treemap::cells::Depth;
use duscope_treemap::packs::Pack;
use duscope_treemap::paint::ColorBy;
use ratatui_core::backend::TestBackend;
use ratatui_core::buffer::Buffer;
use ratatui_core::layout::Rect;
use ratatui_core::terminal::Terminal;

use super::{Body, Marks, Pane, State, Toggled, plan_rows};
use crate::cursor::tests::{char, first_frame, key, press};
use crate::panes::Sources;
use crate::panes::tests::{classes, mockup, opts, status};
use crate::widgets::Canvas;
use crate::{App, Effects, Ended, Flow, Input, run_session, snapshot};

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

fn id_of(view: &dyn TreeView, path: &str) -> u32 {
    (0..view.len())
        .find(|&id| view.path(id) == path.as_bytes())
        .unwrap_or_else(|| panic!("no {path}"))
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

/// A plan of two items and one that is refused, as `clean_plan` would make
/// it of them; none of the paths has to exist.
fn plan_with_refusal() -> CleanPlan {
    let item =
        |path: &str, kind: &'static str, bytes: u64, reason: Option<&'static str>| PlanItem {
            path: path.to_owned(),
            path_bytes: path.as_bytes().to_vec(),
            name: path.rsplit('/').next().unwrap_or(path).to_owned(),
            name_raw: None,
            kind,
            allocated_bytes: bytes,
            frees_bytes: Some(bytes),
            frees_kind: Some("exact"),
            nlink: Some(1),
            links_outside_subtree: Some(0),
            regenerable_by: None,
            action: if reason.is_some() { "refused" } else { "trash" },
            reason,
            outcome: None,
            error: None,
        };
    CleanPlan {
        items: vec![
            item(
                "/Users/alex/Library/Caches/Homebrew/downloads",
                "dir",
                3_145_728,
                None,
            ),
            item("/Users/alex/Library/Logs/old.log", "file", 8192, None),
            item(
                "/Users/alex/Library/Containers",
                "symlink",
                1024,
                Some("symlink"),
            ),
        ],
        total_allocated_bytes: 3_153_920,
        total_frees_bytes: 3_153_920,
        method: "trash",
        confirmation_required: true,
    }
}

/// A plan of the same two items, nothing refused.
fn plan() -> CleanPlan {
    let mut plan = plan_with_refusal();
    plan.items.pop();
    plan
}

fn iec(n: u64) -> String {
    duscope_core::query::human_size(n)
}

/// A frame of `body` over a plain 120×40 screen.
fn frame(body: &Body, pack: &Pack, w: u16, h: u16) -> String {
    let mut canvas = Canvas::new(w, h, pack, duscope_treemap::paint::Glyphs::Unicode);
    body.draw(&mut canvas, Rect::new(0, 2, w, h - 3));
    let mut buf = Buffer::empty(Rect::new(0, 0, w, h));
    crate::app::copy(&canvas.cells, Depth::TrueColor, buf.area, &mut buf);
    snapshot(&buf)
}

#[test]
fn p4_tui_06_d_marks_and_unmarks_a_folder_covers_what_is_inside() {
    let tree = mockup();
    let view = mockup_view(&tree);
    let safari = id_of(&view, "Caches/com.apple.Safari");
    let wal = id_of(&view, "Caches/com.apple.Safari/Cache.db-wal");
    let downloads = id_of(&view, "Caches/Homebrew/downloads");
    let mut marks = Marks::default();
    assert_eq!(marks.toggle(&view, 0), Toggled::Root);
    assert!(marks.is_empty());
    assert_eq!(marks.toggle(&view, wal), Toggled::Marked);
    assert_eq!(marks.toggle(&view, wal), Toggled::Unmarked);
    assert!(marks.is_empty());
    // Inside a marked folder an entry is not marked again.
    assert_eq!(marks.toggle(&view, safari), Toggled::Marked);
    assert_eq!(marks.toggle(&view, wal), Toggled::Inside(safari));
    assert_eq!(marks.ids(), [safari]);
    // A folder's mark replaces the marks inside it.
    let mut marks = Marks::default();
    assert_eq!(marks.toggle(&view, wal), Toggled::Marked);
    assert_eq!(marks.toggle(&view, downloads), Toggled::Marked);
    assert_eq!(marks.toggle(&view, safari), Toggled::Marked);
    assert_eq!(marks.ids(), [downloads, safari]);
    let size = SizeKind::Allocated;
    assert_eq!(
        marks.total(&view, size),
        view.node(safari).size(size) + view.node(downloads).size(size)
    );
    assert_eq!(marks.len(), 2);
}

#[test]
fn p4_tui_06_d_shows_on_the_key_line_and_cap_d_without_a_saved_scan_says_so() {
    let tree = mockup();
    let view = mockup_view(&tree);
    let (mocha, classes) = (Pack::named("mocha").unwrap(), classes());
    let mut app = viewer(&view, &mocha, &classes);
    let mut terminal = first_frame(&app, 120, 40);
    let selected = app.selected();
    // `D` with nothing marked says what `d` does; the keys come back.
    assert_eq!(press(&mut app, &mut terminal, &char('D')), Flow::Redraw);
    assert!(text_of(terminal.backend().buffer()).contains("nothing is marked: d marks"));
    assert_eq!(press(&mut app, &mut terminal, &char('x')), Flow::Redraw);
    // The root cannot be marked: `G` ... the cursor starts on a file.
    assert_eq!(press(&mut app, &mut terminal, &char('d')), Flow::Redraw);
    let frame = text_of(terminal.backend().buffer());
    assert!(frame.contains("1 marked · "), "{frame}");
    assert!(frame.contains("selected · marked"), "{frame}");
    assert_eq!(app.marked_ids(), [selected]);
    // `d` again unmarks.
    assert_eq!(press(&mut app, &mut terminal, &char('d')), Flow::Redraw);
    let frame = text_of(terminal.backend().buffer());
    assert!(!frame.contains(" marked · "), "{frame}");
    assert!(app.marked_ids().is_empty());
    // Marked again, `D` opens the pane; this scan is not saved, and the
    // pane says so and moves nothing.
    assert_eq!(press(&mut app, &mut terminal, &char('d')), Flow::Redraw);
    assert_eq!(press(&mut app, &mut terminal, &char('D')), Flow::Redraw);
    assert!(app.pane_open());
    let frame = text_of(terminal.backend().buffer());
    assert!(frame.contains("mark pane · 1 marked"), "{frame}");
    assert!(frame.contains("clean plans from a saved scan"), "{frame}");
    assert!(frame.contains("nothing was moved"), "{frame}");
    assert!(frame.contains("⏎ confirm"), "{frame}");
    // Keys go to the pane: `q` does not quit; `Esc` closes; the mark stays.
    assert_eq!(press(&mut app, &mut terminal, &char('q')), Flow::Wait);
    assert!(app.pane_open());
    assert_eq!(
        press(&mut app, &mut terminal, &key(KeyCode::Esc)),
        Flow::Redraw
    );
    assert!(!app.pane_open());
    assert_eq!(app.marked_ids(), [selected]);
    // Ctrl-C quits from the pane as from anywhere.
    assert_eq!(press(&mut app, &mut terminal, &char('D')), Flow::Redraw);
    let ctrl_c = crossterm::event::Event::Key(crossterm::event::KeyEvent::new(
        KeyCode::Char('c'),
        crossterm::event::KeyModifiers::CONTROL,
    ));
    assert_eq!(press(&mut app, &mut terminal, &ctrl_c), Flow::Quit);
}

#[test]
fn p4_tui_06_the_plan_is_printed_as_clean_prints_it() {
    // The grid of `out::table`, by hand: right-aligned columns two apart,
    // the path last and whole, and the line under the table.
    let plan = plan_with_refusal();
    let (rows, line) = plan_rows(&plan, &iec);
    let lines: Vec<String> = rows.iter().map(super::PlanRow::whole).collect();
    assert_eq!(
        lines,
        [
            "          action     size    frees  path",
            "           trash  3.0 MiB  3.0 MiB  /Users/alex/Library/Caches/Homebrew/downloads",
            "           trash  8.0 KiB  8.0 KiB  /Users/alex/Library/Logs/old.log",
            "refused: symlink  1.0 KiB  1.0 KiB  /Users/alex/Library/Containers",
        ]
    );
    assert_eq!(
        line,
        "2 items to move to the Trash: 3.0 MiB on disk, 3.0 MiB freed once the Trash is \
         emptied · 1 refused"
    );
    // The `-` of a frees that is not known, a control character in a path.
    let mut plan = super::tests::plan();
    plan.items[1].frees_bytes = None;
    plan.items[1].path = "/Users/alex/a\nb".to_owned();
    let (rows, _) = plan_rows(&plan, &iec);
    assert!(
        rows[2].whole().ends_with("   -  /Users/alex/a\\x0ab"),
        "{rows:?}"
    );
}

#[test]
fn p4_tui_06_the_pane_asks_for_yes_or_says_why_it_cannot() {
    let (mocha, roles) = (
        Pack::named("mocha").unwrap(),
        Pack::named("mocha").unwrap().roles,
    );
    // A plan that moves: the table, the line, the question.
    let body = Body::planned(&plan(), 2, "ye", None, &iec, &roles);
    golden(
        "view_mark_pane_mockup_120x40.snap",
        &frame(&body, &mocha, 120, 40),
    );
    let text = frame(&body, &mocha, 120, 40);
    assert!(text.contains("╭─ mark pane · 2 marked "), "{text}");
    assert!(text.contains("2 items to move to the Trash"), "{text}");
    assert!(
        text.contains("Type yes to move them to the Trash: ye▌"),
        "{text}"
    );

    // A refused item: no question, and why nothing moves.
    let body = Body::planned(&plan_with_refusal(), 3, "", None, &iec, &roles);
    assert_eq!(body.answer, None);
    golden(
        "view_mark_pane_refused_mockup_120x40.snap",
        &frame(&body, &mocha, 120, 40),
    );
    let text = frame(&body, &mocha, 120, 40);
    assert!(text.contains("refused: symlink"), "{text}");
    assert!(
        text.contains("1 of the 3 items cannot be moved to the Trash, so nothing is moved"),
        "{text}"
    );
    assert!(!text.contains("Type yes"), "{text}");

    // A wrong answer: the note.
    let body = Body::planned(
        &plan(),
        2,
        "",
        Some("the answer was not yes, so nothing was moved"),
        &iec,
        &roles,
    );
    let text = frame(&body, &mocha, 120, 40);
    assert!(text.contains("the answer was not yes"), "{text}");

    // Moved, one of them failed: its outcome, the failure and `clean`'s
    // line; no question.
    let mut acted = plan();
    acted.items[0].outcome = Some("trashed");
    acted.items[1].outcome = Some("failed");
    acted.items[1].error = Some(ErrorObject {
        code: "trash_unavailable",
        message: "the volume has no Trash".to_owned(),
        hint: None,
        retryable: false,
        fixable_by: "user",
    });
    let outcome = CleanOutcome {
        trashed_count: 1,
        trashed_bytes: 3_145_728,
        deleted_count: 0,
        freed_bytes: 0,
        failed_count: 1,
    };
    let body = Body::acted(&acted, &outcome, 2, &iec, &roles);
    let text = frame(&body, &mocha, 120, 40);
    assert!(text.contains("trashed"), "{text}");
    assert!(
        text.contains("failed: /Users/alex/Library/Logs/old.log: the volume has no Trash"),
        "{text}"
    );
    assert!(
        text.contains("moved 1 item to the Trash, 3.0 MiB (not yet freed) · 1 not moved"),
        "{text}"
    );

    // No plan: the reason, in the pane.
    let body = Body::unplanned("/gone does not exist", 1, &roles);
    let text = frame(&body, &mocha, 120, 40);
    assert!(text.contains("/gone does not exist"), "{text}");
    assert!(text.contains("nothing was moved"), "{text}");
}

#[test]
fn p4_tui_06_the_pane_cuts_a_long_path_from_the_left_and_counts_what_does_not_fit() {
    let (mocha, roles) = (
        Pack::named("mocha").unwrap(),
        Pack::named("mocha").unwrap().roles,
    );
    let mut plan = plan();
    let mut long = "/Users/alex/".to_owned();
    long.push_str(&"very-long-folder-name/".repeat(4));
    long.push_str("keep-this-end.bin");
    plan.items[0].path = long;
    // More rows than fit at 80×24: a `… n more` row, and the line and
    // the question still shown.
    let filler = plan.items[1].clone();
    for _ in 0..30 {
        plan.items.push(filler.clone());
    }
    let body = Body::planned(&plan, 32, "", None, &iec, &roles);
    let text = frame(&body, &mocha, 80, 24);
    assert!(text.contains("…"), "{text}");
    assert!(text.contains("keep-this-end.bin"), "{text}");
    let more = text
        .lines()
        .find(|l| l.contains(" more"))
        .unwrap_or_else(|| panic!("{text}"));
    assert!(more.contains("… "), "{text}");
    assert!(text.contains("to move to the Trash"), "{text}");
    assert!(text.contains("Type yes to move them"), "{text}");
    let question_row = text
        .lines()
        .position(|l| l.contains("Type yes"))
        .unwrap_or(99);
    assert!(question_row < 24, "{text}");
}

#[test]
fn p4_tui_06_freed_tiles_and_marks_on_the_map() {
    let tree = mockup();
    let view = mockup_view(&tree);
    let (mocha, classes) = (Pack::named("mocha").unwrap(), classes());
    let wal = id_of(&view, "Caches/com.apple.Safari/Cache.db-wal");
    let downloads = id_of(&view, "Caches/Homebrew/downloads");
    let mut app = viewer(&view, &mocha, &classes);
    let mut terminal = first_frame(&app, 120, 40);
    app.free(wal);
    app.free(downloads);
    assert_eq!(press(&mut app, &mut terminal, &char('x')), Flow::Wait);
    // The freed entries are drawn with the next frame.
    terminal
        .draw(|f| app.draw(f.area(), f.buffer_mut()))
        .unwrap();
    let buf = terminal.backend().buffer().clone();
    golden("view_freed_mockup_120x40.snap", &snapshot(&buf));
    let text = text_of(&buf);
    assert!(text.contains(" freed "), "{text}");
    // The cursor is drawn over its tile; it starts on the freed file.
    assert!(text.contains("selected · freed"), "{text}");
    // `d` on what has gone says so, and marks nothing.
    assert_eq!(press(&mut app, &mut terminal, &char('d')), Flow::Redraw);
    assert!(text_of(terminal.backend().buffer()).contains("in the Trash already"));
    assert!(app.marked_ids().is_empty());

    // Marks outlined on the map.
    let mut app = viewer(&view, &mocha, &classes);
    let mut terminal = first_frame(&app, 120, 40);
    app.jump(downloads);
    terminal
        .draw(|f| app.draw(f.area(), f.buffer_mut()))
        .unwrap();
    assert_eq!(press(&mut app, &mut terminal, &char('d')), Flow::Redraw);
    // The cursor drawn off it, so its outline is the mark's.
    app.jump(wal);
    terminal
        .draw(|f| app.draw(f.area(), f.buffer_mut()))
        .unwrap();
    let buf = terminal.backend().buffer().clone();
    golden("view_marks_mockup_120x40.snap", &snapshot(&buf));
    let text = text_of(&buf);
    assert!(text.contains("┏ ✕"), "{text}");
    assert!(text.contains(" marked · "), "{text}");
}

#[test]
fn p4_tui_06_the_siblings_and_the_list_show_the_marks() {
    let tree = mockup();
    let view = mockup_view(&tree);
    let (mocha, classes) = (Pack::named("mocha").unwrap(), classes());
    let mut app = viewer(&view, &mocha, &classes);
    let mut terminal = first_frame(&app, 120, 40);
    press(&mut app, &mut terminal, &char('d'));
    let text = text_of(terminal.backend().buffer());
    assert!(text.contains("✕ Cache.d"), "{text}");
    press(&mut app, &mut terminal, &char('2'));
    let text = text_of(terminal.backend().buffer());
    assert!(text.contains("✕ Cache.db-wal"), "{text}");
    press(&mut app, &mut terminal, &char('3'));
    let text = text_of(terminal.backend().buffer());
    assert!(text.contains("✕ Cache.db-wal"), "{text}");
}

#[test]
fn p4_tui_06_r_asks_the_binary_and_landing_reloads() {
    let tree = mockup();
    let view = mockup_view(&tree);
    let (mocha, classes) = (Pack::named("mocha").unwrap(), classes());
    let mut app = viewer(&view, &mocha, &classes);
    // Without a binary behind it: says so.
    let mut terminal = first_frame(&app, 120, 40);
    assert_eq!(press(&mut app, &mut terminal, &char('r')), Flow::Redraw);
    assert!(text_of(terminal.backend().buffer()).contains("rescan is not available"));

    let starts = Rc::new(Cell::new(0));
    let counter = Rc::clone(&starts);
    let mut app = viewer(&view, &mocha, &classes);
    app.with_effects(Effects {
        rescan: Some(Box::new(move || {
            counter.set(counter.get() + 1);
            Ok(())
        })),
        ..Effects::default()
    });
    let mut terminal = first_frame(&app, 120, 40);
    assert_eq!(press(&mut app, &mut terminal, &char('r')), Flow::Redraw);
    assert_eq!(starts.get(), 1);
    let header: String = text_of(terminal.backend().buffer())
        .lines()
        .next()
        .unwrap()
        .to_owned();
    assert!(header.contains("rescanning…"), "{header}");
    // One at a time.
    assert_eq!(press(&mut app, &mut terminal, &char('r')), Flow::Redraw);
    assert_eq!(starts.get(), 1);
    assert!(text_of(terminal.backend().buffer()).contains("a rescan is running"));

    // The pane waits for it: a move now would be back on the map when
    // the rescan, begun before the move, lands.
    assert_eq!(press(&mut app, &mut terminal, &char('d')), Flow::Redraw);
    assert_eq!(press(&mut app, &mut terminal, &char('D')), Flow::Redraw);
    assert!(!app.pane_open());
    assert!(text_of(terminal.backend().buffer()).contains("a rescan is running: D opens"));

    // A landing reloads, through `run_session`'s return.
    assert_eq!(app.input(Input::Landed), Flow::Reload);

    // Waiting for the filter prompt to close.
    let mut app = viewer(&view, &mocha, &classes);
    app.with_effects(Effects {
        rescan: Some(Box::new(|| Ok(()))),
        ..Effects::default()
    });
    let mut terminal = first_frame(&app, 120, 40);
    press(&mut app, &mut terminal, &char('r'));
    press(&mut app, &mut terminal, &char('/'));
    assert_eq!(app.input(Input::Landed), Flow::Redraw);
    assert_eq!(
        press(&mut app, &mut terminal, &key(KeyCode::Esc)),
        Flow::Reload
    );

    // A failure says why, and the viewer stays.
    let mut app = viewer(&view, &mocha, &classes);
    assert_eq!(
        app.input(Input::RescanFailed("no such volume".to_owned())),
        Flow::Redraw
    );

    // The loop returns on a landing and on `q`.
    let mut app = viewer(&view, &mocha, &classes);
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
    let mut script = vec![Input::Landed].into_iter();
    let ended = run_session(&mut terminal, &mut app, || {
        Ok::<_, std::convert::Infallible>(script.next().unwrap())
    })
    .unwrap();
    assert_eq!(ended, Ended::Reload);
}

/// A tree of `kids` under `/r`, read from an ncdu export.
fn tree_of(kids: &str) -> (Arena, RollUp) {
    tree_at("/r", kids)
}

/// A tree of `kids` under `root`, read from an ncdu export.
fn tree_at(root: &str, kids: &str) -> (Arena, RollUp) {
    let json = format!(r#"[1,2,{{"progname":"ncdu"}},[{{"name":"{root}"}}{kids}]]"#);
    let mut arena = import(json.as_bytes(), Some(ImportFormat::Ncdu))
        .unwrap()
        .arena;
    let rollup = roll_up(&mut arena);
    (arena, rollup)
}

#[test]
fn p4_tui_06_a_reload_keeps_the_view_the_cursor_and_the_marks_by_path() {
    let f = |name: &str, kib: u64| {
        format!(r#"{{"name":"{name}","asize":{0},"dsize":{0}}}"#, kib * 1024)
    };
    let d = |name: &str, kids: String| format!(r#",[{{"name":"{name}"}},{kids}]"#);
    let old = tree_of(&format!(
        "{}{}{}",
        d("keep", format!("{},{}", f("a.bin", 80), f("b.bin", 40))),
        d("gone", f("c.bin", 20)),
        format_args!(",{}", f("f.bin", 10))
    ));
    let new = tree_of(&format!(
        "{}{}",
        d("keep", format!("{},{}", f("b.bin", 40), f("d.bin", 30))),
        format_args!(",{}", f("f.bin", 11))
    ));
    let old_view = ArenaView::new(&old.0, &old.1, PathBuf::from("/r"));
    let new_view = ArenaView::new(&new.0, &new.1, PathBuf::from("/r"));
    let (mocha, classes) = (Pack::named("mocha").unwrap(), classes());
    let mut app = viewer(&old_view, &mocha, &classes);
    let mut terminal = first_frame(&app, 120, 40);
    // Zoom into `keep`, put the cursor on `a.bin`, sort, the list view,
    // and two marks: one that will go, one that stays.
    app.jump(id_of(&old_view, "keep"));
    press(&mut app, &mut terminal, &char('z'));
    app.jump(id_of(&old_view, "keep/a.bin"));
    press(&mut app, &mut terminal, &char('s'));
    press(&mut app, &mut terminal, &char('2'));
    let mut marked = Marks::default();
    marked.toggle(&old_view, id_of(&old_view, "gone/c.bin"));
    marked.toggle(&old_view, id_of(&old_view, "f.bin"));
    app.set_marks(marked);
    let carried = app.carry();

    let mut app = viewer(&new_view, &mocha, &classes);
    app.restore(carried);
    let mut terminal = first_frame(&app, 120, 40);
    assert_eq!(app.tab_label(), "list");
    // The file under the cursor has gone: the cursor is on its folder,
    // still inside the zoom.
    let keep = id_of(&new_view, "keep");
    assert_eq!(app.selected(), keep);
    assert_eq!(app.zoom_root(), keep);
    // The mark on what is gone is gone with it; the other is kept.
    assert_eq!(app.marked_ids(), [id_of(&new_view, "f.bin")]);
    let text = text_of(terminal.backend().buffer());
    assert!(text.contains("rescanned"), "{text}");
    assert!(text.contains("by name"), "{text}");
    // What the reload forgot: the freed tiles (a new App has none) and the
    // answer.
    assert_eq!(press(&mut app, &mut terminal, &char('x')), Flow::Redraw);
}

/// A tree on disk in `env`'s scratch directory (`big.bin`, `dir/b.bin`,
/// `small.bin`), and its saved scan, read from an ncdu export of the same
/// names and sizes: what `clean_plan` reads as a scan of the tree.
fn on_disk(env: &TestEnv) -> (PathBuf, Index) {
    let root = fs::canonicalize(env.tmp_dir()).unwrap().join("tree");
    fs::create_dir_all(root.join("dir")).unwrap();
    fs::write(root.join("big.bin"), vec![1u8; 64 * 1024]).unwrap();
    fs::write(root.join("dir/b.bin"), vec![2u8; 16 * 1024]).unwrap();
    fs::write(root.join("small.bin"), vec![3u8; 4 * 1024]).unwrap();
    let file =
        |name: &str, bytes: u64| format!(r#"{{"name":"{name}","asize":{bytes},"dsize":{bytes}}}"#);
    let kids = format!(
        ",{},[{{\"name\":\"dir\"}},{}],{}",
        file("big.bin", 64 * 1024),
        file("b.bin", 16 * 1024),
        file("small.bin", 4 * 1024)
    );
    let root_text = root.to_str().unwrap();
    let tree = tree_at(root_text, &kids);
    let meta = ScanMeta {
        scan_id: ScanId::new(root_text.as_bytes(), 1, [0; 8]),
        root: root_text.as_bytes().to_vec(),
        root_as_given: root_text.as_bytes().to_vec(),
        started_at: 1,
        finished_at: 2,
        options: "{}".to_owned(),
        platform: "test".to_owned(),
        scanning_uid: 501,
        thread_count: 1,
        size_kind: "allocated",
        volume_total: 1 << 40,
        volume_free: 1 << 39,
        unaccounted_bytes: None,
        error_count: 0,
        vanished_count: 0,
        excluded_count: 0,
        clones_unavailable: true,
        incremental: false,
        parent_scan_id: None,
        source: "scan".to_owned(),
        tool_version: "0.0.0".to_owned(),
    };
    let dir = env.tmp_dir().join("index");
    fs::create_dir_all(&dir).unwrap();
    IndexWriter::write(&tree.0, &tree.1, &meta, &dir).unwrap();
    let index = Index::open(&index_path(&dir, &meta.scan_id)).unwrap();
    (root, index)
}

/// The plan of the pane `pane`, which must have one.
fn planned(pane: &Pane) -> &CleanPlan {
    match &pane.state {
        State::Planned(plan) => &plan.plan,
        other => panic!("no plan: {other:?}"),
    }
}

#[test]
fn p4_tui_06_a_mark_is_planned_as_clean_plans_its_path() {
    let env = TestEnv::new();
    let (root, index) = on_disk(&env);
    let mut marks = Marks::default();
    marks.toggle(&index, id_of(&index, "dir/b.bin"));
    // As scanned: the path the scan holds, to be moved.
    let pane = Pane::open(&marks, &index, &Sources::of(&index));
    let item = &planned(&pane).items[0];
    assert_eq!((item.action, item.reason), ("trash", None));
    assert_eq!(item.path, root.join("dir/b.bin").to_string_lossy());
    assert!(pane.asks());

    // `dir` replaced since the scan by a link to a folder outside the
    // scan's root: the mark is planned where the link leads and refused
    // there, `outside_root`, as `clean` plans and refuses the path it is
    // given (its parent resolved); the pane asks nothing.
    let outside = fs::canonicalize(env.tmp_dir()).unwrap().join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("b.bin"), vec![9u8; 1024]).unwrap();
    fs::rename(root.join("dir"), root.join("dir.old")).unwrap();
    std::os::unix::fs::symlink(&outside, root.join("dir")).unwrap();
    let pane = Pane::open(&marks, &index, &Sources::of(&index));
    let item = &planned(&pane).items[0];
    assert_eq!(
        (item.action, item.reason),
        ("refused", Some("outside_root"))
    );
    assert_eq!(item.path, outside.join("b.bin").to_string_lossy());
    assert!(!pane.asks());
    let mocha = Pack::named("mocha").unwrap();
    let body = Body::planned(planned(&pane), 1, "", None, &iec, &mocha.roles);
    let text = frame(&body, &mocha, 120, 40);
    assert!(text.contains("refused: outside root"), "{text}");
    assert!(!text.contains("Type yes"), "{text}");
    assert!(outside.join("b.bin").exists());
}
