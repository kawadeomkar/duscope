//! Names as the map prints them: how many terminal columns a character
//! takes, and text made safe to show.
//!
//! A control character, or a bidirectional formatting one that would
//! reorder what follows it, is shown as its escape (`\x1b`, `\x202e`), as
//! the table output shows it, so a file name cannot move the cursor, set
//! the window title or break a row. Widths are `unicode-width`'s, from the
//! Unicode tables of every script: a combining mark or another zero-width
//! character rides on the glyph before it (macOS keeps names decomposed:
//! `e` then U+0301), and an East Asian wide character or emoji takes two
//! columns. Under ASCII glyphs every other non-ASCII character is `?`.

use std::fmt::Write as _;

use unicode_width::UnicodeWidthChar as _;

/// One glyph: a character and the combining marks on it (`'\0'` where
/// there are none), one column wide or two.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Glyph {
    pub(crate) ch: char,
    pub(crate) marks: [char; 2],
    pub(crate) wide: bool,
}

impl Glyph {
    fn narrow(ch: char) -> Glyph {
        Glyph {
            ch,
            marks: ['\0'; 2],
            wide: false,
        }
    }

    /// The columns it takes.
    pub(crate) fn columns(self) -> usize {
        if self.wide { 2 } else { 1 }
    }
}

/// `text` as glyphs; `ascii` keeps them to ASCII.
pub(crate) fn glyphs(text: &str, ascii: bool) -> Vec<Glyph> {
    let mut out: Vec<Glyph> = Vec::with_capacity(text.len());
    for c in text.chars() {
        if escaped(c) {
            let mut escape = String::new();
            let _ = write!(escape, "\\x{:02x}", u32::from(c));
            out.extend(escape.chars().map(Glyph::narrow));
            continue;
        }
        match char_width(c) {
            0 => {
                // A mark with nothing before it, a third mark, or any mark
                // under ASCII glyphs is left out.
                if let Some(last) = out.last_mut().filter(|_| !ascii)
                    && let Some(slot) = last.marks.iter_mut().find(|m| **m == '\0')
                {
                    *slot = c;
                }
            }
            _ if ascii && !c.is_ascii() => out.push(Glyph::narrow('?')),
            2 => out.push(Glyph {
                wide: true,
                ..Glyph::narrow(c)
            }),
            _ => out.push(Glyph::narrow(c)),
        }
    }
    out
}

/// The columns `text` takes.
pub(crate) fn columns(text: &str, ascii: bool) -> usize {
    glyphs(text, ascii).iter().map(|g| g.columns()).sum()
}

/// Whether `c` is shown as its escape: a control character, a line or
/// paragraph separator, or a bidirectional formatting character.
fn escaped(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{061c}'
                | '\u{200e}'
                | '\u{200f}'
                | '\u{2028}'..='\u{202e}'
                | '\u{2066}'..='\u{2069}'
        )
}

/// The columns `c` takes in a terminal: 0 for a combining mark or another
/// zero-width character, 2 for an East Asian wide or fullwidth character
/// or an emoji, 1 otherwise (a control character, which is shown as its
/// escape, included).
pub(crate) fn char_width(c: char) -> usize {
    c.width().unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::{char_width, columns, glyphs};

    #[test]
    fn p1_map_02_widths_count_columns_not_characters() {
        // Decomposed é: two characters, one column.
        assert_eq!(
            columns("Beyonce\u{301} re\u{301}sume\u{301}.mp3", false),
            18
        );
        assert_eq!(columns("写真フォルダ", false), 12);
        assert_eq!(columns("🎵 song", false), 7);
        assert_eq!(char_width('a'), 1);
        assert_eq!(char_width('\u{200d}'), 0);
        // The marks of every script ride on their base: Tamil, Telugu,
        // Malayalam, Kannada, Gujarati, Sinhala, Tibetan, Myanmar, Khmer.
        assert_eq!(columns("தமிழ்", false), 4);
        for name in ["తె", "ക്", "ಕ್", "ક્", "ක්", "ཀྱ", "က်", "ក្"]
        {
            assert_eq!(columns(name, false), 1, "{name}");
        }
    }

    #[test]
    fn p1_map_02_control_characters_show_as_escapes() {
        let shown: String = glyphs("evil\x1b]0;PWNED\x07\n\u{202e}x", false)
            .iter()
            .map(|g| g.ch)
            .collect();
        assert_eq!(shown, "evil\\x1b]0;PWNED\\x07\\x0a\\x202ex");
        // Marks ride on the glyph before them; ASCII glyphs drop them and
        // show other non-ASCII characters as `?`.
        let marked = glyphs("e\u{301}", false);
        assert_eq!((marked.len(), marked[0].marks[0]), (1, '\u{301}'));
        let ascii: String = glyphs("e\u{301}·写", true).iter().map(|g| g.ch).collect();
        assert_eq!(ascii, "e??");
    }
}
