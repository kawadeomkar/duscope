//! P4-TUI-04: the panes' facts. The mode row names the owner, the siblings
//! pane shows the selection wherever the list reaches, the legend pools
//! what is past seven, and `i` grows the selected pane into the full fact
//! list, from the scan and `explain`, beside the map or over it; every
//! value read from the entry or its children. The canvas's example tree,
//! saved as an index, snapshots the facts at the four sizes; T2 times `i`
//! on its largest folders.

use std::borrow::Cow;
use std::cell::Cell;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use crossterm::event::KeyCode;
use duscope_core::arena::Arena;
use duscope_core::index::{Index, IndexWriter, ScanId, ScanMeta, index_path};
use duscope_core::interchange::import::{ImportFormat, import};
use duscope_core::query::{ArenaView, Node, SizeKind, TreeView};
use duscope_core::rollup::{RollUp, roll_up};
use duscope_core::rules::Classes;
use duscope_testkit::{TestEnv, golden};
use duscope_treemap::cells::Depth;
use duscope_treemap::packs::Pack;
use duscope_treemap::paint::{ColorBy, Glyphs, classes_of};
use ratatui_core::backend::TestBackend;
use ratatui_core::layout::Rect;
use ratatui_core::terminal::Terminal;

use super::{Facts, ScanFacts, Sources, item_rows};
use crate::cursor::tests::{char, first_frame, folders, key, percentiles, press, t2};
use crate::panes::selected::mode_owner;
use crate::panes::siblings::window;
use crate::panes::tests::{classes, mockup, opts, status};
use crate::panes::{Context, Row, legend_rows, ranks};
use crate::path::siblings;
use crate::widgets::Canvas;
use crate::{App, Flow, snapshot};

/// When the example's scan started: 2026-10-21T23:20:00Z.
const STARTED: i64 = 1_792_624_800_000_000_000;

/// `tree`, scanned from `root`, saved as an index in `env`'s scratch
/// directory: a 500 GiB volume with 120 GiB free, 3 GiB of it not found,
/// scanned in 6.8 s by 8 threads.
fn saved(env: &TestEnv, tree: &(Arena, RollUp), root: &str) -> Index {
    let meta = ScanMeta {
        scan_id: ScanId::new(root.as_bytes(), STARTED, [0; 8]),
        root: root.as_bytes().to_vec(),
        root_as_given: root.as_bytes().to_vec(),
        started_at: STARTED,
        finished_at: STARTED + 6_800_000_000,
        options: "{}".to_owned(),
        platform: "test".to_owned(),
        scanning_uid: 501,
        thread_count: 8,
        size_kind: "allocated",
        volume_total: 500 << 30,
        volume_free: 120 << 30,
        unaccounted_bytes: Some(3 << 30),
        error_count: 0,
        vanished_count: 0,
        // What the scan left out, as a scan's summary counts it.
        excluded_count: tree.0.skipped().len() as u64,
        clones_unavailable: false,
        incremental: false,
        parent_scan_id: None,
        source: "scan".to_owned(),
        tool_version: "0.0.0".to_owned(),
    };
    let dir = env.tmp_dir();
    IndexWriter::write(&tree.0, &tree.1, &meta, &dir).unwrap();
    Index::open(&index_path(&dir, &meta.scan_id)).unwrap()
}

/// A tree read from the children `kids` of an ncdu export rooted at
/// `root`.
fn ncdu(root: &str, kids: &str) -> (Arena, RollUp) {
    let json = format!(r#"[1,2,{{"progname":"ncdu"}},[{{"name":"{root}"}}{kids}]]"#);
    let mut arena = import(json.as_bytes(), Some(ImportFormat::Ncdu))
        .unwrap()
        .arena;
    let rollup = roll_up(&mut arena);
    (arena, rollup)
}

/// The viewer of `view` from `/Users/alex`.
fn viewer<'a>(
    view: &'a dyn TreeView,
    pack: &'a Pack,
    classes: &'a Classes,
    status: String,
) -> App<'a> {
    App::new(
        view,
        opts(pack, classes, ColorBy::Type),
        Depth::TrueColor,
        Some(Path::new("/Users/alex")),
        status,
        None,
    )
}

