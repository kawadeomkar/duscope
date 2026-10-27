//! P1-MAP-02: the static treemap: buffer snapshots on the fixture tree at
//! 80×24 and 120×40 in Mocha, `no-color` and ASCII; and from the command
//! line, no SGR under `NO_COLOR`, the pixels refusal and themes.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use duscope_core::arena::Arena;
use duscope_core::query::{ArenaView, ByTypeQuery, Filters, SizeKind, by_type};
use duscope_core::rollup::roll_up;
use duscope_core::rules::{Class, Classes};
use duscope_testkit::TestEnv;
use duscope_treemap::packs::Pack;
use duscope_treemap::paint::{ColorBy, Footer, Glyphs, PaintOpts, frame};
use duscope_walk::{Attrs, WalkOptions, walk};

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

/// The golden file `name` under `duscope/tests/golden/`, compared with
/// `actual`; with `DUSCOPE_TEST_UPDATE_GOLDEN=1` it is rewritten instead.
fn golden(name: &str, actual: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name);
    if std::env::var_os("DUSCOPE_TEST_UPDATE_GOLDEN").is_some_and(|v| v == "1") {
        fs::write(&path, actual).unwrap();
        return;
    }
    let expected = fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert_eq!(
        actual, expected,
        "golden {name} differs; rerun with DUSCOPE_TEST_UPDATE_GOLDEN=1 to regenerate"
    );
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
