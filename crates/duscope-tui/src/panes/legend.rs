//! The legend pane: what the map's colours stand for under `--color-by`,
//! largest first, with 8-cell bars: the top seven and `… n more` for the
//! rest, or all eight where there are eight. The rows are worked out when
//! the viewer opens and when `c` or `p` change the colours. `⇥` focuses
//! it: the arrows then pick a row, and `⏎` keeps that row's tiles on the
//! map ([`crate::mask`]). A row counts files by [`legend_label`]. The
//! viewer's first rows are tallied in the pass that classifies the scan
//! ([`Tally`]), the rows of a colour mode or pack chosen later in a pass of
//! their own ([`legend_rows`]).

use std::cmp::Reverse;

use duscope_core::query::{Node, TreeView, children};
use duscope_core::rules::Class;
use duscope_treemap::cells::Color;
use duscope_treemap::paint::{ColorBy, LsColors, PaintOpts, age_step, compact};
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
/// folder the map draws whole. One pass over the scan, but for `folder`.
pub fn legend_rows(ctx: &Context<'_>) -> (&'static str, Vec<LegendRow>) {
    let title = legend_title(ctx.opts);
    if matches!(ctx.opts.color_by, ColorBy::Folder) {
        return (title, largest_first(folder_rows(ctx)));
    }
    let view = ctx.view;
    let mut tally = Tally::new(view, ctx.opts);
    for id in 0..view.len() {
        let node = view.node(id);
        // A folder's own class is not counted: the tally reads folders
        // only to know which a file is in.
        let class = if node.is_dir() {
            Class::Other
        } else {
            ctx.class(&node)
        };
        tally.add(&node, class);
    }
    (title, tally.rows())
}

/// The legend's title for the colours `opts` paints with.
#[must_use]
pub fn legend_title(opts: &PaintOpts<'_>) -> &'static str {
    match &opts.color_by {
        ColorBy::Type => "by type",
        ColorBy::LsColors(_) => "by LS_COLORS",
        ColorBy::Folder => "by top folder",
        ColorBy::Level => "by depth",
        ColorBy::Age => "by age",
    }
}

/// The legend's rows, tallied as the scan's entries are read in id order,
/// each with its class: the bytes of each row, in the order rows first
/// appear, as [`legend_rows`] counts them. The viewer feeds it from the
/// pass that classifies the scan, so that its first legend reads nothing
/// more. Under `--color-by folder` it counts nothing: those rows are the
/// root's tiles ([`legend_rows`]).
pub struct Tally<'t> {
    view: &'t dyn TreeView,
    opts: &'t PaintOpts<'t>,
    /// The folders above the entry read last, the root first: where the
    /// `--depth` cap cuts a file's path, read without a lookup.
    open: Vec<Node>,
    /// Each row's key, bytes and colour, in the order rows first appear.
    rows: Vec<(Key, u64, Color)>,
}

impl std::fmt::Debug for Tally<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tally")
            .field("rows", &self.rows.len())
            .finish_non_exhaustive()
    }
}

