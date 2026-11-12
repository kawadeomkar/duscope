//! The legend pane: what the map's colours stand for under `--color-by`,
//! largest first, with 8-cell bars: the top seven and `… n more` for the
//! rest, or all eight where there are eight. The rows are worked out when
//! the viewer opens and when `c` or `p` change the colours. `⇥` focuses
//! it: the arrows then pick a row, and `⏎` keeps that row's tiles on the
//! map ([`crate::mask`]). A row counts files by [`legend_label`].

use std::cmp::Reverse;

use duscope_core::query::{Node, TreeView, children};
use duscope_treemap::cells::Color;
use duscope_treemap::paint::{ColorBy, LsColors, PaintOpts, age_step, compact, dominant};
use ratatui_core::layout::Rect;

use super::{Context, LegendRow, Ranks};
use crate::layout::inner;
use crate::widgets::{Canvas, ListRow, border};

/// The legend rows shown whole; with more, one fewer and `… n more`.
const LEGEND_ROWS: usize = 8;
/// The bar of a legend row.
const LEGEND_BAR: u16 = 8;

/// The legend pane in `pane`, border included: `rows`, largest first,
/// under `title`; with `selected` (the pane focused) the accent border and
/// a `▸` marker on that row.
pub fn legend(
    canvas: &mut Canvas<'_>,
    pane: Rect,
    title: &str,
    rows: &[LegendRow],
    total: u64,
    selected: Option<usize>,
) {
    let roles = canvas.pack.roles;
    let focused = selected.is_some();
    let frame = if focused {
        roles.accent
    } else {
        border(canvas.pack, false)
    };
    canvas.pane(pane, title, frame);
    let area = inner(pane);
    let room = usize::from(area.height);
    let shown = shown_rows(rows.len(), room);
    // The focused pane gives up two columns of name to its marker.
    let name_width = if focused { 8 } else { 10 };
    let draw = |canvas: &mut Canvas<'_>, row: usize, label: &str, bytes: u64, color: Color| {
        let y = area.y + u16::try_from(row).unwrap_or(u16::MAX);
        let size = compact(bytes);
        canvas.list_row(
            area.x,
            y,
            area.right(),
            &ListRow {
                current: focused.then_some(selected == Some(row)),
                color,
                name: label,
                name_color: None,
                size: &size,
                share: Context::share(bytes, total),
            },
            name_width,
            LEGEND_BAR,
        );
    };
    for (row, entry) in rows.iter().take(shown).enumerate() {
        draw(canvas, row, &entry.label, entry.bytes, entry.color);
    }
    if shown < rows.len() {
        let bytes = rows[shown..].iter().map(|r| r.bytes).sum();
        let label = format!("… {} more", rows.len() - shown);
        draw(canvas, shown, &label, bytes, roles.overlay1);
    }
}

/// How many of `count` legend rows are listed in `room` rows, before a
/// `… n more` row for the rest: all when they fit.
#[must_use]
pub fn shown_rows(count: usize, room: usize) -> usize {
    if count <= LEGEND_ROWS.min(room) {
        count
    } else {
        LEGEND_ROWS.min(room).saturating_sub(1)
    }
}

/// The legend's title and rows for the colours `opts` paints with,
/// largest first: the classes' bytes for `type`; the files' `LS_COLORS`
/// colours (the class colour where it gives none) for `lscolors`; the root's
/// tiles' bytes for `folder`; the bytes at each depth for `level` and at
/// each age for `age`, a file below the `--depth` cap counted with the
/// folder the map draws whole.
pub fn legend_rows(ctx: &Context<'_>) -> (&'static str, Vec<LegendRow>) {
    let view = ctx.view;
    let size = ctx.opts.size;
    let title = match &ctx.opts.color_by {
        ColorBy::Type => "by type",
        ColorBy::LsColors(_) => "by LS_COLORS",
        ColorBy::Folder => "by top folder",
        ColorBy::Level => "by depth",
        ColorBy::Age => "by age",
    };
    // Bytes by label, in the order labels first appear.
    let mut tally: Vec<LegendRow> = Vec::new();
    if matches!(ctx.opts.color_by, ColorBy::Folder) {
        tally = folder_rows(ctx);
    } else {
        for node in (0..view.len())
            .map(|id| view.node(id))
            .filter(|n| !n.is_dir())
        {
            let Some((label, color)) = entry(ctx, &node) else {
                continue;
            };
            let bytes = node.counted(size);
            if let Some(row) = tally.iter_mut().find(|r| r.label == label) {
                row.bytes += bytes;
            } else {
                tally.push(LegendRow {
                    label,
                    bytes,
                    color,
                });
            }
        }
    }
    let mut rows: Vec<LegendRow> = tally.into_iter().filter(|r| r.bytes > 0).collect();
    // Stable: equal sizes keep their order.
    rows.sort_by_key(|row| Reverse(row.bytes));
    (title, rows)
}