/// The rows of `terminal`'s screen, as text.
fn screen(terminal: &Terminal<TestBackend>) -> Vec<String> {
    snapshot(terminal.backend().buffer())
        .split("--- styles")
        .next()
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// Columns `from..to` of `row`, trimmed at the end.
fn cols(row: &str, from: usize, to: usize) -> String {
    row.chars()
        .skip(from)
        .take(to - from)
        .collect::<String>()
        .trim_end()
        .to_owned()
}

/// The id of the entry at `path` (`/`-separated, from the root).
fn id_of(view: &dyn TreeView, path: &str) -> u32 {
    (0..view.len())
        .find(|&id| view.path(id) == path.as_bytes())
        .unwrap_or_else(|| panic!("no {path}"))
}

/// The rows of the facts `i` adds for `id`, as `label  value` text.
fn fact_lines(ctx: &Context<'_>, facts: &Facts, id: u32) -> Vec<String> {
    let mocha = Pack::named("mocha").unwrap();
    let canvas = Canvas::new(1, 1, &mocha, Glyphs::Unicode);
    item_rows(&canvas, ctx, facts, id)
        .into_iter()
        .filter_map(|row| match row {
            Row::Fact(label, spans) => Some(format!(
                "{label:<10}{}",
                spans.iter().map(|s| s.text.as_str()).collect::<String>()
            )),
            _ => None,
        })
        .collect()
}

// ------------------------------------------------------------- selected

#[test]
fn p4_tui_04_the_mode_row_names_the_owner_or_reads_a_dash() {
    assert_eq!(
        mode_owner(Some(0o644), Some((501, 20)), false),
        "-rw-r--r-- · 501:20"
    );
    assert_eq!(
        mode_owner(Some(0o755), Some((0, 0)), true),
        "drwxr-xr-x · 0:0"
    );
    // A basic scan collects neither.
    assert_eq!(mode_owner(None, None, false), "—");
    // A mode without an owner, as an import may give it.
    assert_eq!(mode_owner(Some(0o600), None, false), "-rw-------");
}

// ------------------------------------------------------------- siblings

#[test]
fn p4_tui_04_the_siblings_window_holds_the_selection() {
    // The first page: nine and `… n more`, as P4-TUI-02 lists them.
    assert_eq!(window(60, 0, 9, 18), (0, 9));
    assert_eq!(window(60, 8, 9, 18), (0, 9));
    // Past it, the page the selection falls on, a `PgDn` from the first.
    assert_eq!(window(60, 9, 9, 18), (9, 9));
    assert_eq!(window(60, 17, 9, 18), (9, 9));
    assert_eq!(window(60, 18, 9, 18), (18, 9));
    // The last page, short.
    assert_eq!(window(60, 59, 9, 18), (54, 6));
    // All of a short list.
    assert_eq!(window(5, 4, 9, 18), (0, 5));
    // Three rows: two then `… n more`; past them, one between the rows
    // above and below.
    assert_eq!(window(60, 1, 9, 3), (0, 2));
    assert_eq!(window(60, 2, 9, 3), (2, 1));
    assert_eq!(window(60, 40, 9, 3), (40, 1));
}

#[test]
fn p4_tui_04_the_siblings_pane_reaches_tiles_too_small_to_draw() {
    // Sixty files halving every three: the last are too small for a tile.
    let mut kids = String::new();
    for i in 0..60u32 {
        let size = 1u64 << (40 - i / 3);
        let _ = write!(
            kids,
            r#",{{"name":"f{i:02}","asize":{size},"dsize":{size}}}"#
        );
    }
    let tree = ncdu("/scan", &kids);
    let view = ArenaView::new(&tree.0, &tree.1, PathBuf::from("/scan"));
    let (mocha, classes) = (Pack::named("mocha").unwrap(), Classes::vendored());
    let mut app = viewer(&view, &mocha, &classes, status(&view));
    let mut terminal = first_frame(&app, 160, 45);
    let order = siblings(&view, 0, SizeKind::Allocated);
    // The last sibling, which the map pools into its `+N small` tile.
    assert_eq!(press(&mut app, &mut terminal, &char('G')), Flow::Redraw);
    assert_eq!(app.selected(), order[59]);
    let rows = screen(&terminal);
    let list: Vec<String> = (15..24).map(|y| cols(&rows[y], 116, 159)).collect();
    // The larger ones above pooled, then the last page, the cursor on its
    // last row; nothing below.
    assert!(list[0].starts_with("  ■ … 54 more"), "{list:#?}");
    assert!(list[1].starts_with("  ■ f54"), "{list:#?}");
    assert!(list[6].starts_with("▸ ■ f59"), "{list:#?}");
    assert_eq!(list[7], "", "{list:#?}");
    // A page back: the larger ones above, nine, and the rest below.
    assert_eq!(
        press(&mut app, &mut terminal, &key(KeyCode::PageUp)),
        Flow::Redraw
    );
    let rows = screen(&terminal);
    let list: Vec<String> = (15..27).map(|y| cols(&rows[y], 116, 159)).collect();
    assert!(list[0].starts_with("  ■ … 45 more"), "{list:#?}");
    assert!(list[1].starts_with("  ■ f45"), "{list:#?}");
    assert!(list[6].starts_with("▸ ■ f50"), "{list:#?}");
    assert!(list[10].starts_with("  ■ … 6 more"), "{list:#?}");
    // Home: the first page again, as P4-TUI-02 drew it.
    press(&mut app, &mut terminal, &key(KeyCode::Home));
    let rows = screen(&terminal);
    assert!(cols(&rows[15], 116, 159).starts_with("▸ ■ f00"));
    assert!(cols(&rows[24], 116, 159).starts_with("  ■ … 51 more"));
}

// --------------------------------------------------------------- legend

#[test]
fn p4_tui_04_the_legend_shows_the_top_seven_and_pools_the_rest() {
    let (mocha, classes) = (Pack::named("mocha").unwrap(), Classes::vendored());
    for (folders, shown) in [(8u32, 8usize), (9, 8), (12, 8)] {
        let mut kids = String::new();
        for i in 0..folders {
            let size = u64::from(folders - i) << 20;
            let _ = write!(
                kids,
                r#",[{{"name":"d{i}"}},{{"name":"x.bin","asize":{size},"dsize":{size}}}]"#
            );
        }
        let tree = ncdu("/r", &kids);
        let view = ArenaView::new(&tree.0, &tree.1, PathBuf::from("/r"));
        let opts = opts(&mocha, &classes, ColorBy::Folder);
        let app = App::new(
            &view,
            opts.clone(),
            Depth::TrueColor,
            None,
            String::new(),
            None,
        );
        let terminal = first_frame(&app, 160, 45);
        let rows = screen(&terminal);
        let legend: Vec<String> = (35..43)
            .map(|y| cols(&rows[y], 116, 128))
            .filter(|r| !r.is_empty())
            .collect();
        assert_eq!(legend.len(), shown, "{folders}: {legend:#?}");
        // Eight fit whole; more are seven and `… n more`.
        let last = &legend[shown - 1];
        if folders == 8 {
            assert_eq!(last, "■ d7", "{legend:#?}");
        } else {
            assert_eq!(last, &format!("■ … {} more", folders - 7), "{legend:#?}");
        }
        // The rows are the legend's own, worked out once.
        let class_of = classes_of(&view, &classes);
        let ranks = ranks(&view, &opts);
        let dominant = std::cell::RefCell::default();
        let (_, all) = legend_rows(&Context {
            view: &view,
            opts: &opts,
            class_of: &class_of,
            dominant: &dominant,
            ranks: &ranks,
            root: "/r",
            root_parent: "/",
        });
        assert_eq!(all.len(), folders as usize);
    }
}

// ---------------------------------------------------------------- facts

#[test]
fn p4_tui_04_i_grows_the_selected_pane_into_the_facts_and_esc_closes_it() {
    let env = TestEnv::new();
    let tree = mockup();
    let index = saved(&env, &tree, "/Users/alex/Library");
    let (mocha, classes) = (Pack::named("mocha").unwrap(), classes());
    let mut app = viewer(&index, &mocha, &classes, String::new());
    let mut terminal = first_frame(&app, 160, 45);
    assert!(!app.facts_open());
    assert_eq!(press(&mut app, &mut terminal, &char('i')), Flow::Redraw);
    assert!(app.facts_open());
    let rows = screen(&terminal);
    // The selected pane runs down the right column, over the siblings and
    // the legend.
    assert!(cols(&rows[2], 115, 160).starts_with("╭─ selected ─"));
    assert!(cols(&rows[43], 115, 160).starts_with("╰─"));
    let pane: Vec<String> = (3..43).map(|y| cols(&rows[y], 116, 159)).collect();
    assert_eq!(pane[0], "Cache.db-wal");
    assert_eq!(pane[9], "mode      —");
    assert_eq!(
        &pane[11..23],
        [
            "apparent  1.84 GiB",
            "links     1",
            "clones    not a clone",
            "freeable  1.84 GiB · exact",
            "git       not in a repository",
            "attrs     basic · rerun with --attrs full",
            "",
            "volume    500.00 GiB · 120.00 GiB free",
            "not found 3.00 GiB",
            "scanned   2026-10-21 23:20 · 6.8s",
            "measure   allocated",
            "threads   8",
        ]
    );
    // `i` again closes it; so does `Esc`, before it ascends.
    assert_eq!(press(&mut app, &mut terminal, &char('i')), Flow::Redraw);
    assert!(!app.facts_open());
    assert!(cols(&screen(&terminal)[14], 115, 160).starts_with("╭─ in com.apple.Safari"));
    let selected = app.selected();
    press(&mut app, &mut terminal, &char('i'));
    assert_eq!(
        press(&mut app, &mut terminal, &key(KeyCode::Esc)),
        Flow::Redraw
    );
    assert!(!app.facts_open());
    assert_eq!(app.selected(), selected);
    // With the facts open, they follow the cursor.
    press(&mut app, &mut terminal, &char('i'));
    press(&mut app, &mut terminal, &key(KeyCode::Backspace));
    let rows = screen(&terminal);
    let pane: Vec<String> = (3..43).map(|y| cols(&rows[y], 116, 159)).collect();
    assert_eq!(pane[0], "com.apple.Safari");
    assert!(
        pane.contains(&"newest    2026-10-21 23:14".to_owned()),
        "{pane:#?}"
    );
    assert!(
        pane.contains(&"largest   Cache.db-wal · 1.84 GiB · 52%".to_owned()),
        "{pane:#?}"
    );
    assert!(
        pane.contains(&"unread    none inside".to_owned()),
        "{pane:#?}"
    );
}

#[test]
fn p4_tui_04_folded_the_facts_open_over_the_map() {
    let env = TestEnv::new();
    let tree = mockup();
    let index = saved(&env, &tree, "/Users/alex/Library");
    let (mocha, classes) = (Pack::named("mocha").unwrap(), classes());
    let mut app = viewer(&index, &mocha, &classes, String::new());
    let mut terminal = first_frame(&app, 100, 30);
    press(&mut app, &mut terminal, &char('i'));
    let rows = screen(&terminal);
    // The map's pane holds the facts, the folded pane stays under it.
    assert!(rows[2].starts_with("╭─ facts ─"), "{}", rows[2]);
    assert!(rows[23].starts_with("╭─ selected ─"), "{}", rows[23]);
    let left: Vec<String> = (3..22).map(|y| cols(&rows[y], 1, 49)).collect();
    let right: Vec<String> = (3..22).map(|y| cols(&rows[y], 50, 99)).collect();
    assert_eq!(left[0], "mode      —");
    assert!(
        left.contains(&"freeable  1.84 GiB · exact".to_owned()),
        "{left:#?}"
    );
    // The scan's rows in a column of their own.
    assert_eq!(right[0], "volume    500.00 GiB · 120.00 GiB free");
    assert_eq!(right[4], "threads   8");
    press(&mut app, &mut terminal, &key(KeyCode::Esc));
    assert!(screen(&terminal)[2].starts_with("╭─ treemap ·"));
}

#[test]
fn p4_tui_04_snapshots_of_the_facts_at_the_four_sizes() {
    let env = TestEnv::new();
    let tree = mockup();
    let index = saved(&env, &tree, "/Users/alex/Library");
    let view = ArenaView::new(&tree.0, &tree.1, PathBuf::from("/Users/alex/Library"));
    let (mocha, classes) = (Pack::named("mocha").unwrap(), classes());
    for (w, h) in [(160, 45), (120, 40), (100, 30), (80, 24)] {
        let mut app = viewer(&index, &mocha, &classes, status(&view));
        let mut terminal = first_frame(&app, w, h);
        let mut out = String::new();
        for (label, event) in [
            ("i on Cache.db-wal", char('i')),
            ("⌫ to com.apple.Safari", key(KeyCode::Backspace)),
        ] {
            assert_eq!(press(&mut app, &mut terminal, &event), Flow::Redraw);
            let _ = writeln!(out, "=== {label}");
            out.push_str(&snapshot(terminal.backend().buffer()));
        }
        golden(&format!("view_facts_mockup_{w}x{h}.snap"), &out);
    }
}

#[test]
fn p4_tui_04_hard_links_name_the_first_and_count_those_outside() {
    let env = TestEnv::new();
    let mib = 1u64 << 20;
    let link = format!(r#""asize":{mib},"dsize":{mib},"ino":7,"nlink":2"#);
    let tree = ncdu(
        "/r",
        &format!(
            r#",[{{"name":"a"}},{{"name":"x.bin",{link}}}],[{{"name":"b"}},{{"name":"y.bin",{link}}}]"#
        ),
    );
    let index = saved(&env, &tree, "/r");
    let (mocha, classes) = (Pack::named("mocha").unwrap(), Classes::vendored());
    let opts = opts(&mocha, &classes, ColorBy::Type);
    let class_of = classes_of(&index, &classes);
    let ranks = ranks(&index, &opts);
    let dominant = std::cell::RefCell::default();
    let ctx = Context {
        view: &index,
        opts: &opts,
        class_of: &class_of,
        dominant: &dominant,
        ranks: &ranks,
        root: "/r",
        root_parent: "/",
    };
    let sources = Sources::of(&index);
    let lines = |path: &str| {
        let id = id_of(&index, path);
        fact_lines(&ctx, &Facts::read(&ctx, &sources, id), id)
    };
    // The first link carries the bytes; the later one names it.
    assert!(lines("a/x.bin").contains(&"links     2 · this is the first".to_owned()));
    assert!(lines("b/y.bin").contains(&"links     2 · first /r/a/x.bin".to_owned()));
    // Each folder holds one link of the inode, and one lies outside it.
    let b = lines("b");
    assert!(b.contains(&"links     1 outside".to_owned()), "{b:#?}");
    // Its bytes stay while the link outside does: none freed.
    assert!(
        b.contains(&"freeable  0 B · lower bound".to_owned()),
        "{b:#?}"
    );
}

#[test]
fn p4_tui_04_a_scan_not_saved_is_answered_from_a_fresh_stat() {
    let env = TestEnv::new();
    let root = fs::canonicalize(env.tmp_dir()).unwrap().join("unsaved");
    fs::create_dir_all(root.join("dir")).unwrap();
    fs::write(root.join("dir/a.txt"), b"hello").unwrap();
    let root_text = root.to_str().unwrap();
    let tree = ncdu(
        root_text,
        r#",[{"name":"dir"},{"name":"a.txt","asize":5,"dsize":4096}]"#,
    );
    let view = ArenaView::new(&tree.0, &tree.1, root.clone());
    let (mocha, classes) = (Pack::named("mocha").unwrap(), Classes::vendored());
    let opts = opts(&mocha, &classes, ColorBy::Type);
    let class_of = classes_of(&view, &classes);
    let ranks = ranks(&view, &opts);
    let dominant = std::cell::RefCell::default();
    let ctx = Context {
        view: &view,
        opts: &opts,
        class_of: &class_of,
        dominant: &dominant,
        ranks: &ranks,
        root: root_text,
        root_parent: "/",
    };
    let sources = Sources::of(&view);
    assert!(sources.index.is_none());
    let id = id_of(&view, "dir/a.txt");
    let facts = Facts::read(&ctx, &sources, id);
    assert!(!facts.saved);
    let explained = facts.explain.as_ref().unwrap();
    assert_eq!(explained.confidence, "low");
    let lines = fact_lines(&ctx, &facts, id);
    assert!(
        lines.contains(&"saved     no · rerun with --save".to_owned()),
        "{lines:#?}"
    );
    assert!(lines.contains(&"links     1".to_owned()), "{lines:#?}");
    // A folder's: what a stat does not tell reads `—`.
    let dir = id_of(&view, "dir");
    let lines = fact_lines(&ctx, &Facts::read(&ctx, &sources, dir), dir);
    assert!(lines.contains(&"freeable  —".to_owned()), "{lines:#?}");
    assert!(lines.contains(&"unread    —".to_owned()), "{lines:#?}");
    // A path gone since the scan: why, in place of the facts.
    fs::remove_dir_all(&root).unwrap();
    let facts = Facts::read(&ctx, &sources, id);
    let lines = fact_lines(&ctx, &facts, id);
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("explain") && l.contains("does not exist")),
        "{lines:#?}"
    );
}

