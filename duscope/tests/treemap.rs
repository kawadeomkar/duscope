//! P1-MAP-02: the static treemap: buffer snapshots on the fixture tree at
//! 80×24 and 120×40 in Mocha, `no-color` and ASCII; and from the command
//! line, no SGR under `NO_COLOR`, the pixels refusal and themes.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::borrow::Cow;
use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use duscope_core::arena::Arena;
use duscope_core::query::{
    ArenaView, ByTypeQuery, Filters, Node, SizeKind, SortKey, TreeView, by_type,
};
use duscope_core::rollup::roll_up;
use duscope_core::rules::{Class, Classes};
use duscope_testkit::{TestEnv, golden};
use duscope_treemap::cells::Color;
use duscope_treemap::packs::Pack;
use duscope_treemap::paint::{ColorBy, Footer, Glyphs, LsColors, PaintOpts, frame};
use duscope_walk::{Attrs, Flags, Kind, WalkOptions, walk};

fn duscope(env: &TestEnv) -> Command {
    Command::from_std(env.cmd())
}

#[test]
fn p1_map_02_no_color_output_contains_no_sgr() {
    let env = TestEnv::new();
    let home = env.fixtures().path("home");
    let out = duscope(&env)
        .args(["scan", "--treemap", "always", "--threads", "4"])
        .arg(&home)
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(!text.contains('\x1b'), "an escape under NO_COLOR");
    // 80×24 assumed off a terminal: the map, legend, summary, warning and
    // next lines, the last row left for the prompt.
    assert_eq!(text.lines().count(), 23);
    assert!(text.contains("┌─ Library · "), "{text}");
    // With colour asked for, the map is drawn in 256 colours (no
    // COLORTERM) and the same layout.
    let out = duscope(&env)
        .args([
            "scan",
            "--treemap",
            "always",
            "--color",
            "always",
            "--threads",
            "4",
        ])
        .arg(&home)
        .output()
        .unwrap();
    let colored = String::from_utf8(out.stdout).unwrap();
    assert!(colored.contains("\x1b[0;38;5;"), "no 256-colour escapes");
    assert_eq!(colored.lines().count(), 23);
}

#[test]
fn p1_map_02_pixels_and_bad_themes_are_refused() {
    let env = TestEnv::new();
    let tree = env.fixtures().case_root("empty");
    let out = duscope(&env)
        .args(["scan", "--treemap", "pixels"])
        .arg(&tree)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--treemap pixels needs the pixels feature"),
        "{stderr}"
    );
    // --json wins over --treemap: the envelope, no map to refuse.
    duscope(&env)
        .args(["scan", "--treemap", "pixels", "--json"])
        .arg(&tree)
        .assert()
        .success();
    duscope(&env)
        .args(["scan", "--treemap", "always", "--theme", "solarized"])
        .arg(&tree)
        .assert()
        .code(2);
    // A theme file in the documented format is taken; DUSCOPE_THEME names
    // one too.
    let theme = env.tmp_dir().join("theme.toml");
    let doc = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../docs/rules.md")).unwrap();
    let from = doc.find("```toml theme\n").unwrap() + "```toml theme\n".len();
    let to = from + doc[from..].find("```").unwrap();
    fs::write(&theme, &doc[from..to]).unwrap();
    for (flag, var) in [(true, false), (false, true)] {
        let mut cmd = duscope(&env);
        cmd.args(["scan", "--treemap", "always", "--color", "always"])
            .arg(&tree);
        if flag {
            cmd.arg("--theme").arg(&theme);
        }
        if var {
            cmd.env("DUSCOPE_THEME", &theme);
        }
        cmd.assert().success();
    }
}

#[test]
fn p1_map_02_refused_map_flags_are_refused_before_the_scan() {
    let env = TestEnv::new();
    // On a path that does not exist the usage error wins over
    // scan_not_found: nothing was scanned.
    let missing = env.tmp_dir().join("does-not-exist");
    let out = duscope(&env)
        .args(["scan", "--treemap", "pixels"])
        .arg(&missing)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("needs the pixels feature"), "{stderr}");
    assert!(
        stderr.contains("cargo install duscope --features pixels"),
        "{stderr}"
    );
    let out = duscope(&env)
        .args([
            "scan",
            "--treemap",
            "always",
            "--theme",
            "solarized",
            "--json",
        ])
        .arg(&missing)
        .output()
        .unwrap();
    // --json wins over the map: no theme to read, the path is missing.
    assert_eq!(out.status.code(), Some(3));
    let out = duscope(&env)
        .args(["scan", "--treemap", "always", "--theme", "solarized"])
        .arg(&missing)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--theme solarized"), "{stderr}");
}

