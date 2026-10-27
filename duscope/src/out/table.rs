//! Tables for people, and file names made safe to show them.

use std::borrow::Cow;
use std::fmt::Write as _;

use super::{Row, Units};

/// A table for people: a heading line, then a line per row. Every column
/// but the last is right-aligned; the last, the path, is not padded.
pub fn table<T: Row>(rows: &[T], units: Units, size_kind: &str, color: bool) -> String {
    let cells: Vec<Vec<String>> = rows.iter().map(|row| row.cells(units, size_kind)).collect();
    let last = T::COLUMNS.len() - 1;
    let widths: Vec<usize> = (0..last)
        .map(|c| {
            cells
                .iter()
                .map(|row| row[c].chars().count())
                .chain([T::COLUMNS[c].len()])
                .max()
                .unwrap_or(0)
        })
        .collect();
    let line = |row: &[&str]| {
        let mut out = String::new();
        for (c, cell) in row.iter().enumerate() {
            if c < last {
                let _ = write!(out, "{cell:>width$}  ", width = widths[c]);
            } else {
                out.push_str(cell);
            }
        }
        out
    };
    let mut out = line(T::COLUMNS);
    if color {
        out = format!("\x1b[1m{out}\x1b[0m");
    }
    out.push('\n');
    for row in &cells {
        let row: Vec<&str> = row.iter().map(String::as_str).collect();
        out.push_str(&line(&row));
        out.push('\n');
    }
    out
}

/// `text` with control characters shown as escapes (`\x1b`), so a file
/// name cannot move the cursor or change colours.
pub fn printable(text: &str) -> Cow<'_, str> {
    if !text.chars().any(char::is_control) {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len() + 8);
    for c in text.chars() {
        if c.is_control() {
            let _ = write!(out, "\\x{:02x}", u32::from(c));
        } else {
            out.push(c);
        }
    }
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::printable;

    #[test]
    fn p1_cli_02_control_characters_are_escaped_for_people() {
        assert_eq!(printable("plain.txt"), "plain.txt");
        assert_eq!(
            printable("escape-\x1b[31mred\x1b[0m.txt"),
            "escape-\\x1b[31mred\\x1b[0m.txt"
        );
        assert_eq!(printable("tab\there\u{9b}"), "tab\\x09here\\x9b");
    }
}