#[test]
fn p4_tui_04_a_folder_not_entered_says_why() {
    let env = TestEnv::new();
    let tree = ncdu(
        "/r",
        r#",{"name":"skipped","excluded":"pattern","mode":16877},[{"name":"in"},{"name":"a.bin","asize":10,"dsize":10}]"#,
    );
    let index = saved(&env, &tree, "/r");
    let (mocha, classes) = (Pack::named("mocha").unwrap(), Classes::vendored());
    let opts = opts(&mocha, &classes, ColorBy::Type);
    let class_of = classes_of(&index, &classes);
    let ranks = ranks(&index, &opts);
    let dominant = std::cell::RefCell::default();
    let ctx = Context {
        view: &index,
        opts: &opts,
        class_of: &class_of,
        dominant: &dominant,
        ranks: &ranks,
        root: "/r",
        root_parent: "/",
    };
    let sources = Sources::of(&index);
    let freeable = |path: &str| {
        let id = id_of(&index, path);
        let lines = fact_lines(&ctx, &Facts::read(&ctx, &sources, id), id);
        lines
            .into_iter()
            .find(|l| l.starts_with("freeable"))
            .unwrap()
    };
    assert_eq!(freeable("skipped"), "freeable  — · not entered (excluded)");
    assert_eq!(freeable("in"), "freeable  10 B · exact");
}

