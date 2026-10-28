//! The cell buffer the static map is painted into, and its output: ANSI
//! escapes in truecolour, the 256-colour cube or the 16 ANSI colours, or
//! none; and a text form for snapshots.

use std::fmt::Write as _;

use crate::width::{char_width, glyphs};

/// A colour as a cell holds it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Color {
    /// The terminal's own foreground or background.
    #[default]
    Reset,
    /// One of the 16 ANSI colours.
    Ansi(u8),
    /// Red, green, blue.
    Rgb(u8, u8, u8),
}

impl Color {
    /// `#rrggbb`.
    #[must_use]
    pub fn hex(text: &str) -> Option<Color> {
        let hex = text.strip_prefix('#')?;
        if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
        Some(Color::Rgb(byte(0)?, byte(2)?, byte(4)?))
    }

    /// `self` mixed `percent` toward `other` (both RGB; anything else is
    /// left as it is).
    #[must_use]
    pub fn mix(self, other: Color, percent: u8) -> Color {
        match (self, other) {
            (Color::Rgb(red, green, blue), Color::Rgb(to_red, to_green, to_blue)) => {
                let share = u16::from(percent.min(100));
                let blend = |from: u8, to: u8| {
                    let mixed =
                        (u16::from(from) * (100 - share) + u16::from(to) * share + 50) / 100;
                    u8::try_from(mixed).unwrap_or(u8::MAX)
                };
                Color::Rgb(
                    blend(red, to_red),
                    blend(green, to_green),
                    blend(blue, to_blue),
                )
            }
            _ => self,
        }
    }

    /// Relative luminance (sRGB, 0 to 1); `None` for a colour that is not
    /// RGB.
    #[must_use]
    pub fn luminance(self) -> Option<f64> {
        let Color::Rgb(r, g, b) = self else {
            return None;
        };
        let channel = |c: u8| {
            let c = f64::from(c) / 255.0;
            if c <= 0.039_28 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        Some(0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b))
    }

    /// The nearest of the 256 xterm colours: the 6×6×6 cube or the grey
    /// ramp, whichever is closer.
    #[must_use]
    pub fn to_256(self) -> Option<u8> {
        const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
        let Color::Rgb(r, g, b) = self else {
            return None;
        };
        let nearest = |c: u8| {
            (0..6usize)
                .min_by_key(|&i| (i32::from(LEVELS[i]) - i32::from(c)).abs())
                .unwrap_or(0)
        };
        let (ri, gi, bi) = (nearest(r), nearest(g), nearest(b));
        let cube = (LEVELS[ri], LEVELS[gi], LEVELS[bi]);
        let average = (u16::from(r) + u16::from(g) + u16::from(b)) / 3;
        let grey_step = (average.saturating_sub(3) / 10).min(23);
        let grey = u8::try_from(8 + 10 * grey_step).unwrap_or(238);
        let distance = |(x, y, z): (u8, u8, u8)| {
            let d = |a: u8, b: u8| (i32::from(a) - i32::from(b)).pow(2);
            d(x, r) + d(y, g) + d(z, b)
        };
        let index = if distance((grey, grey, grey)) < distance(cube) {
            232 + grey_step
        } else {
            u16::try_from(16 + 36 * ri + 6 * gi + bi).unwrap_or(16)
        };
        u8::try_from(index).ok()
    }

    /// As snapshots spell it: `-`, `aN` or `#rrggbb`.
    fn token(self) -> String {
        match self {
            Color::Reset => "-".to_owned(),
            Color::Ansi(n) => format!("a{n}"),
            Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        }
    }
}

/// How many colours the terminal shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    /// None: no escapes at all.
    None,
    /// The 16 ANSI colours.
    Ansi16,
    /// The 256 xterm colours.
    Ansi256,
    /// 24-bit colour.
    TrueColor,
}

/// One terminal cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    /// The character; `'\0'` in the right half of a wide character, which
    /// the cell to its left prints.
    pub ch: char,
    /// Combining marks printed after `ch`; `'\0'` where there are none.
    pub marks: [char; 2],
    /// Its colour.
    pub fg: Color,
    /// The cell's background.
    pub bg: Color,
    /// Bold.
    pub bold: bool,
}

impl Default for Cell {
    fn default() -> Cell {
        Cell {
            ch: ' ',
            marks: ['\0'; 2],
            fg: Color::Reset,
            bg: Color::Reset,
            bold: false,
        }
    }
}

/// A rectangle of cells, row by row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellBuffer {
    w: u16,
    h: u16,
    cells: Vec<Cell>,
}

