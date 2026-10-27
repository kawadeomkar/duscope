//! The static treemap `scan` draws on a terminal (or with `--treemap
//! always`): the theme, the colour depth and the glyphs decided from the
//! flags and the environment, the frame painted by `duscope-treemap`.

use std::ffi::OsString;

use duscope_core::envelope::Warning;
use duscope_core::query::{self, ByTypeQuery, Filters, TreeView, timegrammar};
use duscope_core::rules::{Class, Classes};
use duscope_treemap::cells::{Color, Depth};
use duscope_treemap::packs::Pack;
use duscope_treemap::paint::{ColorBy, Footer, Glyphs, LsColors, PaintOpts, frame};

use super::scan::{self, Scanned};
use super::{Exit, Failure};
use crate::cli::{ColorBy as ColorByArg, MinTile, ScanArgs, TreemapWhen};
use crate::out::{Output, emit_bytes, stderr_line};

/// The size assumed when stdout is not a terminal (`--treemap always`).
const DEFAULT_SIZE: (u16, u16) = (80, 24);

/// How the map looks: settled from the flags and the environment before
/// the scan, so a flag the map refuses is refused without scanning.
pub struct Style {
    pack: Pack,
    depth: Depth,
    glyphs: Glyphs,
    /// `LS_COLORS`, read when `--color-by lscolors` asks for it.
    ls_colors: LsColors,
}

/// The variables the map's look depends on, read once, so the decisions
/// can be tested with any values.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct MapEnv {
    /// `COLORTERM`.
    colorterm: Option<String>,
    /// `COLORFGBG`.
    colorfgbg: Option<String>,
    /// The first of `LC_ALL`, `LC_CTYPE` and `LANG` that is set and not
    /// empty.
    locale: Option<String>,
    /// `LS_COLORS`.
    ls_colors: Option<String>,
}

impl MapEnv {
    /// Reads the variables through `get` (`std::env::var_os` in the
    /// binary).
    fn read(get: impl Fn(&str) -> Option<OsString>) -> MapEnv {
        let value = |name: &str| get(name).map(|v| v.to_string_lossy().into_owned());
        MapEnv {
            colorterm: value("COLORTERM"),
            colorfgbg: value("COLORFGBG"),
            locale: ["LC_ALL", "LC_CTYPE", "LANG"]
                .iter()
                .find_map(|name| value(name).filter(|v| !v.is_empty())),
            ls_colors: value("LS_COLORS"),
        }
    }

    /// Whether `COLORFGBG` names a light background (`…;7` or `…;15`).
    fn light_background(&self) -> bool {
        self.colorfgbg
            .as_deref()
            .and_then(|v| v.rsplit(';').next())
            .is_some_and(|bg| bg == "7" || bg == "15")
    }

    /// Whether `COLORTERM` promises 24-bit colour.
    fn truecolor(&self) -> bool {
        matches!(self.colorterm.as_deref(), Some("truecolor" | "24bit"))
    }

    /// Whether the locale speaks UTF-8.
    fn utf8(&self) -> bool {
        self.locale.as_deref().is_some_and(|v| {
            let v = v.to_ascii_lowercase();
            v.contains("utf-8") || v.contains("utf8")
        })
    }
}

/// The map's style for `args`: refuses `--treemap pixels` (`not_compiled`)
/// and a theme that is neither a pack nor a readable theme file (`usage`).
pub fn style(args: &ScanArgs, output: &Output) -> Result<Style, Failure> {
    if args.treemap == Some(TreemapWhen::Pixels) {
        return Err(Failure::new(
            Exit::Usage,
            "not_compiled",
            "--treemap pixels needs the pixels feature, which this build does not have",
        )
        .hint(
            "Install with cargo install duscope --features pixels, or run duscope scan --treemap always for the cell map",
        ));
    }
    let env = MapEnv::read(|name| std::env::var_os(name));
    decide(args.theme.as_ref(), output.color, &env)
}

/// The style from `--theme`, whether what people read is coloured, and
/// the environment: the theme is read (and a bad one refused) even when
/// colour is off, which draws with `no-color`; the colour depth follows
/// the pack and `COLORTERM`, the glyphs the locale.
fn decide(theme_arg: Option<&OsString>, color: bool, env: &MapEnv) -> Result<Style, Failure> {
    let themed = theme(theme_arg, env)?;
    let pack = if color { themed } else { named("no-color") };
    let depth = if pack.shades {
        Depth::None
    } else if pack.name == "terminal-native" {
        Depth::Ansi16
    } else if env.truecolor() {
        Depth::TrueColor
    } else {
        Depth::Ansi256
    };
    Ok(Style {
        pack,
        depth,
        glyphs: if env.utf8() {
            Glyphs::Unicode
        } else {
            Glyphs::Ascii
        },
        ls_colors: ls_colors(env.ls_colors.as_deref().unwrap_or_default()),
    })
}

