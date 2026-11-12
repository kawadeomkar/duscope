//! `?`: every binding of the key map, in its order, over the panes.
//!
//! Two columns, each row a key and what it does, in the design system's
//! key colours. The rows are [`crate::keys::ALL`]'s and nothing else, so
//! a key added there is listed here.

use ratatui_core::layout::Rect;

use crate::keys::{ALL, Binding};
use crate::layout::inner;
use crate::widgets::{Canvas, Span};

/// The columns of the keys.
const KEYS: u16 = 11;

/// How many rows the help's two columns take: every binding, half in each.
#[must_use]
pub fn rows_per_column() -> usize {
    ALL.len().div_ceil(2)
}

/// A row's two parts: the keys as this terminal writes them and the words.
#[must_use]
pub fn row(binding: &Binding, ascii: bool) -> (&'static str, &'static str) {
    (binding.keys(ascii), binding.what)
}

/// The help in `pane`, border included.
pub fn draw(canvas: &mut Canvas<'_>, pane: Rect, ascii: bool) {
    let roles = canvas.pack.roles;
    canvas.fill(pane, roles.base);
    canvas.pane(pane, "keys", roles.accent);
    let area = inner(pane);
    if area.width < 24 || area.height == 0 {
        return;
    }
    let per = rows_per_column();
    let width = area.width / 2;
    for (i, binding) in ALL.iter().enumerate() {
        let (keys, what) = row(binding, ascii);
        let column = u16::from(i >= per);
        let row = if i < per { i } else { i - per };
        let Ok(dy) = u16::try_from(row) else { break };
        if dy >= area.height {
            break;
        }
        let x = area.x + column * width;
        let end = (area.x + (column + 1) * width).min(area.right());
        let keys = canvas.cut(keys, KEYS);
        let pad = usize::from(KEYS.saturating_sub(canvas.width(&keys)));
        canvas.spans(
            x,
            area.y + dy,
            end,
            &[
                Span::bold(format!("{keys}{}", " ".repeat(pad)), roles.accent),
                Span::new(format!(" {what}"), roles.subtext),
            ],
        );
    }
}
