//! The design system's table, for the list, largest and types views: a
//! header row in the title colour, a double rule under it, rows with
//! box-drawing separators in surface2, the first column in subtext, the
//! current row in the text colour, bold, with its `▸`.

use duscope_treemap::cells::Color;
use duscope_treemap::paint::Glyphs;
use ratatui_core::layout::Rect;

use crate::widgets::{Canvas, Span};

/// A column: its title, width and side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Col {
    /// The header.
    pub title: &'static str,
    /// Its width, in columns.
    pub width: u16,
    /// Right-aligned.
    pub right: bool,
}

/// A row of a table: a cell per column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// A cell's spans, one per column. A cell wider than its column is cut.
    pub cells: Vec<Vec<Span>>,
    /// The row the cursor is on.
    pub current: bool,
}

/// The rows a table of `count` rows shows in `room` rows with the cursor on
/// `at`: the first shown; the cursor stays in view, on the last row when it
/// is past the first page.
#[must_use]
pub fn window(count: usize, at: Option<usize>, room: usize) -> usize {
    let Some(at) = at else { return 0 };
    if count <= room || at < room {
        0
    } else {
        at + 1 - room
    }
}

/// Draws the table of `cols` and `rows` into `area` (the pane's inside):
/// the header on its first row, the rule on the second, `rows` from the
/// third. The caller windows `rows`.
pub fn draw(canvas: &mut Canvas<'_>, area: Rect, cols: &[Col], rows: &[Row]) {
    let roles = canvas.pack.roles;
    let ascii = canvas.glyphs == Glyphs::Ascii;
    if area.height == 0 || area.width == 0 {
        return;
    }
    let (sep, rule, cross) = if ascii {
        ("|", '=', '+')
    } else {
        ("│", '═', '╪')
    };
    // The header, the rule beneath it.
    let header: Vec<Vec<Span>> = cols
        .iter()
        .map(|c| vec![Span::bold(c.title, roles.title)])
        .collect();
    draw_row(canvas, area, area.y, cols, &header, sep);
    if area.height >= 2 {
        let y = area.y + 1;
        let mut x = area.x;
        for (i, col) in cols.iter().enumerate() {
            let run = col.width;
            let text: String = std::iter::repeat_n(rule, usize::from(run)).collect();
            canvas.spans(x, y, area.right(), &[Span::new(text, roles.surface2)]);
            x += run;
            if i + 1 < cols.len() {
                canvas.spans(
                    x,
                    y,
                    area.right(),
                    &[Span::new(rule.to_string(), roles.surface2)],
                );
                x += 1;
                canvas.spans(
                    x,
                    y,
                    area.right(),
                    &[Span::new(cross.to_string(), roles.surface2)],
                );
                x += 1;
                canvas.spans(
                    x,
                    y,
                    area.right(),
                    &[Span::new(rule.to_string(), roles.surface2)],
                );
                x += 1;
            }
        }
    }
    for (dy, row) in rows.iter().enumerate() {
        let Ok(dy) = u16::try_from(dy + 2) else {
            break;
        };
        if dy >= area.height {
            break;
        }
        draw_row(canvas, area, area.y + dy, cols, &row.cells, sep);
    }
}

/// Draws one row of a table on line `y`.
fn draw_row(
    canvas: &mut Canvas<'_>,
    area: Rect,
    y: u16,
    cols: &[Col],
    cells: &[Vec<Span>],
    sep: &str,
) {
    let surface2 = canvas.pack.roles.surface2;
    let mut x = area.x;
    let end = area.right();
    for (i, col) in cols.iter().enumerate() {
        if x >= end {
            break;
        }
        let width = col.width.min(end - x);
        let cell = cells.get(i).map_or(&[][..], Vec::as_slice);
        let spans = fit(canvas, cell, width);
        let used: u16 = spans
            .iter()
            .map(|s| canvas.width(&s.text))
            .fold(0, u16::saturating_add);
        let at = if col.right {
            x + width.saturating_sub(used)
        } else {
            x
        };
        canvas.spans(at, y, end, &spans);
        x = x.saturating_add(col.width);
        if i + 1 < cols.len() && x < end {
            canvas.spans(x, y, end, &[Span::new(format!(" {sep} "), surface2)]);
            x = x.saturating_add(3);
        }
    }
}

/// `spans` cut to `width` columns, the last ones dropped and the one that
/// crosses the edge cut with `…`.
fn fit(canvas: &Canvas<'_>, spans: &[Span], width: u16) -> Vec<Span> {
    let mut out = Vec::new();
    let mut room = width;
    for span in spans {
        let w = canvas.width(&span.text);
        if w <= room {
            out.push(span.clone());
            room -= w;
        } else {
            let text = canvas.cut(&span.text, room);
            out.push(Span {
                text,
                fg: span.fg,
                bold: span.bold,
            });
            break;
        }
    }
    out
}

/// The cell of a row's first column: the `▸` on the current row, a square
/// in `color`, and `name`, bold in the text colour on the current row and
/// in subtext on the others.
#[must_use]
pub fn name_cell(canvas: &Canvas<'_>, name: String, color: Color, current: bool) -> Vec<Span> {
    let roles = canvas.pack.roles;
    let ascii = canvas.glyphs == Glyphs::Ascii;
    let marker = match (current, ascii) {
        (true, false) => "▸ ",
        (true, true) => "> ",
        (false, _) => "  ",
    };
    let square = if ascii { "# " } else { "■ " };
    vec![
        Span::new(marker, roles.accent),
        Span::new(square, color),
        if current {
            Span::bold(name, roles.text)
        } else {
            Span::new(name, roles.subtext)
        },
    ]
}