/// Draws the treemap of `scanned` in `style`.
pub fn draw(
    scanned: &Scanned,
    args: &ScanArgs,
    output: &Output,
    style: &Style,
) -> Result<(), Failure> {
    let (columns, rows) = duscope_walk::terminal_size().unwrap_or(DEFAULT_SIZE);
    let Style {
        pack,
        depth,
        glyphs,
        ls_colors,
    } = style;
    let classes = Classes::vendored();
    let size = scan::size_kind(&args.walk);
    let opts = PaintOpts {
        pack,
        color_by: match args.color_by {
            None | Some(ColorByArg::Type) => ColorBy::Type,
            Some(ColorByArg::Folder) => ColorBy::Folder,
            Some(ColorByArg::Level) => ColorBy::Level,
            Some(ColorByArg::Age) => ColorBy::Age,
            Some(ColorByArg::Lscolors) => ColorBy::LsColors(ls_colors.clone()),
        },
        glyphs: *glyphs,
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
    // The next commands name the tree as it was given, unless it is the
    // current directory.
    let here = std::env::current_dir()
        .and_then(std::fs::canonicalize)
        .is_ok_and(|cwd| cwd == scanned.root);
    let root = (!here).then(|| scanned.root_as_given.to_string_lossy().into_owned());
    // A terminal without UTF-8 gets ASCII separators.
    let dot = match opts.glyphs {
        Glyphs::Unicode => "·",
        Glyphs::Ascii => "-",
    };
    let warnings = scan::warnings(scanned);
    let footer = Footer {
        legend: legend(&view, &classes, size),
        summary: scan::footer(&block, scanned, output).replace('·', dot),
        warning: frame_warning(&warnings),
        next: next_line(root.as_deref(), dot, columns),
    };
    let buffer = frame(&view, &opts, &footer, columns, rows);
    emit_bytes(buffer.to_ansi(*depth).as_bytes())?;
    // The frame has room for one warning; the others follow on stderr, as
    // every other format writes them.
    for warning in warnings.iter().skip(1) {
        stderr_line(&format!("warning: {}", warning.message));
        if let Some(hint) = &warning.hint {
            stderr_line(&format!("hint: {hint}"));
        }
    }
    Ok(())
}

/// The frame's warning row: the first warning, and how many more follow
/// on stderr.
fn frame_warning(warnings: &[Warning]) -> Option<String> {
    let first = warnings.first()?;
    Some(match warnings.len() - 1 {
        0 => format!("warning: {}", first.message),
        more => format!("warning: {} (+{more} more below)", first.message),
    })
}

/// The `next` line: `top`, `by-type` and `stale` of the same tree, each
/// with `root` (shell-quoted) when the tree is not the current directory;
/// the ones after the first that do not fit in `width` columns are left
/// out rather than cut.
fn next_line(root: Option<&str>, dot: &str, width: u16) -> String {
    let arg = root
        .map(|r| format!(" {}", shell_quote(r)))
        .unwrap_or_default();
    let commands = [
        format!("duscope top --depth 2{arg}"),
        format!("duscope by-type{arg}"),
        format!("duscope stale{arg}"),
    ];
    let mut line = format!("next: {}", commands[0]);
    for command in &commands[1..] {
        let more = format!(" {dot} {command}");
        if line.chars().count() + more.chars().count() > usize::from(width) {
            break;
        }
        line.push_str(&more);
    }
    line
}

/// `text` as one word for a POSIX shell: as it is when it holds only
/// characters no shell treats specially, single-quoted otherwise.
fn shell_quote(text: &str) -> String {
    let plain = !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "@%+=:,./_-".contains(c));
    if plain {
        text.to_owned()
    } else {
        format!("'{}'", text.replace('\'', "'\\''"))
    }
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
fn theme(arg: Option<&OsString>, env: &MapEnv) -> Result<Pack, Failure> {
    let Some(arg) = arg else {
        return Ok(named(if env.light_background() {
            "light"
        } else {
            "mocha"
        }));
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
    Pack::named(name).unwrap_or_else(Pack::mocha)
}

/// What `--color-by lscolors` reads of `LS_COLORS`: `ex=` and every
/// `*suffix=` entry with a foreground colour (8, 16, 256 or 24-bit). The
/// other keys (`di=`, `ln=`, `fi=`, …) are left to the pack.
fn ls_colors(spec: &str) -> LsColors {
    let mut table = LsColors::default();
    for entry in spec.split(':') {
        let Some((pattern, codes)) = entry.split_once('=') else {
            continue;
        };
        let Some(color) = foreground(codes) else {
            continue;
        };
        if pattern == "ex" {
            table.executable = Some(color);
        } else if let Some(suffix) = pattern.strip_prefix('*').filter(|s| !s.is_empty()) {
            table.suffixes.push((suffix.to_ascii_lowercase(), color));
        }
    }
    table
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
    use std::ffi::OsString;

    use super::{
        MapEnv, Warning, decide, foreground, frame_warning, from_256, ls_colors, next_line,
        shell_quote,
    };
    use duscope_treemap::cells::{Color, Depth};
    use duscope_treemap::paint::Glyphs;

    /// The environment of `vars`, as the binary would read it.
    fn env(vars: &[(&str, &str)]) -> MapEnv {
        MapEnv::read(|name| {
            vars.iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| OsString::from(v))
        })
    }

    #[test]
    fn p1_map_02_ls_colors_codes_become_colours() {
        assert_eq!(foreground("01;31"), Some(Color::Ansi(1)));
        assert_eq!(foreground("1;94"), Some(Color::Ansi(12)));
        assert_eq!(foreground("38;5;196"), Some(Color::Rgb(255, 0, 0)));
        assert_eq!(foreground("38;2;1;2;3"), Some(Color::Rgb(1, 2, 3)));
        assert_eq!(foreground("01"), None);
        assert_eq!(from_256(244), Color::Rgb(128, 128, 128));
    }

    #[test]
    fn p1_map_02_ls_colors_takes_executables_and_suffixes() {
        let table = ls_colors(
            "di=01;34:ln=01;36:ex=01;32:*.png=38;5;196:*.TXT=01;34:*.tar.gz=01;31:*README=33:*.bad=01:*=31",
        );
        assert_eq!(table.executable, Some(Color::Ansi(2)));
        let red = Color::Rgb(255, 0, 0);
        assert_eq!(table.of(b"photo.png", None), Some(red));
        // Case does not matter, and a longer suffix is a suffix like any.
        assert_eq!(table.of(b"notes.txt", None), Some(Color::Ansi(4)));
        assert_eq!(table.of(b"src.tar.gz", None), Some(Color::Ansi(1)));
        assert_eq!(table.of(b"README", None), Some(Color::Ansi(3)));
        assert_eq!(table.of(b"FOO_README", None), Some(Color::Ansi(3)));
        // ex= comes before the suffixes for a file with an execute bit;
        // without a mode (a basic scan) a file is not known to be one.
        assert_eq!(table.of(b"run.png", Some(0o755)), Some(Color::Ansi(2)));
        assert_eq!(table.of(b"run.png", Some(0o644)), Some(red));
        assert_eq!(table.of(b"run", None), None);
        // An entry without a foreground colour, or an empty suffix, is
        // left out; the last matching suffix wins.
        assert_eq!(table.of(b"x.bad", None), None);
        let later = ls_colors("*.png=31:*png=32");
        assert_eq!(later.of(b"a.png", None), Some(Color::Ansi(2)));
        assert_eq!(ls_colors(""), super::LsColors::default());
    }

    #[test]
    fn p1_map_02_colorfgbg_picks_the_light_pack_on_a_light_background() {
        let pack = |vars: &[(&str, &str)]| decide(None, true, &env(vars)).unwrap().pack.name;
        // `fg;bg`: a background of 7 or 15 is light.
        assert_eq!(pack(&[("COLORFGBG", "0;15")]), "light");
        assert_eq!(pack(&[("COLORFGBG", "0;default;7")]), "light");
        assert_eq!(pack(&[("COLORFGBG", "15;0")]), "mocha");
        assert_eq!(pack(&[]), "mocha");
        assert_eq!(pack(&[("COLORFGBG", "garbage")]), "mocha");
        assert_eq!(pack(&[("COLORFGBG", "")]), "mocha");
        // --theme wins over the background; without colour, no-color.
        let dracula = OsString::from("dracula");
        let light = env(&[("COLORFGBG", "0;15")]);
        assert_eq!(
            decide(Some(&dracula), true, &light).unwrap().pack.name,
            "dracula"
        );
        assert_eq!(
            decide(Some(&dracula), false, &light).unwrap().pack.name,
            "no-color"
        );
    }

    #[test]
    fn p1_map_02_colorterm_and_the_pack_set_the_colour_depth() {
        let depth = |theme: Option<&str>, color: bool, vars: &[(&str, &str)]| {
            let theme = theme.map(OsString::from);
            decide(theme.as_ref(), color, &env(vars)).unwrap().depth
        };
        assert_eq!(
            depth(None, true, &[("COLORTERM", "truecolor")]),
            Depth::TrueColor
        );
        assert_eq!(
            depth(None, true, &[("COLORTERM", "24bit")]),
            Depth::TrueColor
        );
        assert_eq!(depth(None, true, &[("COLORTERM", "yes")]), Depth::Ansi256);
        assert_eq!(depth(None, true, &[]), Depth::Ansi256);
        let truecolor = [("COLORTERM", "truecolor")];
        assert_eq!(
            depth(Some("terminal-native"), true, &truecolor),
            Depth::Ansi16
        );
        assert_eq!(depth(Some("no-color"), true, &truecolor), Depth::None);
        assert_eq!(depth(None, false, &truecolor), Depth::None);
    }

    #[test]
    fn p1_map_02_a_locale_without_utf8_draws_ascii() {
        let glyphs = |vars: &[(&str, &str)]| decide(None, true, &env(vars)).unwrap().glyphs;
        assert_eq!(glyphs(&[("LANG", "en_US.UTF-8")]), Glyphs::Unicode);
        assert_eq!(glyphs(&[("LC_CTYPE", "de_DE.utf8")]), Glyphs::Unicode);
        // LC_ALL, then LC_CTYPE, then LANG: the first set and not empty.
        assert_eq!(
            glyphs(&[("LC_ALL", "C"), ("LANG", "en_US.UTF-8")]),
            Glyphs::Ascii
        );
        assert_eq!(
            glyphs(&[("LC_ALL", ""), ("LC_CTYPE", "C.UTF-8"), ("LANG", "C")]),
            Glyphs::Unicode
        );
        assert_eq!(glyphs(&[("LANG", "POSIX")]), Glyphs::Ascii);
        assert_eq!(glyphs(&[]), Glyphs::Ascii);
        // LS_COLORS is read with the rest.
        let style = decide(None, true, &env(&[("LS_COLORS", "ex=32")])).unwrap();
        assert_eq!(style.ls_colors.executable, Some(Color::Ansi(2)));
    }

    #[test]
    fn p1_map_02_next_commands_are_runnable_and_fit() {
        // In the scanned directory, no path is needed.
        assert_eq!(
            next_line(None, "·", 80),
            "next: duscope top --depth 2 · duscope by-type · duscope stale"
        );
        // Elsewhere the path as given, quoted for the shell.
        assert_eq!(
            next_line(Some("../my photos"), "-", 120),
            "next: duscope top --depth 2 '../my photos' - duscope by-type '../my photos' - duscope stale '../my photos'"
        );
        // What does not fit is left out, never cut; the first stays.
        let long = "/Volumes/Archive/projects/2026/clients";
        let line = next_line(Some(long), "·", 80);
        assert_eq!(line, format!("next: duscope top --depth 2 {long}"));
        assert_eq!(
            next_line(None, "·", 50),
            "next: duscope top --depth 2 · duscope by-type"
        );
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
        assert_eq!(shell_quote("/tmp/a-b_c.d"), "/tmp/a-b_c.d");
        assert_eq!(shell_quote(""), "''");
    }

    #[test]
    fn p1_map_02_the_frame_shows_one_warning_and_counts_the_rest() {
        let warning = |code: &'static str, message: &str| Warning {
            code,
            message: message.to_owned(),
            count: None,
            hint: None,
        };
        assert_eq!(frame_warning(&[]), None);
        let partial = warning("partial_read", "2 directories could not be read");
        assert_eq!(
            frame_warning(std::slice::from_ref(&partial)).as_deref(),
            Some("warning: 2 directories could not be read")
        );
        let mounts = warning("mounts_excluded", "1 mount was not entered");
        assert_eq!(
            frame_warning(&[partial, mounts]).as_deref(),
            Some("warning: 2 directories could not be read (+1 more below)")
        );
    }
}
