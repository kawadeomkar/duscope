//! What the panes show: the selection's facts, its siblings and the
//! colours' shares, laid out as the canvas draws them; and, on `i`, the
//! selection's full fact list.
//!
//! - **selected** ([`selected`]): the name, the parent's path, the size in
//!   full, the counts, the shares of the parent and of the scan, the type,
//!   the modification time, the mode and owner.
//! - **siblings** ([`siblings`]): the selection's parent's children,
//!   largest first or in the `s` sort, the nine around the selection; `⇥`
//!   moves the arrows onto it.
//! - **legend** ([`legend`]): what the map's colours stand for; `⇥` moves
//!   the arrows onto it, and `⏎` keeps the map's tiles of a row.
//! - **facts** ([`facts`]): what `i` adds, from the scan and `explain`, in
//!   the selected pane grown over the right column, or over the map when
//!   the column is folded under it.
//!
//! Everything is read from the view, as the map is, each value from the
//! entry itself or its children; `i` adds `explain` of the saved scan.

use std::cell::RefCell;
use std::collections::HashMap;

use duscope_core::query::timegrammar::iso8601;
use duscope_core::query::{Node, TreeView};
use duscope_core::rules::Class;
use duscope_treemap::cells::Color;
use duscope_treemap::paint::{PaintOpts, dominant, tile_color};
use ratatui_core::layout::Rect;

use crate::widgets::{Canvas, Span};

pub(crate) mod facts;
mod legend;
mod selected;
mod siblings;

pub use facts::{Facts, ScanFacts, Sources};
pub use legend::{legend, legend_label, legend_rows, ranks, shown_rows};
#[cfg(test)]
use selected::mode_text;
pub use selected::{folded, selected};
#[cfg(test)]
use siblings::list_rows;
pub use siblings::{siblings, siblings_row};

/// What the panes read: the scan, how it is painted, and what the viewer
/// worked out once.
pub struct Context<'a> {
    /// The scan.
    pub view: &'a dyn TreeView,
    /// How the map is painted.
    pub opts: &'a PaintOpts<'a>,
    /// Each entry's class, by id.
    pub class_of: &'a [Class],
    /// The class of most of each folder's bytes, worked out the first time
    /// a pane shows the folder: the scan does not change.
    pub dominant: &'a RefCell<HashMap<u32, Class>>,
    /// The `--color-by folder` places of the root's tiles.
    pub ranks: &'a Ranks,
    /// The root's path as the header shows it (`~/Library`).
    pub root: &'a str,
    /// The folder the root is in, written the same way.
    pub root_parent: &'a str,
}

/// The places of the root's tiles in the order the map lays them out,
/// largest first: each one's `--color-by folder` colour.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ranks {
    /// A child of the root with a tile of its own, by id.
    pub by_id: HashMap<u32, usize>,
    /// Under `--pool-files`, the one tile of the root's files.
    pub files: Option<usize>,
}

/// One row of the legend.
#[derive(Debug, Clone, PartialEq)]
pub struct LegendRow {
    /// What the colour stands for.
    pub label: String,
    /// The bytes it covers.
    pub bytes: u64,
    /// The colour.
    pub color: Color,
}

impl Context<'_> {
    /// The size the map weighs `node` by.
    pub(crate) fn size(&self, node: &Node) -> u64 {
        node.size(self.opts.size)
    }

    /// A file's class, or the class of most of a folder's bytes.
    pub fn class(&self, node: &Node) -> Class {
        if node.is_dir() {
            *self
                .dominant
                .borrow_mut()
                .entry(node.id)
                .or_insert_with(|| {
                    dominant(self.view, self.class_of, self.opts.size, node.id, false)
                })
        } else {
            self.class_of
                .get(node.id as usize)
                .copied()
                .unwrap_or(Class::Other)
        }
    }

    /// The colour of `node`'s tile, as the map paints it untinted.
    pub fn color(&self, node: &Node) -> Color {
        let mut top = *node;
        while top.depth > 1 {
            top = self.view.node(top.parent);
        }
        let folder = (top.depth == 1)
            .then(|| self.ranks.by_id.get(&top.id).copied())
            .flatten();
        tile_color(
            self.view,
            self.opts,
            node,
            self.class(node),
            node.depth,
            folder,
        )
    }

    /// The name of `id`, a folder's ending in `/` when `slash`.
    pub(crate) fn name(&self, id: u32, slash: bool) -> String {
        let mut name = String::from_utf8_lossy(&self.view.name(id)).into_owned();
        if slash && self.view.node(id).is_dir() {
            name.push('/');
        }
        name
    }

    /// The path of `id` as the header writes the root's.
    pub(crate) fn path(&self, id: u32) -> String {
        let rel = String::from_utf8_lossy(&self.view.path(id)).into_owned();
        match (rel.is_empty(), self.root) {
            (true, root) => root.to_owned(),
            (false, "/") => format!("/{rel}"),
            (false, root) => format!("{root}/{rel}"),
        }
    }

    /// The selection's parent: the root for the root.
    fn parent(&self, node: &Node) -> Node {
        if node.id == 0 {
            *node
        } else {
            self.view.node(node.parent)
        }
    }

    /// `part` of `whole`, 0–1.
    pub(crate) fn share(part: u64, whole: u64) -> f64 {
        if whole == 0 {
            0.0
        } else {
            // Byte counts as f64: exact far beyond any disk.
            #[allow(clippy::cast_precision_loss)]
            let share = part as f64 / whole as f64;
            share
        }
    }
}

/// One row of the selected pane, or of the facts.
#[derive(Debug, Clone)]
pub(crate) enum Row {
    /// Text across the row.
    Text(Vec<Span>),
    /// A key–value row: the label in its 10-column field, then the value.
    Fact(&'static str, Vec<Span>),
    /// An empty row.
    Blank,
}

/// Draws `rows` down `area`, one a row, as many as fit.
pub(crate) fn draw_rows(canvas: &mut Canvas<'_>, area: Rect, rows: &[Row]) {
    let end = area.right();
    for (dy, row) in (0..area.height).zip(rows) {
        let y = area.y + dy;
        match row {
            Row::Fact(label, value) => {
                canvas.key_value(area.x, y, end, label, value);
            }
            Row::Text(value) => {
                canvas.spans(area.x, y, end, value);
            }
            Row::Blank => {}
        }
    }
}

/// The path of `node`'s parent, as the header writes the root's; for the
/// root, the folder it is in.
pub(crate) fn parent_path(ctx: &Context<'_>, node: &Node) -> String {
    if node.id == 0 {
        return ctx.root_parent.to_owned();
    }
    ctx.path(node.parent)
}

/// A time to the minute, in UTC: `2026-10-21 23:14`.
pub(crate) fn minute(ns: i64) -> String {
    let iso = iso8601(ns);
    iso.get(..16).unwrap_or(&iso).replace('T', " ")
}

#[cfg(test)]
pub(crate) mod tests;