impl<'t> Tally<'t> {
    /// An empty tally for the colours `opts` paints `view` with.
    #[must_use]
    pub fn new(view: &'t dyn TreeView, opts: &'t PaintOpts<'t>) -> Tally<'t> {
        Tally {
            view,
            opts,
            open: Vec::new(),
            rows: Vec::new(),
        }
    }

    /// Counts the next entry, `class` its class; the entries come in id
    /// order, folders included.
    pub fn add(&mut self, node: &Node, class: Class) {
        while self.open.last().is_some_and(|dir| dir.last_id < node.id) {
            self.open.pop();
        }
        if node.is_dir() {
            self.open.push(*node);
            return;
        }
        let Some((key, color)) = entry(self.view, self.opts, node, || class, || self.shown(node))
        else {
            return;
        };
        let bytes = node.counted(self.opts.size);
        if let Some(row) = self.rows.iter_mut().find(|row| row.0 == key) {
            row.1 += bytes;
        } else {
            self.rows.push((key, bytes, color));
        }
    }

    /// The rows counted, largest first, those of no bytes left out.
    #[must_use]
    pub fn rows(self) -> Vec<LegendRow> {
        largest_first(
            self.rows
                .into_iter()
                .map(|(key, bytes, color)| LegendRow {
                    label: key.label(),
                    bytes,
                    color,
                })
                .collect(),
        )
    }

    /// The entry whose tile holds file `node`: the file, or below the
    /// `--depth` cap the folder at the cap, drawn whole.
    fn shown(&self, node: &Node) -> Node {
        match self.opts.depth.map(|d| d.max(1)) {
            Some(cap) if node.depth > cap => self
                .open
                .iter()
                .find(|dir| dir.depth == cap)
                .copied()
                .unwrap_or_else(|| climb(self.view, cap, node)),
            _ => *node,
        }
    }
}

/// `rows` with bytes, largest first; equal sizes keep their order.
fn largest_first(rows: Vec<LegendRow>) -> Vec<LegendRow> {
    let mut rows: Vec<LegendRow> = rows.into_iter().filter(|r| r.bytes > 0).collect();
    rows.sort_by_key(|row| Reverse(row.bytes));
    rows
}

/// What a legend row stands for: the label it is shown with.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Key {
    /// A class (`type`, and `lscolors` where `LS_COLORS` gives no colour).
    Class(Class),
    /// An `LS_COLORS` entry, as it is shown: `executable` or `*suffix`.
    Ls(String),
    /// A depth (`level`).
    Depth(u32),
    /// An age step (`age`).
    Age(usize),
}

impl Key {
    /// The row's label.
    fn label(&self) -> String {
        const AGES: [&str; 5] = ["< 1 day", "< 1 week", "< 1 month", "< 1 year", "older"];
        match self {
            Key::Class(class) => class.as_str().to_owned(),
            Key::Ls(label) => label.clone(),
            Key::Depth(depth) => format!("depth {depth}"),
            Key::Age(step) => AGES[*step].to_owned(),
        }
    }
}

/// The legend row file `node` counts under, by the colours `opts` paints
/// `view` with, given its `class` and the entry whose tile holds it
/// (`shown`): the row's key and colour. `None` under `--color-by folder`,
/// whose rows are the root's tiles ([`legend_label`]).
fn entry(
    view: &dyn TreeView,
    opts: &PaintOpts<'_>,
    node: &Node,
    class: impl FnOnce() -> Class,
    shown: impl FnOnce() -> Node,
) -> Option<(Key, Color)> {
    let pack = opts.pack;
    Some(match &opts.color_by {
        ColorBy::Type => {
            let class = class();
            (Key::Class(class), pack.class(class))
        }
        ColorBy::LsColors(table) => {
            if let Some((label, color)) = ls_key(table, &view.name(node.id), node.mode) {
                (Key::Ls(label), color)
            } else {
                let class = class();
                (Key::Class(class), pack.class(class))
            }
        }
        ColorBy::Level => {
            let depth = shown().depth;
            (
                Key::Depth(depth),
                pack.cycle[depth as usize % pack.cycle.len()],
            )
        }
        ColorBy::Age => {
            let step = age_step(opts.now, shown().max_mtime_ns);
            (Key::Age(step), pack.age[step])
        }
        ColorBy::Folder => return None,
    })
}

/// The ancestor of `node` at depth `cap` (or the first one at or above
/// it), read up its path.
fn climb(view: &dyn TreeView, cap: u32, node: &Node) -> Node {
    let mut at = *node;
    while at.depth > cap {
        at = view.node(at.parent);
    }
    at
}

/// The label of the legend row file `node` counts under, by the colours
/// `ctx.opts` paints with: under `--color-by folder` its top-level folder's
/// name, or `files` for the root's files pooled under `--pool-files`.
#[must_use]
pub fn legend_label(ctx: &Context<'_>, node: &Node) -> String {
    let shown = || match ctx.opts.depth.map(|d| d.max(1)) {
        Some(cap) => climb(ctx.view, cap, node),
        None => *node,
    };
    if let Some((key, _)) = entry(ctx.view, ctx.opts, node, || ctx.class(node), shown) {
        return key.label();
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
        let class = ctx.dominants(0).own;
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
