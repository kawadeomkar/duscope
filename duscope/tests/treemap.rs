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