#[test]
fn p1_map_02_buffer_snapshots_on_the_fixture_tree() {
    let env = TestEnv::new();
    let root = env.fixtures().path("home");
    let mut arena = Arena::new(Attrs::Basic);
    let opts = WalkOptions {
        threads: 4,
        ..WalkOptions::default()
    };
    walk(&root, &opts, |event| arena.push(event).unwrap()).unwrap();
    let rollup = roll_up(&mut arena);
    let view = ArenaView::new(&arena, &rollup, root.clone());
    let classes = Classes::vendored();
    let legend: Vec<(Class, f64)> = by_type(
        &view,
        &ByTypeQuery {
            classes: &classes,
            examples: 0,
            filters: Filters::default(),
            limit: None,
            size: SizeKind::Allocated,
            sort: SortKey::Size,
            reverse: false,
        },
    )
    .entries
    .iter()
    .map(|e| {
        (
            Class::ALL
                .into_iter()
                .find(|c| c.as_str() == e.class)
                .unwrap(),
            e.pct_of_root,
        )
    })
    .collect();
    let footer = Footer {
        legend,
        summary: "<summary>".to_owned(),
        warning: None,
        next: "next: duscope top --depth 2".to_owned(),
    };
    let os = std::env::consts::OS;
    for (name, pack, glyphs) in [
        ("mocha", "mocha", Glyphs::Unicode),
        ("no_color", "no-color", Glyphs::Unicode),
        ("ascii", "mocha", Glyphs::Ascii),
    ] {
        let pack = Pack::named(pack).unwrap();
        let opts = PaintOpts {
            pack: &pack,
            color_by: ColorBy::Type,
            glyphs,
            depth: None,
            min_tile: 1,
            pool_files: false,
            size: SizeKind::Allocated,
            now: 0,
            classes: &classes,
        };
        for (w, h) in [(80u16, 24u16), (120, 40)] {
            let buffer = frame(&view, &opts, &footer, w, h);
            assert_eq!((buffer.width(), buffer.height()), (w, h - 1));
            if glyphs == Glyphs::Ascii {
                assert!(buffer.snapshot().is_ascii(), "{name} {w}x{h}");
            }
            golden(
                &format!("scan_treemap_{name}_{w}x{h}@{os}.snap"),
                &buffer.snapshot(),
            );
        }
    }
}

#[test]
fn p1_map_02_buffer_snapshots_of_the_map_flags() {
    let env = TestEnv::new();
    let root = env.fixtures().path("home");
    let mut arena = Arena::new(Attrs::Basic);
    let walk_opts = WalkOptions {
        threads: 4,
        ..WalkOptions::default()
    };
    walk(&root, &walk_opts, |event| arena.push(event).unwrap()).unwrap();
    let rollup = roll_up(&mut arena);
    let view = ArenaView::new(&arena, &rollup, root.clone());
    let classes = Classes::vendored();
    let mocha = Pack::named("mocha").unwrap();
    let footer = Footer {
        summary: "<summary>".to_owned(),
        next: "<next>".to_owned(),
        ..Footer::default()
    };
    let base = PaintOpts {
        size: SizeKind::Allocated,
        ..opts(&mocha, &classes, Glyphs::Unicode)
    };
    let os = std::env::consts::OS;
    for (name, opts) in [
        (
            "min_tile_4",
            PaintOpts {
                min_tile: 4,
                ..base.clone()
            },
        ),
        (
            "pool_files",
            PaintOpts {
                pool_files: true,
                ..base.clone()
            },
        ),
        (
            "depth_1",
            PaintOpts {
                depth: Some(1),
                ..base.clone()
            },
        ),
    ] {
        let buffer = frame(&view, &opts, &footer, 80, 24);
        golden(
            &format!("scan_treemap_{name}_80x24@{os}.snap"),
            &buffer.snapshot(),
        );
    }
    // Each flag changes the picture.
    let plain = frame(&view, &base, &footer, 80, 24).snapshot();
    for name in ["min_tile_4", "pool_files", "depth_1"] {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/golden")
            .join(format!("scan_treemap_{name}_80x24@{os}.snap"));
        assert_ne!(fs::read_to_string(path).unwrap(), plain, "{name}");
    }
}