/// The legend row file `node` counts under, by the colours `ctx.opts`
/// paints with: the row's label and colour. `None` under `--color-by
/// folder`, whose rows are the root's tiles ([`legend_label`]).
fn entry(ctx: &Context<'_>, node: &Node) -> Option<(String, Color)> {
    let (pack, view) = (ctx.opts.pack, ctx.view);
    // The entry whose tile holds the file: the file, or above the `--depth`
    // cap the folder drawn as one tile.
    let shown = |node: &Node| {
        let cap = ctx.opts.depth.map(|d| d.max(1));
        let mut at = *node;
        while cap.is_some_and(|cap| at.depth > cap) {
            at = view.node(at.parent);
        }
        at
    };
    Some(match &ctx.opts.color_by {
        ColorBy::Type => {
            let class = ctx.class(node);
            (class.as_str().to_owned(), pack.class(class))
        }
        ColorBy::LsColors(table) => {
            ls_key(table, &view.name(node.id), node.mode).unwrap_or_else(|| {
                let class = ctx.class(node);
                (class.as_str().to_owned(), pack.class(class))
            })
        }
        ColorBy::Level => {
            let depth = shown(node).depth;
            (
                format!("depth {depth}"),
                pack.cycle[depth as usize % pack.cycle.len()],
            )
        }
        ColorBy::Age => {
            const AGES: [&str; 5] = ["< 1 day", "< 1 week", "< 1 month", "< 1 year", "older"];
            let step = age_step(ctx.opts.now, shown(node).max_mtime_ns);
            (AGES[step].to_owned(), pack.age[step])
        }
        ColorBy::Folder => return None,
    })
}

/// The label of the legend row file `node` counts under, by the colours
/// `ctx.opts` paints with: under `--color-by folder` its top-level folder's
/// name, or `files` for the root's files pooled under `--pool-files`.
#[must_use]
pub fn legend_label(ctx: &Context<'_>, node: &Node) -> String {
    if let Some((label, _)) = entry(ctx, node) {
        return label;
    }
    let view = ctx.view;
    let mut top = *node;
    while top.depth > 1 {
        top = view.node(top.parent);
    }
    if top.depth == 1 && ctx.ranks.by_id.contains_key(&top.id) {
        String::from_utf8_lossy(&view.name(top.id)).into_owned()
    } else {
        "files".to_owned()
    }
}

/// The legend's rows under `--color-by folder`: the root's tiles in the
/// order the map lays them out, each in its colour of the cycle; under
/// `--pool-files` the root's files as one tile in the colour of most of
/// their bytes.
fn folder_rows(ctx: &Context<'_>) -> Vec<LegendRow> {
    let (pack, view, size) = (ctx.opts.pack, ctx.view, ctx.opts.size);
    let root = view.node(0);
    let mut ranked: Vec<(usize, LegendRow)> = Vec::new();
    let mut pooled = 0u64;
    if root.is_dir() {
        for id in children(view, &root) {
            let node = view.node(id);
            if let Some(&rank) = ctx.ranks.by_id.get(&id) {
                ranked.push((
                    rank,
                    LegendRow {
                        label: String::from_utf8_lossy(&view.name(id)).into_owned(),
                        bytes: node.counted(size),
                        color: pack.cycle[rank % pack.cycle.len()],
                    },
                ));
            } else if !node.is_dir() {
                pooled += node.counted(size);
            }
        }
    }
    if let Some(rank) = ctx.ranks.files {
        let class = dominant(view, ctx.class_of, size, 0, true);
        ranked.push((
            rank,
            LegendRow {
                label: "files".to_owned(),
                bytes: pooled,
                color: pack.class(class),
            },
        ));
    }
    ranked.sort_by_key(|&(rank, _)| rank);
    ranked.into_iter().map(|(_, row)| row).collect()
}

/// The `LS_COLORS` entry that colours a file named `name` with `mode`, as
/// [`LsColors::of`] picks it: `ex` for an executable, else the last suffix
/// it ends with; with its colour.
fn ls_key(table: &LsColors, name: &[u8], mode: Option<u32>) -> Option<(String, Color)> {
    if let Some(color) = table.executable
        && mode.is_some_and(|m| m & 0o111 != 0)
    {
        return Some(("executable".to_owned(), color));
    }
    let name = name.to_ascii_lowercase();
    table
        .suffixes
        .iter()
        .rev()
        .find(|(suffix, _)| name.ends_with(suffix.as_bytes()))
        .map(|(suffix, color)| (format!("*{suffix}"), *color))
}

/// The places of the root's tiles as the map lays them out: its children,
/// under `--pool-files` its folders and one tile for its files, by the size
/// the map weighs them by, largest first, equals in their order.
pub fn ranks(view: &dyn TreeView, opts: &PaintOpts<'_>) -> Ranks {
    let root = view.node(0);
    if view.is_empty() || !root.is_dir() {
        return Ranks::default();
    }
    let mut items: Vec<(u64, Option<u32>)> = Vec::new();
    let (mut pooled, mut any) = (0u64, false);
    for id in children(view, &root) {
        let node = view.node(id);
        if opts.pool_files && !node.is_dir() {
            pooled += node.counted(opts.size);
            any = true;
        } else {
            items.push((node.counted(opts.size), Some(id)));
        }
    }
    if any {
        items.push((pooled, None));
    }
    items.sort_by_key(|&(bytes, _)| Reverse(bytes));
    let mut ranks = Ranks::default();
    for (rank, &(_, id)) in items.iter().enumerate() {
        match id {
            Some(id) => {
                ranks.by_id.insert(id, rank);
            }
            None => ranks.files = Some(rank),
        }
    }
    ranks
}
