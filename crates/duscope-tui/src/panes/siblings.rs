//! The siblings pane: the selection's parent's children, largest first or
//! in the sort `s` chose ([`crate::views::Viewing::children`]), the order
//! `g`, `G` and the pages walk them: the first nine with 6-cell bars, then
//! `… n more`; titled with the parent's name, size and file count, and the
//! sort where it is not by size. A selection past the first nine, reached
//! through the list where its tile is too small to draw (or the map has
//! none), shows the nine it falls among, under a `… n more` row for the
//! larger ones above. `⇥` focuses it: the arrows then move along it, and
//! its border takes the accent colour. Reading the list is O(children),
//! once for a folder and sort.

use duscope_treemap::paint::compact;
use ratatui_core::layout::Rect;

use super::Context;
use crate::layout::inner;
use crate::views::{Sort, Viewing};
use crate::widgets::{Canvas, ListRow, border, full_size, thousands};

/// The siblings listed before `… n more`: a page of `PgUp` and `PgDn`.
const SIBLINGS: usize = 9;
/// The bar of a sibling.
const SIBLING_BAR: u16 = 6;

/// The siblings pane in `pane`, border included: the selection's parent's
/// children in the sort of `viewing`, the selection marked; the accent
/// border when it is `focused`.
pub fn siblings(
    canvas: &mut Canvas<'_>,
    pane: Rect,
    ctx: &Context<'_>,
    id: u32,
    viewing: &Viewing<'_>,
    focused: bool,
) {
    let roles = canvas.pack.roles;
    let node = ctx.view.node(id);
    let parent = ctx.parent(&node);
    let mut title = format!(
        "in {} · {} · {} files",
        ctx.name(parent.id, false),
        full_size(ctx.size(&parent)),
        thousands(parent.file_count)
    );
    // Ahead of the rest, which a narrow pane cuts.
    if viewing.sort != Sort::Size {
        title = format!("by {} · {title}", viewing.sort.word());
    }
    let frame = if focused {
        roles.accent
    } else {
        border(canvas.pack, false)
    };
    canvas.pane(pane, &title, frame);
    let area = inner(pane);
    let listed = viewing.children(ctx, parent.id);
    let lonely = [parent.id];
    let kids: &[u32] = if listed.is_empty() { &lonely } else { &listed };
    let size = |id: u32| ctx.size(&ctx.view.node(id));
    let whole = ctx.size(&parent);
    let name_width = area.width.saturating_sub(23).min(16);
    let at = kids.iter().position(|&k| k == id).unwrap_or(0);
    let (start, shown) = window(kids.len(), at, SIBLINGS, usize::from(area.height));
    let mut y = area.y;
    let bottom = area.bottom();
    let more = |canvas: &mut Canvas<'_>, y: u16, left: &[u32]| {
        if y >= bottom {
            return;
        }
        let bytes: u64 = left.iter().map(|&k| size(k)).sum();
        let (label, size) = (format!("… {} more", left.len()), compact(bytes));
        canvas.list_row(
            area.x,
            y,
            area.right(),
            &ListRow {
                current: Some(false),
                color: roles.overlay1,
                name: &label,
                name_color: Some(roles.overlay1),
                size: &size,
                share: Context::share(bytes, whole),
            },
            name_width,
            SIBLING_BAR,
        );
    };
    if start > 0 {
        more(canvas, y, &kids[..start]);
        y += 1;
    }
    for &kid in &kids[start..start + shown] {
        if y >= bottom {
            break;
        }
        let node = ctx.view.node(kid);
        let (name, bytes) = (ctx.name(kid, true), ctx.size(&node));
        let size = compact(bytes);
        canvas.list_row(
            area.x,
            y,
            area.right(),
            &ListRow {
                current: Some(kid == id),
                color: ctx.color(&node),
                name: &name,
                name_color: None,
                size: &size,
                share: Context::share(bytes, whole),
            },
            name_width,
            SIBLING_BAR,
        );
        y += 1;
    }
    if start + shown < kids.len() {
        more(canvas, y, &kids[start + shown..]);
    }
}

/// How many of `count` rows a list shows in `room` rows when it shows at
/// most `most` before a `… n more` row: all when they fit, else `most`, or
/// as many as leave room for the `more` row.
pub(super) fn list_rows(count: usize, most: usize, room: usize) -> usize {
    if count <= most.min(room) {
        count
    } else {
        most.min(room.saturating_sub(1))
    }
}

/// The entries a list of `count` shows in `room` rows with the selection
/// `at` its `at`th, at most `most` at a time: the first shown and how many.
/// The first page is [`list_rows`]'; a selection past it shows the page it
/// falls on, pages of `most` (fewer where the rows above and below for `…
/// n more` leave less room), so that `PgDn` from the first entry turns one
/// page.
pub(super) fn window(count: usize, at: usize, most: usize, room: usize) -> (usize, usize) {
    let first = list_rows(count, most, room);
    if at < first || first == count {
        return (0, first);
    }
    let page = most.min(room.saturating_sub(2)).max(1);
    let start = first + (at - first) / page * page;
    (start, page.min(count - start))
}
