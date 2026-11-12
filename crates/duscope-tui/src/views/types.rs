//! The types view (`4`): the classes of the files under the map's root, as
//! `by-type` reads them, each with its size, its share, its files and its
//! largest file; `⏎` on a row filters the map to that class. The rows are
//! worked out once for a root.

use std::rc::Rc;

use duscope_core::envelope::ByTypeEntry;
use duscope_core::query::{ByTypeQuery, Filters, SizeKind, SortKey, Subtree, by_type};
use duscope_core::rules::Class;
use duscope_treemap::paint::compact;
use ratatui_core::layout::Rect;

use super::Viewing;
use super::table::{self, Col};
use crate::layout::inner;
use crate::panes::Context;
use crate::widgets::{Canvas, Span, border, percent, thousands};

const SIZE: u16 = 6;
const SHARE: u16 = 13;
const FILES: u16 = 9;
const CLASS: u16 = 14;

/// One class.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    /// The class.
    pub class: Class,
    /// Its bytes in the size the map weighs by.
    pub bytes: u64,
    /// Its share of the root, 0–1.
    pub share: f64,
    /// Its files.
    pub files: u64,
    /// Its largest file's name.
    pub largest: Option<String>,
}

fn class_of(name: &str) -> Class {
    Class::ALL
        .iter()
        .copied()
        .find(|class| class.as_str() == name)
        .unwrap_or(Class::Other)
}

fn row(entry: &ByTypeEntry, size: SizeKind) -> Row {
    Row {
        class: class_of(entry.class),
        bytes: match size {
            SizeKind::Allocated => entry.allocated_bytes,
            SizeKind::Apparent => entry.apparent_bytes,
        },
        share: entry.pct_of_root / 100.0,
        files: entry.file_count,
        largest: entry.largest.first().map(|example| example.name.clone()),
    }
}

/// The classes under `root`, largest first and `other` last, as `by-type`
/// reads them; worked out once.
#[must_use]
pub fn rows(ctx: &Context<'_>, viewing: &Viewing<'_>, root: u32) -> Rc<Vec<Row>> {
    if let Some(found) = viewing
        .rows
        .types
        .borrow()
        .as_ref()
        .filter(|(at, _)| *at == root)
    {
        return Rc::clone(&found.1);
    }
    let query = ByTypeQuery {
        classes: ctx.opts.classes,
        examples: 1,
        filters: Filters::default(),
        limit: None,
        size: ctx.opts.size,
        sort: SortKey::Size,
        reverse: false,
    };
    let view = ctx.view;
    let page = if root == 0 || view.is_empty() {
        by_type(view, &query)
    } else {
        by_type(&Subtree::new(view, root), &query)
    };
    let rows: Rc<Vec<Row>> = Rc::new(page.entries.iter().map(|e| row(e, ctx.opts.size)).collect());
    *viewing.rows.types.borrow_mut() = Some((root, Rc::clone(&rows)));
    rows
}

/// The types view in `pane`, the map's pane; `at` is the row the cursor
/// is on.
pub fn draw(
    canvas: &mut Canvas<'_>,
    pane: Rect,
    ctx: &Context<'_>,
    viewing: &Viewing<'_>,
    at: usize,
    root: u32,
) {
    let title = super::path_title(
        canvas,
        pane,
        "types · ",
        &ctx.path(root),
        &viewing.filter_suffix(),
    );
    canvas.pane(pane, &title, border(canvas.pack, true));
    let area = inner(pane);
    if area.height < 3 {
        return;
    }
    let rows = rows(ctx, viewing, root);
    let at = at.min(rows.len().saturating_sub(1));
    let roles = canvas.pack.roles;
    let pack = canvas.pack;
    let visible = usize::from(area.height) - 2;
    let start = table::window(rows.len(), Some(at), visible);
    let seps = 3 * 4;
    let fixed = CLASS + SIZE + SHARE + FILES + seps;
    let largest = area.width.saturating_sub(fixed).max(1);
    let cols = [
        Col {
            title: "type",
            width: CLASS,
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
        Col {
            title: "largest file",
            width: largest,
            right: false,
        },
    ];
    let rows: Vec<table::Row> = rows[start..]
        .iter()
        .enumerate()
        .take(visible)
        .map(|(i, row)| {
            let color = pack.class(row.class);
            let current = start + i == at;
            let mut bar = canvas.bar(row.share, 8, color);
            bar.push(Span::new(
                format!(" {:>3}%", percent(row.share)),
                roles.subtext,
            ));
            let name = row.largest.clone().unwrap_or_else(|| "—".to_owned());
            table::Row {
                cells: vec![
                    table::name_cell(canvas, row.class.as_str().to_owned(), color, current),
                    vec![Span::new(compact(row.bytes), roles.text)],
                    bar,
                    vec![Span::new(thousands(row.files), roles.subtext)],
                    vec![Span::new(
                        super::one_line(canvas, &name, largest),
                        roles.subtext,
                    )],
                ],
                current,
            }
        })
        .collect();
    table::draw(canvas, area, &cols, &rows);
}
