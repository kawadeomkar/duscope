//! The list view (`2`): the current folder as a table, in the `s` sort,
//! the in-app `tree`. The folder is the one the cursor is in, the panes'
//! siblings; a row is a child with its size, its share of the folder, its
//! files and when its subtree last changed.

use duscope_treemap::paint::compact;
use ratatui_core::layout::Rect;

use super::Viewing;
use super::table::{self, Col};
use crate::layout::inner;
use crate::panes::Context;
use crate::widgets::{Canvas, Span, border, percent, thousands};

/// The share column's width: an 8-cell bar and ` 100%`.
const SHARE: u16 = 13;
/// A column's width beside the name's, separators included.
const SIZE: u16 = 6;
const FILES: u16 = 9;
const MODIFIED: u16 = 16;

/// The folder the list shows with the cursor on `selected` and the map's
/// root `root`: the folder the cursor is in, or the root itself where the
/// cursor is on it (a scan's root has no folder above it).
#[must_use]
pub fn folder(ctx: &Context<'_>, selected: u32, root: u32) -> u32 {
    if selected == root {
        selected
    } else {
        ctx.view.node(selected).parent
    }
}

/// The pane's title: `list · ~/Library/Caches · by name`, the sort named
/// when it is not the start's size order.
fn title(
    canvas: &Canvas<'_>,
    pane: Rect,
    ctx: &Context<'_>,
    viewing: &Viewing<'_>,
    folder: u32,
) -> String {
    let by = if viewing.sort == super::Sort::Size {
        String::new()
    } else {
        format!(" · by {}", viewing.sort.word())
    };
    let tail = format!("{by}{}", viewing.filter_suffix());
    super::path_title(canvas, pane, "list · ", &ctx.path(folder), &tail)
}

/// How many rows the list's table holds in the map pane `pane`.
#[must_use]
pub fn table_rows(pane: Rect) -> usize {
    usize::from(inner(pane).height.saturating_sub(2))
}

/// The list view in `pane`, the map's pane: the children of the folder
/// the cursor is in, the selection's row marked.
pub fn draw(
    canvas: &mut Canvas<'_>,
    pane: Rect,
    ctx: &Context<'_>,
    viewing: &Viewing<'_>,
    selected: u32,
    root: u32,
) {
    let folder = folder(ctx, selected, root);
    let title = title(canvas, pane, ctx, viewing, folder);
    canvas.pane(pane, &title, border(canvas.pack, true));
    let area = inner(pane);
    if area.height < 3 {
        return;
    }
    let kids = viewing.children(ctx, folder);
    let whole = ctx.size(&ctx.view.node(folder));
    let at = kids.iter().position(|&id| id == selected);
    let visible = usize::from(area.height) - 2;
    let start = table::window(kids.len(), at, visible);
    // The name takes what the other columns leave: at most 40 columns, and
    // the time goes first where there is no room for it.
    let seps = 3 * 4;
    let fixed = SIZE + SHARE + FILES + seps;
    let mut cols = vec![
        Col {
            title: "name",
            width: 0,
            right: false,
        },
        Col {
            title: "size",
            width: SIZE,
            right: true,
        },
        Col {
            title: "share",
            width: SHARE,
            right: false,
        },
        Col {
            title: "files",
            width: FILES,
            right: true,
        },
    ];
    let with_time = area.width >= fixed + MODIFIED + 3 + 14;
    if with_time {
        cols.push(Col {
            title: "modified",
            width: MODIFIED,
            right: false,
        });
    }
    let used: u16 = cols.iter().map(|c| c.width).sum::<u16>()
        + 3 * u16::try_from(cols.len()).unwrap_or(0).saturating_sub(1);
    cols[0].width = area.width.saturating_sub(used).clamp(8, 40);
    let name_width = cols[0].width.saturating_sub(4);

    let rows: Vec<table::Row> = kids[start..]
        .iter()
        .take(visible)
        .map(|&id| {
            row(
                canvas,
                ctx,
                viewing,
                id,
                whole,
                name_width,
                id == selected,
                with_time,
            )
        })
        .collect();
    table::draw(canvas, area, &cols, &rows);
}

/// The row of child `id` of a folder of `whole` bytes.
#[allow(clippy::too_many_arguments)]
fn row(
    canvas: &Canvas<'_>,
    ctx: &Context<'_>,
    viewing: &Viewing<'_>,
    id: u32,
    whole: u64,
    name_width: u16,
    current: bool,
    with_time: bool,
) -> table::Row {
    let roles = canvas.pack.roles;
    let node = ctx.view.node(id);
    let bytes = ctx.size(&node);
    let share = Context::share(bytes, whole);
    let name = viewing.marked_name(canvas, id, ctx.name(id, true));
    let name = super::one_line(canvas, &name, name_width);
    let color = ctx.color(&node);
    let mut bar = canvas.bar(share, 8, color);
    bar.push(Span::new(format!(" {:>3}%", percent(share)), roles.subtext));
    let mut cells = vec![
        table::name_cell(canvas, name, color, current),
        vec![Span::new(compact(bytes), roles.text)],
        bar,
        vec![Span::new(thousands(node.file_count), roles.subtext)],
    ];
    if with_time {
        cells.push(vec![Span::new(
            crate::panes::minute(node.max_mtime_ns),
            roles.subtext,
        )]);
    }
    table::Row { cells, current }
}