/// The arena and roll-up of a walk of `root`.
fn scan_tree(root: &Path) -> (Arena, duscope_core::rollup::RollUp) {
    let mut arena = Arena::new(Attrs::Basic);
    walk(root, &WalkOptions::default(), |event| {
        arena.push(event).unwrap();
    })
    .unwrap();
    let rollup = roll_up(&mut arena);
    (arena, rollup)
}

/// The columns `line` takes, for the characters the tests below use:
/// combining accents take none, CJK characters two.
fn columns(line: &str) -> usize {
    line.chars()
        .map(|c| match u32::from(c) {
            0x0300..=0x036F => 0,
            0x3000..=0x9FFF => 2,
            _ => 1,
        })
        .sum()
}

fn opts<'a>(pack: &'a Pack, classes: &'a Classes, glyphs: Glyphs) -> PaintOpts<'a> {
    PaintOpts {
        pack,
        color_by: ColorBy::Type,
        glyphs,
        depth: None,
        min_tile: 1,
        pool_files: false,
        size: SizeKind::Apparent,
        now: 0,
        classes,
    }
}

#[test]
fn p1_map_02_names_cannot_escape_or_misalign_the_map() {
    let env = TestEnv::new();
    let root = env.tmp_dir().join("names");
    let mib = 1u64 << 20;
    for (name, size) in [
        ("evil\x1b]0;PWNED\x07.txt", 4 * mib),
        ("line\nbreak.txt", 3 * mib),
        ("写真フォルダの名前.jpg", 3 * mib),
        ("Beyonce\u{301} re\u{301}sume\u{301}.mp3", 2 * mib),
        ("写真/a.jpg", 2 * mib),
    ] {
        let path = root.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::File::create(&path).unwrap().set_len(size).unwrap();
    }
    let (arena, rollup) = scan_tree(&root);
    let view = ArenaView::new(&arena, &rollup, root.clone());
    let classes = Classes::vendored();
    let footer = Footer {
        summary: "<summary>".to_owned(),
        next: "next".to_owned(),
        ..Footer::default()
    };
    let mocha = Pack::named("mocha").unwrap();
    let buffer = frame(
        &view,
        &opts(&mocha, &classes, Glyphs::Unicode),
        &footer,
        70,
        14,
    );
    let plain = buffer.to_ansi(duscope_treemap::cells::Depth::None);
    assert!(!plain.contains(['\x1b', '\x07']), "{plain}");
    assert!(plain.contains("\\x1b"), "the escape is shown: {plain}");
    // Every row is the terminal's width: no row broken by a newline, none
    // pushed past the edge by wide characters or cut short by accents.
    assert_eq!(plain.lines().count(), 13);
    for line in plain.lines() {
        assert_eq!(columns(line), 70, "{line:?}");
    }
    // In colour, the only escapes are SGR.
    let colored = buffer.to_ansi(duscope_treemap::cells::Depth::TrueColor);
    assert!(!colored.contains("\x1b]") && !colored.contains('\x07'));
    // ASCII glyphs keep the names to ASCII too.
    let ascii = frame(
        &view,
        &opts(&mocha, &classes, Glyphs::Ascii),
        &footer,
        70,
        14,
    );
    assert!(
        ascii
            .to_ansi(duscope_treemap::cells::Depth::None)
            .is_ascii()
    );
}

#[test]
fn p1_map_02_a_rule_above_the_root_colours_the_map_as_the_legend() {
    let env = TestEnv::new();
    let root = env.tmp_dir().join("w/node_modules/pkg");
    fs::create_dir_all(&root).unwrap();
    fs::File::create(root.join("logo.png"))
        .unwrap()
        .set_len(1 << 20)
        .unwrap();
    let (arena, rollup) = scan_tree(&root);
    let view = ArenaView::new(&arena, &rollup, root.clone());
    let classes = Classes::vendored();
    let mocha = Pack::named("mocha").unwrap();
    // One tile, the png's: code, as node_modules makes it in by-type.
    let buffer =
        duscope_treemap::paint::paint(&view, &opts(&mocha, &classes, Glyphs::Unicode), 20, 5);
    assert_eq!(buffer.get(0, 4).unwrap().bg, mocha.class(Class::Code));
}