impl CellBuffer {
    /// A blank buffer of `w` columns and `h` rows.
    #[must_use]
    pub fn new(w: u16, h: u16) -> CellBuffer {
        CellBuffer {
            w,
            h,
            cells: vec![Cell::default(); usize::from(w) * usize::from(h)],
        }
    }

    /// Columns.
    #[must_use]
    pub fn width(&self) -> u16 {
        self.w
    }

    /// Rows.
    #[must_use]
    pub fn height(&self) -> u16 {
        self.h
    }

    /// Row `y`.
    fn row(&self, y: u16) -> &[Cell] {
        let w = usize::from(self.w);
        &self.cells[usize::from(y) * w..(usize::from(y) + 1) * w]
    }

    /// The cell at column `x`, row `y`.
    #[must_use]
    pub fn get(&self, x: u16, y: u16) -> Option<&Cell> {
        (x < self.w && y < self.h)
            .then(|| &self.cells[usize::from(y) * usize::from(self.w) + usize::from(x)])
    }

    /// The cell at column `x`, row `y`, to change.
    #[must_use]
    pub fn get_mut(&mut self, x: u16, y: u16) -> Option<&mut Cell> {
        (x < self.w && y < self.h)
            .then(|| &mut self.cells[usize::from(y) * usize::from(self.w) + usize::from(x)])
    }

    /// Writes `text` from column `x` on row `y`, clipped at `max` columns
    /// and the buffer's edge, keeping each cell's background; `ascii` keeps
    /// it to ASCII. Control characters show as escapes (`\x1b`), and wide
    /// characters take two columns.
    #[allow(clippy::too_many_arguments)]
    pub fn text(
        &mut self,
        x: u16,
        y: u16,
        text: &str,
        max: u16,
        fg: Color,
        bold: bool,
        ascii: bool,
    ) {
        let max = max.min(self.w.saturating_sub(x));
        lay_out(text, max, ascii, |i, ch, marks| {
            if let Some(cell) = self.get_mut(x + i, y) {
                cell.ch = ch;
                cell.marks = marks;
                cell.fg = fg;
                cell.bold = bold;
            }
        });
    }

    /// The buffer as a terminal shows it: escapes for `depth`, then each
    /// row and a newline.
    #[must_use]
    pub fn to_ansi(&self, depth: Depth) -> String {
        let mut out = String::new();
        for y in 0..self.h {
            let row = self.row(y);
            let mut current: Option<(Color, Color, bool)> = None;
            for x in 0..self.w {
                let cell = row[usize::from(x)];
                if tail(row, usize::from(x)) {
                    continue;
                }
                let style = (cell.fg, cell.bg, cell.bold);
                if depth != Depth::None && current != Some(style) {
                    out.push_str("\x1b[0");
                    if cell.bold {
                        out.push_str(";1");
                    }
                    sgr(&mut out, cell.fg, depth, false);
                    sgr(&mut out, cell.bg, depth, true);
                    out.push('m');
                    current = Some(style);
                }
                push_cell(&mut out, row, usize::from(x));
            }
            if depth != Depth::None {
                out.push_str("\x1b[0m");
            }
            out.push('\n');
        }
        out
    }

    /// The buffer as snapshot text: the characters row by row, then each
    /// row's runs of style as `count fg bg[ b]`.
    #[must_use]
    pub fn snapshot(&self) -> String {
        let mut out = String::new();
        for y in 0..self.h {
            let row = self.row(y);
            for x in 0..row.len() {
                if !tail(row, x) {
                    push_cell(&mut out, row, x);
                }
            }
            out.push('\n');
        }
        out.push_str("--- styles: count fg bg [b]\n");
        for y in 0..self.h {
            let row = &self.cells
                [usize::from(y) * usize::from(self.w)..usize::from(y + 1) * usize::from(self.w)];
            let mut runs: Vec<(usize, (Color, Color, bool))> = Vec::new();
            for cell in row {
                let style = (cell.fg, cell.bg, cell.bold);
                match runs.last_mut() {
                    Some((count, last)) if *last == style => *count += 1,
                    _ => runs.push((1, style)),
                }
            }
            let line: Vec<String> = runs
                .iter()
                .map(|(count, (fg, bg, bold))| {
                    format!(
                        "{count} {} {}{}",
                        fg.token(),
                        bg.token(),
                        if *bold { " b" } else { "" }
                    )
                })
                .collect();
            let _ = writeln!(out, "{y}: {}", line.join(" | "));
        }
        out
    }
}