/// A view that counts the entries read from it.
struct Counted<'v> {
    view: &'v dyn TreeView,
    nodes: Cell<usize>,
}

impl TreeView for Counted<'_> {
    fn len(&self) -> u32 {
        self.view.len()
    }

    fn node(&self, id: u32) -> Node {
        self.nodes.set(self.nodes.get() + 1);
        self.view.node(id)
    }

    fn root_path(&self) -> &Path {
        self.view.root_path()
    }

    fn full_attrs(&self) -> bool {
        self.view.full_attrs()
    }

    fn name(&self, id: u32) -> Cow<'_, [u8]> {
        self.view.name(id)
    }

    fn as_index(&self) -> Option<(&Index, u32)> {
        self.view.as_index()
    }
}

#[test]
fn p4_tui_04_every_fact_reads_the_entry_or_its_children() {
    // Four folders, each with 4 subfolders of 50 files: 1,025 entries.
    let mut kids = String::new();
    for d in 0..4 {
        let _ = write!(kids, r#",[{{"name":"d{d}"}}"#);
        for s in 0..4 {
            let _ = write!(kids, r#",[{{"name":"s{s}"}}"#);
            for f in 0..50 {
                let size = 1000 + f + s * 100;
                let _ = write!(
                    kids,
                    r#",{{"name":"f{f}.bin","asize":{size},"dsize":{size},"mtime":{}}}"#,
                    1_700_000_000 + d * 1000 + s * 100 + f
                );
            }
            kids.push(']');
        }
        kids.push(']');
    }
    let tree = ncdu("/r", &kids);
    let env = TestEnv::new();
    let index = saved(&env, &tree, "/r");
    let counted = Counted {
        view: &index,
        nodes: Cell::new(0),
    };
    let (mocha, classes) = (Pack::named("mocha").unwrap(), Classes::vendored());
    let opts = opts(&mocha, &classes, ColorBy::Type);
    let class_of = classes_of(&index, &classes);
    let ranks = ranks(&index, &opts);
    let dominant = std::cell::RefCell::default();
    let ctx = Context {
        view: &counted,
        opts: &opts,
        class_of: &class_of,
        dominant: &dominant,
        ranks: &ranks,
        root: "/r",
        root_parent: "/",
    };
    let sources = Sources::of(&index);
    for (path, children) in [("", 4), ("d2", 4), ("d2/s3", 50)] {
        let id = if path.is_empty() {
            0
        } else {
            id_of(&index, path)
        };
        counted.nodes.set(0);
        let facts = Facts::read(&ctx, &sources, id);
        let lines = fact_lines(&ctx, &facts, id);
        let read = counted.nodes.get();
        assert!(
            read <= 3 * children + 12,
            "{path:?}: {read} entries read for {children} children"
        );
        assert!(lines.iter().any(|l| l.starts_with("largest")), "{lines:#?}");
    }
    // The oldest child is the one changed least recently, as its newest.
    let d2 = id_of(&index, "d2");
    let facts = Facts::read(&ctx, &sources, d2);
    let (oldest, _) = facts.oldest.unwrap();
    assert_eq!(index.path(oldest), b"d2/s0");
    let (largest, bytes) = facts.largest.unwrap();
    assert_eq!(index.path(largest), b"d2/s3");
    assert_eq!(bytes, index.node(largest).allocated);
}

#[test]
fn p4_tui_04_the_scan_facts_are_the_saved_scans_own() {
    let env = TestEnv::new();
    let tree = mockup();
    let index = saved(&env, &tree, "/Users/alex/Library");
    assert_eq!(
        ScanFacts::of_index(&index),
        ScanFacts {
            volume: Some((500 << 30, 120 << 30)),
            unaccounted_bytes: Some(3 << 30),
            times: Some((STARTED, STARTED + 6_800_000_000)),
            threads: Some(8),
        }
    );
    // A view of the arena reads no saved scan: no scan facts unless given.
    let view = ArenaView::new(&tree.0, &tree.1, PathBuf::from("/Users/alex/Library"));
    assert_eq!(Sources::of(&view).scan, ScanFacts::default());
    let (mocha, classes) = (Pack::named("mocha").unwrap(), classes());
    let mut app = viewer(&view, &mocha, &classes, String::new());
    app.with_sources(Sources {
        index: Some((&index, 0)),
        ..Sources::of(&view)
    });
    let mut terminal = first_frame(&app, 160, 45);
    press(&mut app, &mut terminal, &char('i'));
    let rows = screen(&terminal);
    let pane: Vec<String> = (3..43).map(|y| cols(&rows[y], 116, 159)).collect();
    // The arena's view, the saved scan's facts: the same ids.
    assert!(
        pane.contains(&"freeable  1.84 GiB · exact".to_owned()),
        "{pane:#?}"
    );
    assert!(pane.contains(&"volume    —".to_owned()), "{pane:#?}");
}

// --------------------------------------------------------------- timing

/// `i` on T2's folders of at least 400,000 entries (the root, whose eight
/// top-level folders hold about 125,000 each: T2 is fdu's balanced tree,
/// fan-out 8) and on 200 folders all over it, each from a frame with the
/// facts closed to the frame that shows them: `explain` of the saved scan
/// (git's facts from the repository T2 lies in), the folder's children and
/// the frame. Run as P4-TUI-03's descend test is: `DUSCOPE_TEST_PLATFORM=1`,
/// a release build, the release binary built, T2 at `bench/.trees/T2`.
#[test]
#[ignore = "needs bench/.trees/T2 and the release binary; DUSCOPE_TEST_PLATFORM=1"]
fn p4_tui_04_i_on_a_400k_entry_directory_answers_under_50_ms_on_t2() {
    use std::time::{Duration, Instant};

    let env = TestEnv::new();
    let Some(index) = t2(&env) else {
        return;
    };
    let (mocha, classes) = (Pack::named("mocha").unwrap(), Classes::vendored());
    let mut app = viewer(&index, &mocha, &classes, String::new());
    let mut terminal = first_frame(&app, 120, 40);
    let entries = |id: u32| u64::from(index.node(id).last_id - id) + 1;
    let large: Vec<u32> = std::iter::once(0)
        .chain(index.child_dirs(0).into_iter().map(|(id, _)| id))
        .filter(|&id| entries(id) >= 400_000)
        .collect();
    assert!(!large.is_empty(), "T2 has no folder of 400,000 entries");
    let mut time = |app: &mut App<'_>, folder: u32| -> Duration {
        app.jump(folder);
        terminal
            .draw(|f| app.draw(f.area(), f.buffer_mut()))
            .unwrap();
        let start = Instant::now();
        assert_eq!(press(app, &mut terminal, &char('i')), Flow::Redraw);
        let took = start.elapsed();
        press(app, &mut terminal, &char('i'));
        took
    };
    let mut on_large = Vec::new();
    for &folder in &large {
        let took = time(&mut app, folder);
        let name = match folder {
            0 => "the root".to_owned(),
            _ => String::from_utf8_lossy(&index.path(folder)).into_owned(),
        };
        eprintln!(
            "i on {name} ({} entries): {} µs",
            entries(folder),
            took.as_micros()
        );
        on_large.push(took);
    }
    let mut all: Vec<Duration> = folders(&index, 200)
        .into_iter()
        .map(|folder| time(&mut app, folder))
        .collect();
    let [p50, p90, p99, max] = percentiles(&mut all);
    eprintln!(
        "i on {} folders: p50 {p50} µs, p90 {p90} µs, p99 {p99} µs, max {max} µs",
        all.len()
    );
    let worst = on_large.iter().max().unwrap();
    assert!(
        *worst < Duration::from_millis(50),
        "i on a 400k-entry folder took {worst:?}"
    );
    assert!(max < 50_000, "i took {max} µs at most");
}

/// A folder's facts, the longest list, fit the wide column at 160×45: the
/// scan's last row is drawn.
#[test]
fn p4_tui_04_the_facts_fit_the_wide_column() {
    let env = TestEnv::new();
    let tree = mockup();
    let index = saved(&env, &tree, "/Users/alex/Library");
    let (mocha, classes) = (Pack::named("mocha").unwrap(), classes());
    let opts = opts(&mocha, &classes, ColorBy::Type);
    let class_of = classes_of(&index, &classes);
    let ranks = ranks(&index, &opts);
    let dominant = std::cell::RefCell::default();
    let ctx = Context {
        view: &index,
        opts: &opts,
        class_of: &class_of,
        dominant: &dominant,
        ranks: &ranks,
        root: "~/Library",
        root_parent: "~",
    };
    let sources = Sources::of(&index);
    let mut canvas = Canvas::new(45, 42, &mocha, Glyphs::Unicode);
    // A folder's list is the longest: every one of its rows is drawn.
    let caches = id_of(&index, "Caches");
    let facts = Facts::read(&ctx, &sources, caches);
    super::expanded(
        &mut canvas,
        Rect::new(0, 0, 45, 42),
        &ctx,
        &sources,
        &facts,
        caches,
    );
    let threads = (1..41).any(|y| {
        let row: String = (0..45)
            .filter_map(|x| canvas.cells.get(x, y).map(|c| c.ch))
            .collect();
        row.contains("threads   8")
    });
    assert!(threads);
}