#[test]
fn p1_map_02_a_terminal_taller_than_the_units_hold_is_capped() {
    let env = TestEnv::new();
    let root = env.fixtures().case_root("empty");
    let (arena, rollup) = scan_tree(&root);
    let view = ArenaView::new(&arena, &rollup, root.clone());
    let classes = Classes::vendored();
    let mocha = Pack::named("mocha").unwrap();
    let opts = opts(&mocha, &classes, Glyphs::Unicode);
    let buffer = duscope_treemap::paint::paint(&view, &opts, 1, u16::MAX);
    assert_eq!(buffer.height(), u16::MAX / 2);
}

#[test]
fn p1_map_02_ls_colors_paint_executables_and_suffixes_but_not_folders() {
    use std::os::unix::fs::PermissionsExt;

    let env = TestEnv::new();
    let root = env.tmp_dir().join("ls");
    fs::create_dir_all(root.join("x.png")).unwrap();
    let mib = 1u64 << 20;
    for (name, size) in [
        ("x.png/data.bin", 8 * mib),
        ("tool", 4 * mib),
        ("pic.png", 2 * mib),
    ] {
        fs::File::create(root.join(name))
            .unwrap()
            .set_len(size)
            .unwrap();
    }
    fs::set_permissions(root.join("tool"), fs::Permissions::from_mode(0o755)).unwrap();
    // Modes are collected with --attrs full.
    let mut arena = Arena::new(Attrs::Full);
    let walk_opts = WalkOptions {
        attrs: Attrs::Full,
        ..WalkOptions::default()
    };
    walk(&root, &walk_opts, |event| arena.push(event).unwrap()).unwrap();
    let rollup = roll_up(&mut arena);
    let view = ArenaView::new(&arena, &rollup, root.clone());
    let classes = Classes::vendored();
    let mocha = Pack::named("mocha").unwrap();
    let (green, red) = (Color::Ansi(2), Color::Ansi(1));
    let opts = PaintOpts {
        color_by: ColorBy::LsColors(LsColors {
            executable: Some(green),
            suffixes: vec![("png".to_owned(), red)],
        }),
        depth: Some(1),
        ..opts(&mocha, &classes, Glyphs::Unicode)
    };
    let buffer = duscope_treemap::paint::paint(&view, &opts, 40, 10);
    let cells: Vec<Color> = (0..10)
        .flat_map(|y| (0..40).map(move |x| (x, y)))
        .map(|(x, y)| buffer.get(x, y).unwrap().bg)
        .collect();
    assert!(cells.contains(&green), "the executable takes ex=");
    assert!(cells.contains(&red), "pic.png takes *png=");
    // x.png/, the largest tile on the left, is a folder: the pack's colour.
    let folder = buffer.get(2, 8).unwrap().bg;
    assert!(matches!(folder, Color::Rgb(..)), "{folder:?}");
}

#[test]
fn p1_map_02_theme_help_names_every_pack() {
    let env = TestEnv::new();
    let out = duscope(&env).args(["scan", "--help"]).output().unwrap();
    let help = String::from_utf8(out.stdout).unwrap();
    let names = duscope_treemap::packs::NAMES.join("|");
    assert!(help.contains(&names), "{names} not in:\n{help}");
}

#[test]
fn p1_map_02_below_60x12_only_the_summary_is_drawn() {
    let env = TestEnv::new();
    let root = env.fixtures().path("home");
    let (arena, rollup) = scan_tree(&root);
    let view = ArenaView::new(&arena, &rollup, root.clone());
    let classes = Classes::vendored();
    let mocha = Pack::named("mocha").unwrap();
    let footer = Footer {
        summary: "<summary>".to_owned(),
        next: "<next>".to_owned(),
        ..Footer::default()
    };
    let opts = opts(&mocha, &classes, Glyphs::Unicode);
    for (w, h) in [(59u16, 12u16), (60, 11), (20, 5)] {
        let buffer = frame(&view, &opts, &footer, w, h);
        assert_eq!((buffer.width(), buffer.height()), (w, 1), "{w}x{h}");
        let text = buffer.to_ansi(duscope_treemap::cells::Depth::None);
        assert_eq!(text.trim_end(), "<summary>", "{w}x{h}");
    }
    // From 60×12 the map takes rows − 5 and the four footer rows follow,
    // leaving the prompt's row.
    let buffer = frame(&view, &opts, &footer, 60, 12);
    assert_eq!((buffer.width(), buffer.height()), (60, 11));
    let text = buffer.to_ansi(duscope_treemap::cells::Depth::None);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[8].trim_end(), "<summary>");
    assert_eq!(lines[10].trim_end(), "<next>");
}

