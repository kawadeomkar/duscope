//! Names as the map prints them: how many terminal columns a character
//! takes, and text made safe to show.
//!
//! A control character, or a bidirectional formatting one that would
//! reorder what follows it, is shown as its escape (`\x1b`, `\x202e`), as
//! the table output shows it, so a file name cannot move the cursor, set
//! the window title or break a row. A combining mark rides on the glyph
//! before it (macOS keeps names decomposed: `e` then U+0301), and an East
//! Asian wide character or emoji takes two columns. The tables cover the
//! scripts and emoji names are commonly written in; a rarer combining mark
//! counts as one column. Under ASCII glyphs every other non-ASCII character
//! is `?`.

use std::fmt::Write as _;

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
/// or an emoji, 1 otherwise.
pub(crate) fn char_width(c: char) -> usize {
    let c = u32::from(c);
    if within(c, ZERO) {
        0
    } else if within(c, WIDE) {
        2
    } else {
        1
    }
}

fn within(c: u32, ranges: &[(u32, u32)]) -> bool {
    ranges
        .binary_search_by(|&(lo, hi)| {
            if hi < c {
                std::cmp::Ordering::Less
            } else if lo > c {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

/// Combining marks and zero-width characters, sorted.
const ZERO: &[(u32, u32)] = &[
    (0x0300, 0x036F),
    (0x0483, 0x0489),
    (0x0591, 0x05BD),
    (0x05BF, 0x05BF),
    (0x05C1, 0x05C2),
    (0x05C4, 0x05C5),
    (0x05C7, 0x05C7),
    (0x0610, 0x061A),
    (0x064B, 0x065F),
    (0x0670, 0x0670),
    (0x06D6, 0x06DC),
    (0x06DF, 0x06E4),
    (0x06E7, 0x06E8),
    (0x06EA, 0x06ED),
    (0x0711, 0x0711),
    (0x0730, 0x074A),
    (0x07A6, 0x07B0),
    (0x0900, 0x0902),
    (0x093A, 0x093A),
    (0x093C, 0x093C),
    (0x0941, 0x0948),
    (0x094D, 0x094D),
    (0x0951, 0x0957),
    (0x0962, 0x0963),
    (0x0981, 0x0981),
    (0x09BC, 0x09BC),
    (0x09C1, 0x09C4),
    (0x09CD, 0x09CD),
    (0x0E31, 0x0E31),
    (0x0E34, 0x0E3A),
    (0x0E47, 0x0E4E),
    (0x0EB1, 0x0EB1),
    (0x0EB4, 0x0EBC),
    (0x0EC8, 0x0ECD),
    (0x1160, 0x11FF),
    (0x1AB0, 0x1AFF),
    (0x1DC0, 0x1DFF),
    (0x200B, 0x200D),
    (0x20D0, 0x20FF),
    (0x302A, 0x302D),
    (0x3099, 0x309A),
    (0xFE00, 0xFE0F),
    (0xFE20, 0xFE2F),
    (0xFEFF, 0xFEFF),
    (0x1F3FB, 0x1F3FF),
    (0xE0100, 0xE01EF),
];

/// East Asian wide and fullwidth characters and wide emoji, sorted.
const WIDE: &[(u32, u32)] = &[
    (0x1100, 0x115F),
    (0x231A, 0x231B),
    (0x2329, 0x232A),
    (0x23E9, 0x23EC),
    (0x23F0, 0x23F0),
    (0x23F3, 0x23F3),
    (0x25FD, 0x25FE),
    (0x2614, 0x2615),
    (0x2648, 0x2653),
    (0x267F, 0x267F),
    (0x2693, 0x2693),
    (0x26A1, 0x26A1),
    (0x26AA, 0x26AB),
    (0x26BD, 0x26BE),
    (0x26C4, 0x26C5),
    (0x26CE, 0x26CE),
    (0x26D4, 0x26D4),
    (0x26EA, 0x26EA),
    (0x26F2, 0x26F3),
    (0x26F5, 0x26F5),
    (0x26FA, 0x26FA),
    (0x26FD, 0x26FD),
    (0x2705, 0x2705),
    (0x270A, 0x270B),
    (0x2728, 0x2728),
    (0x274C, 0x274C),
    (0x274E, 0x274E),
    (0x2753, 0x2755),
    (0x2757, 0x2757),
    (0x2795, 0x2797),
    (0x27B0, 0x27B0),
    (0x27BF, 0x27BF),
    (0x2B1B, 0x2B1C),
    (0x2B50, 0x2B50),
    (0x2B55, 0x2B55),
    (0x2E80, 0x3029),
    (0x302E, 0x303E),
    (0x3041, 0x3098),
    (0x309B, 0x33FF),
    (0x3400, 0x4DBF),
    (0x4E00, 0x9FFF),
    (0xA000, 0xA4CF),
    (0xA960, 0xA97F),
    (0xAC00, 0xD7A3),
    (0xF900, 0xFAFF),
    (0xFE10, 0xFE19),
    (0xFE30, 0xFE6F),
    (0xFF00, 0xFF60),
    (0xFFE0, 0xFFE6),
    (0x16FE0, 0x16FE4),
    (0x17000, 0x18CFF),
    (0x1B000, 0x1B2FF),
    (0x1F004, 0x1F004),
    (0x1F0CF, 0x1F0CF),
    (0x1F18E, 0x1F18E),
    (0x1F191, 0x1F19A),
    (0x1F200, 0x1F202),
    (0x1F210, 0x1F23B),
    (0x1F240, 0x1F248),
    (0x1F250, 0x1F251),
    (0x1F260, 0x1F265),
    (0x1F300, 0x1F320),
    (0x1F32D, 0x1F335),
    (0x1F337, 0x1F37C),
    (0x1F37E, 0x1F393),
    (0x1F3A0, 0x1F3CA),
    (0x1F3CF, 0x1F3D3),
    (0x1F3E0, 0x1F3F0),
    (0x1F3F4, 0x1F3F4),
    (0x1F3F8, 0x1F3FA),
    (0x1F400, 0x1F43E),
    (0x1F440, 0x1F440),
    (0x1F442, 0x1F4FC),
    (0x1F4FF, 0x1F53D),
    (0x1F54B, 0x1F54E),
    (0x1F550, 0x1F567),
    (0x1F57A, 0x1F57A),
    (0x1F595, 0x1F596),
    (0x1F5A4, 0x1F5A4),
    (0x1F5FB, 0x1F64F),
    (0x1F680, 0x1F6C5),
    (0x1F6CC, 0x1F6CC),
    (0x1F6D0, 0x1F6D2),
    (0x1F6D5, 0x1F6D7),
    (0x1F6DC, 0x1F6DF),
    (0x1F6EB, 0x1F6EC),
    (0x1F6F4, 0x1F6FC),
    (0x1F7E0, 0x1F7EB),
    (0x1F7F0, 0x1F7F0),
    (0x1F90C, 0x1F93A),
    (0x1F93C, 0x1F945),
    (0x1F947, 0x1F9FF),
    (0x1FA70, 0x1FAFF),
    (0x20000, 0x2FFFD),
    (0x30000, 0x3FFFD),
];

#[cfg(test)]
mod tests {
    use super::{WIDE, ZERO, char_width, columns, glyphs};

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
        // The tables are sorted and do not overlap, as the search needs.
        for table in [ZERO, WIDE] {
            assert!(table.windows(2).all(|w| w[0].1 < w[1].0));
            assert!(table.iter().all(|&(lo, hi)| lo <= hi));
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
