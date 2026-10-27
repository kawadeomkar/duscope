//! The static treemap `scan` draws on a terminal (or with `--treemap
//! always`): the theme, the colour depth and the glyphs decided from the
//! flags and the environment, the frame painted by `duscope-treemap`.

use std::ffi::OsString;

use duscope_core::query::{self, ByTypeQuery, Filters, TreeView, timegrammar};
use duscope_core::rules::{Class, Classes};
use duscope_treemap::cells::{Color, Depth};
use duscope_treemap::packs::Pack;
use duscope_treemap::paint::{ColorBy, Footer, Glyphs, PaintOpts, frame};

use super::scan::{self, Scanned};
use super::{Exit, Failure};
use crate::cli::{ColorBy as ColorByArg, MinTile, ScanArgs, TreemapWhen};
use crate::out::{Output, emit_bytes};

/// The size assumed when stdout is not a terminal (`--treemap always`).
const DEFAULT_SIZE: (u16, u16) = (80, 24);

/// Draws the treemap of `scanned`.
pub fn draw(scanned: &Scanned, args: &ScanArgs, output: &Output) -> Result<(), Failure> {
    if args.treemap == Some(TreemapWhen::Pixels) {
        return Err(Failure::new(
            Exit::Usage,
            "not_compiled",
            "--treemap pixels needs the pixels feature, which this build does not have",
        )
        .hint("Run duscope scan with --treemap always for the cell map"));
    }
    let (columns, rows) = duscope_walk::terminal_size().unwrap_or(DEFAULT_SIZE);
    // The theme is read (and a bad one refused) even when colour is off.
    let themed = theme(args.theme.as_ref())?;
    let pack = if output.color {
        themed
    } else {
        named("no-color")
    };
    let depth = if pack.shades {
        Depth::None
    } else if pack.name == "terminal-native" {
        Depth::Ansi16
    } else if env_is("COLORTERM", &["truecolor", "24bit"]) {
        Depth::TrueColor
    } else {
        Depth::Ansi256
    };
    let classes = Classes::vendored();
    let size = scan::size_kind(&args.walk);
    let opts = PaintOpts {
        pack: &pack,
        color_by: match args.color_by {
            None | Some(ColorByArg::Type) => ColorBy::Type,
            Some(ColorByArg::Folder) => ColorBy::Folder,
            Some(ColorByArg::Level) => ColorBy::Level,
            Some(ColorByArg::Age) => ColorBy::Age,
            Some(ColorByArg::Lscolors) => ColorBy::LsColors(ls_colors()),
        },
        glyphs: if utf8_locale() {
            Glyphs::Unicode
        } else {
            Glyphs::Ascii
        },
        depth: args.depth,
        min_tile: match args.min_tile {
            None | Some(MinTile::One) => 1,
            Some(MinTile::Two) => 2,
            Some(MinTile::Four) => 4,
        },
        pool_files: args.pool_files,
        size,
        now: timegrammar::now(),
        classes: &classes,
    };
    let view = scanned.view();
    let block = scan::scan_block(scanned, &args.walk, size);
    let root = view.root_path().display().to_string();
    // A terminal without UTF-8 gets ASCII separators.
    let dot = match opts.glyphs {
        Glyphs::Unicode => "·",
        Glyphs::Ascii => "-",
    };
    let footer = Footer {
        legend: legend(&view, &classes, size),
        summary: scan::footer(&block, scanned, output).replace('·', dot),
        warning: scan::warnings(scanned)
            .first()
            .map(|w| format!("warning: {}", w.message)),
        next: format!(
            "next: duscope top {root} --depth 2 {dot} duscope by-type {root} {dot} duscope stale {root}"
        ),
    };
    let buffer = frame(&view, &opts, &footer, columns, rows);
    emit_bytes(buffer.to_ansi(depth).as_bytes())
}

/// Each class's share of the scan, largest first, `other` last.
fn legend(
    view: &(impl TreeView + ?Sized),
    classes: &Classes,
    size: query::SizeKind,
) -> Vec<(Class, f64)> {
    let page = query::by_type(
        view,
        &ByTypeQuery {
            classes,
            examples: 0,
            filters: Filters::default(),
            limit: None,
            size,
        },
    );
    page.entries
        .iter()
        .filter_map(|e| {
            Class::ALL
                .into_iter()
                .find(|c| c.as_str() == e.class)
                .map(|c| (c, e.pct_of_root))
        })
        .collect()
}

