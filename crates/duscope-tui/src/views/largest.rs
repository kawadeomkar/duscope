//! The largest view (`3`): the largest files under the map's root, the
//! in-app `top --files`, from the saved scan's own ranking where it has
//! one. A row is a file with its size, its share of the root and the
//! folder it is in, relative to the root. The rows are worked out once for
//! a root and a mask.

use std::rc::Rc;

use duscope_core::query::{SizeKind, SortKey, ranked, ranked_files};
use duscope_treemap::paint::compact;
use ratatui_core::layout::Rect;

use super::Viewing;
use super::table::{self, Col};
use crate::layout::inner;
use crate::panes::Context;
use crate::widgets::{Canvas, Span, border, percent, thousands};

/// The files listed.
pub const LIMIT: usize = 1000;
const SIZE: u16 = 6;
const SHARE: u16 = 13;

/// The largest files under `root`, those the mask keeps, largest first.
#[must_use]
pub fn rows(ctx: &Context<'_>, viewing: &Viewing<'_>, root: u32) -> Rc<Vec<u32>> {
    if let Some(found) = viewing
        .rows
        .largest
        .borrow()
        .as_ref()
        .filter(|(at, _)| *at == root)
    {
        return Rc::clone(&found.1);
    }
    let view = ctx.view;
    let size = ctx.opts.size;
    let node = view.node(root);
    let ids = if !node.is_dir() {
        vec![root]
    } else if let (None, SizeKind::Allocated, Some(found)) =
        (viewing.mask, size, ranked_files(view, root, LIMIT, None))
    {
        found.ids
    } else {
        let candidates = (root..=node.last_id).filter(|&id| {
            let n = view.node(id);
            !n.is_dir() && viewing.mask.is_none_or(|mask| mask.keeps(ctx, &n))
        });
        ranked(view, candidates, SortKey::Size, size, false, Some(LIMIT)).0
    };
    let ids = Rc::new(ids);
    *viewing.rows.largest.borrow_mut() = Some((root, Rc::clone(&ids)));
    ids
}

/// The largest view in `pane`, the map's pane.
pub fn draw(
    canvas: &mut Canvas<'_>,
    pane: Rect,
    ctx: &Context<'_>,
    viewing: &Viewing<'_>,
    selected: u32,
    root: u32,
) {
    let ids = rows(ctx, viewing, root);
    let count = if ids.len() == LIMIT {
        format!("top {}", thousands(ids.len() as u64))
    } else {
        thousands(ids.len() as u64)
    };
    let tail = format!(" · {count} files{}", viewing.filter_suffix());
    let title = super::path_title(canvas, pane, "largest · ", &ctx.path(root), &tail);
    canvas.pane(pane, &title, border(canvas.pack, true));
    let area = inner(pane);
    if area.height < 3 {
        return;
    }
    let at = ids.iter().position(|&id| id == selected);
    let visible = usize::from(area.height) - 2;
    let start = table::window(ids.len(), at, visible);
    let roles = canvas.pack.roles;
    let whole = ctx.size(&ctx.view.node(root));
    let seps = 3 * 3;
    let fixed = SIZE + SHARE + seps;
    let rest = area.width.saturating_sub(fixed);
    let name = (rest / 2).clamp(8, 48);
    let cols = [
        Col {
            title: "name",
            width: name,
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
            title: "in",
            width: rest.saturating_sub(name),
            right: false,
        },
    ];
    let rows: Vec<table::Row> = ids[start..]
        .iter()
        .take(visible)
        .map(|&id| {
            let node = ctx.view.node(id);
            let bytes = ctx.size(&node);
            let share = Context::share(bytes, whole);
            let color = ctx.color(&node);
            let current = id == selected;
            let name = super::one_line(canvas, &ctx.name(id, false), name.saturating_sub(4));
            // The nearest folders tell the most; the path's start is the
            // map's root, which the title names.
            let parent = canvas.cut_left(&ctx.path(node.parent), cols[3].width);
            let mut bar = canvas.bar(share, 8, color);
            bar.push(Span::new(format!(" {:>3}%", percent(share)), roles.subtext));
            table::Row {
                cells: vec![
                    table::name_cell(canvas, name, color, current),
                    vec![Span::new(compact(bytes), roles.text)],
                    bar,
                    vec![Span::new(parent, roles.subtext)],
                ],
                current,
            }
        })
        .collect();
    table::draw(canvas, area, &cols, &rows);
}