#[test]
fn p1_map_02_color_by_folder_level_and_age_use_the_cycle_and_the_ramp() {
    use std::time::{Duration, SystemTime};

    let env = TestEnv::new();
    let root = env.tmp_dir().join("colours");
    fs::create_dir_all(root.join("a")).unwrap();
    let mib = 1u64 << 20;
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(2_000_000_000);
    let day = Duration::from_secs(86_400);
    // a/f is two days old, g four hundred.
    for (name, size, age) in [("a/f", 8 * mib, 2 * day), ("g", 4 * mib, 400 * day)] {
        let file = fs::File::create(root.join(name)).unwrap();
        file.set_len(size).unwrap();
        file.set_modified(now - age).unwrap();
    }
    let (arena, rollup) = scan_tree(&root);
    let view = ArenaView::new(&arena, &rollup, root.clone());
    let classes = Classes::vendored();
    let mocha = Pack::named("mocha").unwrap();
    let base = mocha.roles.base;
    let now_ns = 2_000_000_000 * 1_000_000_000;
    // The colours of a 40×10 map, with or without the sibling tint.
    let painted = |color_by: ColorBy| {
        let opts = PaintOpts {
            color_by,
            now: now_ns,
            ..opts(&mocha, &classes, Glyphs::Unicode)
        };
        let buffer = duscope_treemap::paint::paint(&view, &opts, 40, 10);
        let cells: Vec<Color> = (0..10)
            .flat_map(|y| (0..40).map(move |x| (x, y)))
            .flat_map(|(x, y)| {
                let cell = buffer.get(x, y).unwrap();
                [cell.fg, cell.bg]
            })
            .collect();
        move |color: Color| cells.contains(&color) || cells.contains(&color.mix(base, 20))
    };
    // Folder: a's files take the first top-level folder's colour, g the
    // second's.
    let has = painted(ColorBy::Folder);
    assert!(has(mocha.cycle[0]) && has(mocha.cycle[1]));
    // Level: g at depth 1, a/f at depth 2.
    let has = painted(ColorBy::Level);
    assert!(has(mocha.cycle[1]) && has(mocha.cycle[2]));
    assert!(!has(mocha.cycle[0]));
    // Age: a week old at most, and older than a year.
    let has = painted(ColorBy::Age);
    assert!(has(mocha.age[1]) && has(mocha.age[4]));
    assert!(!has(mocha.age[0]) && !has(mocha.age[2]) && !has(mocha.age[3]));
}

/// The proposal's mockup tree (`tests/fixtures/mockup.toml`) as a view:
/// the root, then each tile as a directory holding one file of its class.
struct MockupView {
    root: PathBuf,
    /// Each entry in pre-order: name, whether a directory, size.
    entries: Vec<(Vec<u8>, bool, u64)>,
}

impl TreeView for MockupView {
    fn len(&self) -> u32 {
        u32::try_from(self.entries.len()).unwrap()
    }

    fn node(&self, id: u32) -> Node {
        let (_, dir, size) = &self.entries[id as usize];
        let last = self.len() - 1;
        // The root (0), the tiles' directories (odd) and their files (even).
        let (parent, depth, last_id) = match id {
            0 => (0, 0, last),
            _ if id % 2 == 1 => (0, 1, id + 1),
            _ => (id - 1, 2, id),
        };
        Node {
            id,
            parent,
            depth,
            kind: if *dir { Kind::Dir } else { Kind::File },
            allocated: *size,
            apparent: *size,
            file_count: if id == 0 { u64::from(last / 2) } else { 1 },
            dir_count: u64::from(*dir),
            mtime_ns: 0,
            max_mtime_ns: 0,
            ctime_ns: 0,
            atime_ns: None,
            btime_ns: None,
            mode: None,
            nlink: 1,
            charged: true,
            linked_away: false,
            flags: Flags::empty(),
            upper_bound: false,
            unreadable: false,
            files_end: if id == 0 { 0 } else { last_id },
            last_id,
        }
    }