/// The SGR parameters for `color` as a foreground or background.
/// Lays `text` out in at most `max` columns, calling `put` with each
/// column's offset, character and marks: a wide character's right half is
/// a `'\0'` cell, and a wide character with one column left is a space.
pub(crate) fn lay_out(
    text: &str,
    max: u16,
    ascii: bool,
    mut put: impl FnMut(u16, char, [char; 2]),
) {
    let mut at = 0u16;
    for glyph in glyphs(text, ascii) {
        if at >= max {
            break;
        }
        if glyph.wide && at + 1 >= max {
            put(at, ' ', ['\0'; 2]);
            break;
        }
        put(at, glyph.ch, glyph.marks);
        if glyph.wide {
            put(at + 1, '\0', ['\0'; 2]);
            at += 1;
        }
        at += 1;
    }
}

/// Whether the cell at `x` is the right half of the wide character before
/// it, which prints both columns.
fn tail(row: &[Cell], x: usize) -> bool {
    row[x].ch == '\0' && x > 0 && char_width(row[x - 1].ch) == 2
}

/// Writes the cell at `x` of `row`: its character and marks, or a space
/// for a half of a wide character whose other half was painted over.
fn push_cell(out: &mut String, row: &[Cell], x: usize) {
    let cell = row[x];
    let whole = cell.ch != '\0'
        && (char_width(cell.ch) != 2 || row.get(x + 1).is_some_and(|next| next.ch == '\0'));
    out.push(if whole { cell.ch } else { ' ' });
    out.extend(cell.marks.iter().filter(|&&m| m != '\0'));
}

fn sgr(out: &mut String, color: Color, depth: Depth, background: bool) {
    let (base, bright, extended) = if background {
        (40, 100, 48)
    } else {
        (30, 90, 38)
    };
    match (color, depth) {
        (Color::Reset, _) | (_, Depth::None) => {}
        (Color::Ansi(n), _) => {
            let code = if n < 8 {
                base + u16::from(n)
            } else {
                bright + u16::from(n - 8)
            };
            let _ = write!(out, ";{code}");
        }
        (Color::Rgb(r, g, b), Depth::TrueColor) => {
            let _ = write!(out, ";{extended};2;{r};{g};{b}");
        }
        (rgb @ Color::Rgb(..), Depth::Ansi256) => {
            let _ = write!(out, ";{extended};5;{}", rgb.to_256().unwrap_or(16));
        }
        (rgb @ Color::Rgb(..), Depth::Ansi16) => {
            let _ = write!(out, ";{}", base + u16::from(ansi16(rgb)));
        }
    }
}

/// The nearest of the eight basic ANSI colours.
fn ansi16(color: Color) -> u8 {
    let Color::Rgb(r, g, b) = color else {
        return 7;
    };
    let bit = |c: u8| u8::from(c >= 128);
    bit(r) | (bit(g) << 1) | (bit(b) << 2)
}

#[cfg(test)]
mod tests {
    use super::{CellBuffer, Color, Depth};

    #[test]
    fn p1_map_02_colours_quantise_to_the_cube_and_the_grey_ramp() {
        assert_eq!(Color::hex("#ff0000").unwrap().to_256(), Some(196));
        assert_eq!(Color::hex("#000000").unwrap().to_256(), Some(16));
        assert_eq!(Color::hex("#ffffff").unwrap().to_256(), Some(231));
        // A mid grey is nearer the ramp than the cube.
        assert_eq!(Color::hex("#808080").unwrap().to_256(), Some(244));
        assert_eq!(Color::hex("#89b4fa").unwrap().to_256(), Some(111));
        assert_eq!(Color::hex("#zzzzzz"), None);
    }

    #[test]
    fn p1_map_02_mixing_and_luminance() {
        let base = Color::hex("#1e1e2e").unwrap();
        let tile = Color::hex("#89b4fa").unwrap();
        assert_eq!(tile.mix(base, 20), Color::Rgb(116, 150, 209));
        assert!(tile.luminance().unwrap() > 0.18);
        assert!(base.luminance().unwrap() < 0.18);
    }

    #[test]
    fn p1_map_02_no_colour_writes_no_escapes() {
        let mut buffer = CellBuffer::new(4, 1);
        buffer.text(0, 0, "abcdef", 4, Color::Rgb(1, 2, 3), true, false);
        assert_eq!(buffer.to_ansi(Depth::None), "abcd\n");
        assert!(
            buffer
                .to_ansi(Depth::TrueColor)
                .contains("\x1b[0;1;38;2;1;2;3m")
        );
        assert!(buffer.to_ansi(Depth::Ansi256).contains(";38;5;16m"));
    }
}