/// The pack `--theme` names, or the theme file it names; without one, the
/// light pack on a light terminal (`COLORFGBG`) and Mocha otherwise.
fn theme(arg: Option<&OsString>) -> Result<Pack, Failure> {
    let Some(arg) = arg else {
        return Ok(named(if light_background() { "light" } else { "mocha" }));
    };
    let text = arg.to_string_lossy();
    if let Some(pack) = Pack::named(&text) {
        return Ok(pack);
    }
    let usage = |message: String| {
        Failure::new(Exit::Usage, "usage", message).hint(
            "Run duscope scan --theme with mocha, tokyo-night, dracula, gruvbox, light, terminal-native, no-color or a theme file",
        )
    };
    let contents = std::fs::read_to_string(arg).map_err(|e| {
        usage(format!(
            "--theme {text}: not a theme name, and the file cannot be read: {e}"
        ))
    })?;
    Pack::from_toml(&contents, &text).map_err(|e| usage(format!("--theme {e}")))
}

fn named(name: &str) -> Pack {
    Pack::named(name).unwrap_or_else(|| Pack::named("mocha").unwrap_or_else(|| unreachable!()))
}

/// Whether `COLORFGBG` names a light background (`…;7` or `…;15`).
fn light_background() -> bool {
    std::env::var("COLORFGBG")
        .ok()
        .and_then(|v| v.rsplit(';').next().map(str::to_owned))
        .is_some_and(|bg| bg == "7" || bg == "15")
}

/// Whether variable `name` is one of `values`.
fn env_is(name: &str, values: &[&str]) -> bool {
    std::env::var(name).is_ok_and(|v| values.contains(&v.as_str()))
}

/// Whether the locale speaks UTF-8 (`LC_ALL`, then `LC_CTYPE`, then
/// `LANG`; the first one set decides).
fn utf8_locale() -> bool {
    ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .find_map(|name| std::env::var(name).ok().filter(|v| !v.is_empty()))
        .is_some_and(|v| {
            let v = v.to_ascii_lowercase();
            v.contains("utf-8") || v.contains("utf8")
        })
}

/// `LS_COLORS`' extension colours (`*.tar=01;31`): the colour of each
/// extension, from its 8, 16, 256 or 24-bit foreground code.
fn ls_colors() -> Vec<(String, Color)> {
    let Ok(spec) = std::env::var("LS_COLORS") else {
        return Vec::new();
    };
    spec.split(':')
        .filter_map(|entry| {
            let (pattern, codes) = entry.split_once('=')?;
            let ext = pattern.strip_prefix("*.")?.to_ascii_lowercase();
            Some((ext, foreground(codes)?))
        })
        .collect()
}

/// The foreground colour in an SGR code list.
fn foreground(codes: &str) -> Option<Color> {
    let codes: Vec<u16> = codes.split(';').filter_map(|c| c.parse().ok()).collect();
    let mut i = 0;
    let mut color = None;
    while i < codes.len() {
        match codes[i] {
            n @ 30..=37 => color = u8::try_from(n - 30).ok().map(Color::Ansi),
            n @ 90..=97 => color = u8::try_from(n - 90 + 8).ok().map(Color::Ansi),
            38 if codes.get(i + 1) == Some(&5) => {
                color = codes
                    .get(i + 2)
                    .and_then(|&n| u8::try_from(n).ok())
                    .map(from_256);
                i += 2;
            }
            38 if codes.get(i + 1) == Some(&2) => {
                let c = |k: usize| codes.get(i + k).and_then(|&v| u8::try_from(v).ok());
                color = Some(Color::Rgb(c(2)?, c(3)?, c(4)?));
                i += 4;
            }
            _ => {}
        }
        i += 1;
    }
    color
}

/// One of the 256 xterm colours as a colour.
fn from_256(n: u8) -> Color {
    match n {
        0..=15 => Color::Ansi(n),
        16..=231 => {
            const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
            let i = usize::from(n - 16);
            Color::Rgb(LEVELS[i / 36], LEVELS[i / 6 % 6], LEVELS[i % 6])
        }
        _ => {
            let grey = 8 + 10 * (n - 232);
            Color::Rgb(grey, grey, grey)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{foreground, from_256};
    use duscope_treemap::cells::Color;

    #[test]
    fn p1_map_02_ls_colors_codes_become_colours() {
        assert_eq!(foreground("01;31"), Some(Color::Ansi(1)));
        assert_eq!(foreground("1;94"), Some(Color::Ansi(12)));
        assert_eq!(foreground("38;5;196"), Some(Color::Rgb(255, 0, 0)));
        assert_eq!(foreground("38;2;1;2;3"), Some(Color::Rgb(1, 2, 3)));
        assert_eq!(foreground("01"), None);
        assert_eq!(from_256(244), Color::Rgb(128, 128, 128));
    }
}