    fn root_path(&self) -> &Path {
        &self.root
    }

    fn full_attrs(&self) -> bool {
        false
    }

    fn name(&self, id: u32) -> Cow<'_, [u8]> {
        Cow::Borrowed(&self.entries[id as usize].0)
    }
}

#[derive(serde::Deserialize)]
struct Mockup {
    root: PathBuf,
    canvas: MockCanvas,
    tile: Vec<MockTile>,
}

#[derive(serde::Deserialize)]
struct MockCanvas {
    columns: u16,
    rows: u16,
}

#[derive(serde::Deserialize)]
struct MockTile {
    name: String,
    bytes: u64,
    class: String,
    rect: [u16; 4],
}

#[test]
fn p1_map_02_depth_1_paints_the_mockups_eleven_titled_boxes() {
    let text = fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../tests/fixtures/mockup.toml"
    ))
    .unwrap();
    let mockup: Mockup = toml::from_str(&text).unwrap();
    // A file of each tile's class: an extension the classes table gives
    // it, or none for other.
    let file = |class: &str| -> &[u8] {
        match class {
            "code" => b"main.rs",
            "media" => b"clip.mp4",
            "archives" => b"a.zip",
            "data" => b"store.db",
            "binaries" => b"lib.o",
            "docs" => b"notes.pdf",
            _ => b"blob",
        }
    };
    let total = mockup.tile.iter().map(|t| t.bytes).sum();
    let mut entries = vec![(b"alex".to_vec(), true, total)];
    for tile in &mockup.tile {
        entries.push((tile.name.as_bytes().to_vec(), true, tile.bytes));
        entries.push((file(&tile.class).to_vec(), false, tile.bytes));
    }
    let view = MockupView {
        root: mockup.root.clone(),
        entries,
    };
    let classes = Classes::vendored();
    let mocha = Pack::named("mocha").unwrap();
    let opts = PaintOpts {
        depth: Some(1),
        size: SizeKind::Allocated,
        ..opts(&mocha, &classes, Glyphs::Unicode)
    };
    let (w, h) = (mockup.canvas.columns, mockup.canvas.rows);
    let buffer = duscope_treemap::paint::paint(&view, &opts, w, h);
    let ch = |x: u16, y: u16| buffer.get(x, y).unwrap().ch;
    assert_eq!(mockup.tile.len(), 11);
    for (order, tile) in mockup.tile.iter().enumerate() {
        let [x, y, tw, th] = tile.rect;
        // The box's whole cell rows inside its half-cell rectangle.
        let (top, bottom) = (y.div_ceil(2), y.midpoint(th) - 1);
        let (right, name) = (x + tw - 1, &tile.name);
        assert_eq!(
            [ch(x, top), ch(right, top), ch(x, bottom), ch(right, bottom)],
            ['┌', '┐', '└', '┘'],
            "{name}"
        );
        // The title on the top border: ` name · size `, or the name cut.
        let title = buffer.get(x + 3, top).unwrap();
        assert_eq!(Some(title.ch), name.chars().next(), "{name}");
        assert!(title.bold, "{name}");
        // Filled with its class's colour, every second box tinted.
        let class = Class::ALL
            .into_iter()
            .find(|c| c.as_str() == tile.class)
            .unwrap();
        let mut inside = mocha.class(class);
        if order % 2 == 1 {
            inside = inside.mix(mocha.roles.base, 20);
        }
        assert_eq!(buffer.get(x + 1, top + 1).unwrap().bg, inside, "{name}");
    }
    // Where ` name · size ` is wider than the box less 6, the name alone.
    let row: String = (0..w).map(|x| ch(x, 0)).collect();
    assert!(row.contains("┌─ dev/target ──"), "{row}");
    assert!(row.contains("┌─ Movies ──"), "{row}");
    // Twice as wide, the whole title fits.
    let wide = duscope_treemap::paint::paint(&view, &opts, 2 * w, h);
    let row: String = (0..2 * w).map(|x| wide.get(x, 0).unwrap().ch).collect();
    assert!(row.contains("┌─ dev/target · 61G ─"), "{row}");
    // --depth 0 draws as --depth 1.
    let zero = PaintOpts {
        depth: Some(0),
        ..opts.clone()
    };
    assert_eq!(duscope_treemap::paint::paint(&view, &zero, w, h), buffer);
}
