//! What the panes show: the selection's facts, its siblings and the
//! colours' shares, laid out as the canvas draws them.
//!
//! - **selected**: the name (bold), the parent's path, then key–value rows:
//!   the size in full, the counts, the share of the parent and of the scan
//!   as 16-cell bars with percentages, the type, the modification time and
//!   the mode (`—` where the scan did not collect it). Folded under the map,
//!   the same facts in two columns of four rows.
//! - **siblings**: the selection's parent's children, largest first: the
//!   first nine with 6-cell bars, then `… n more`; titled with the parent's
//!   name, size and file count.
//! - **legend**: what the map's colours stand for under `--color-by`, with
//!   8-cell bars: up to eight rows, or seven and `… n more`.
//!
//! Everything is read from the view, as the map is.

use std::cell::RefCell;
use std::cmp::Reverse;
use std::collections::HashMap;

use duscope_core::query::timegrammar::iso8601;
use duscope_core::query::{Node, TreeView, children};
use duscope_core::rules::{Class, ext};
use duscope_treemap::cells::Color;
use duscope_treemap::paint::{
    ColorBy, LsColors, PaintOpts, age_step, compact, dominant, tile_color,
};
use ratatui_core::layout::Rect;

use crate::layout::inner;
use crate::widgets::{Canvas, ListRow, Span, border, full_size, thousands};

/// The siblings listed before `… n more`.
const SIBLINGS: usize = 9;
/// The legend rows shown whole; with more, one fewer and `… n more`.
const LEGEND_ROWS: usize = 8;
/// The bar of a selected-pane share, of a sibling, of a legend row.
const SELECTED_BAR: u16 = 16;
const SIBLING_BAR: u16 = 6;
const LEGEND_BAR: u16 = 8;

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
    fn size(&self, node: &Node) -> u64 {
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
    fn name(&self, id: u32, slash: bool) -> String {
        let mut name = String::from_utf8_lossy(&self.view.name(id)).into_owned();
        if slash && self.view.node(id).is_dir() {
            name.push('/');
        }
        name
    }

    /// The path of folder `id` as the header writes the root's.
    fn path(&self, id: u32) -> String {
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
    fn share(part: u64, whole: u64) -> f64 {
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

/// The type line of `node`: a folder's main class; a file's class with
/// what it holds, or for `other` the extension the classes table does not
/// list.
fn type_text(ctx: &Context<'_>, node: &Node) -> String {
    let class = ctx.class(node);
    if node.is_dir() {
        return format!("folder · mostly {}", class.as_str());
    }
    match class {
        Class::Code => "code · js, py, h, wasm",
        Class::Media => "media · images, video, audio",
        Class::Archives => "archives · tar.gz, dmg, whl",
        Class::Data => "data · db, cache, blobs, json",
        Class::Binaries => "binaries · dylib, .o, frameworks",
        Class::Docs => "documents · logs, mail, text",
        Class::Other => {
            let suffix = ext(&ctx.view.name(node.id));
            return if suffix.is_empty() {
                "other / no extension".to_owned()
            } else {
                format!("other · {suffix}")
            };
        }
    }
    .to_owned()
}

/// The mode as `ls -l` writes it, a folder's with `d`, or `—` where the
/// scan did not collect it.
fn mode_text(mode: Option<u32>, dir: bool) -> String {
    let Some(mode) = mode else {
        return "—".to_owned();
    };
    let mut text = String::from(if dir { "d" } else { "-" });
    for (shift, special, set) in [(6, 0o4000, 's'), (3, 0o2000, 's'), (0, 0o1000, 't')] {
        let bits = (mode >> shift) & 7;
        text.push(if bits & 4 != 0 { 'r' } else { '-' });
        text.push(if bits & 2 != 0 { 'w' } else { '-' });
        text.push(match (bits & 1 != 0, mode & special != 0) {
            (true, true) => set,
            (false, true) => set.to_ascii_uppercase(),
            (true, false) => 'x',
            (false, false) => '-',
        });
    }
    text
}

/// The counts: a file's one, a folder's files and folders below it.
fn counts(node: &Node) -> String {
    if node.is_dir() {
        format!(
            "{} · {} dirs",
            thousands(node.file_count),
            thousands(node.dir_count.saturating_sub(1))
        )
    } else {
        thousands(node.file_count)
    }
}

/// A share of the selected pane: the bar, then the percentage to a tenth.
fn share_spans(canvas: &Canvas<'_>, share: f64, color: Color) -> Vec<Span> {
    let mut spans = canvas.bar(share, SELECTED_BAR, color);
    spans.push(Span::new(
        format!(" {:>5.1}%", share * 100.0),
        canvas.pack.roles.subtext,
    ));
    spans
}

/// The selected pane's rows for `id` inside `area`, one fact a row, as many
/// as fit.
pub fn selected(canvas: &mut Canvas<'_>, area: Rect, ctx: &Context<'_>, id: u32) {
    let roles = canvas.pack.roles;
    let node = ctx.view.node(id);
    let parent = ctx.parent(&node);
    let root = ctx.view.node(0);
    let color = ctx.color(&node);
    let end = area.right();
    let rows: Vec<(Option<&str>, Vec<Span>)> = vec![
        (None, vec![Span::bold(ctx.name(id, false), roles.text)]),
        (
            None,
            vec![Span::new(parent_path(ctx, &node), roles.overlay1)],
        ),
        (None, Vec::new()),
        (
            Some("size"),
            vec![Span::new(full_size(ctx.size(&node)), roles.text)],
        ),
        (Some("files"), vec![Span::new(counts(&node), roles.text)]),
        (
            Some("parent"),
            share_spans(
                canvas,
                Context::share(ctx.size(&node), ctx.size(&parent)),
                color,
            ),
        ),
        (
            Some("scan"),
            share_spans(
                canvas,
                Context::share(ctx.size(&node), ctx.size(&root)),
                color,
            ),
        ),
        (
            Some("type"),
            vec![
                Span::new("● ", color),
                Span::new(type_text(ctx, &node), roles.text),
            ],
        ),
        (
            Some("modified"),
            vec![Span::new(minute(node.mtime_ns), roles.text)],
        ),
        (
            Some("mode"),
            vec![Span::new(mode_text(node.mode, node.is_dir()), roles.text)],
        ),
    ];
    for (row, (label, value)) in rows.iter().enumerate() {
        let Ok(dy) = u16::try_from(row) else { break };
        if dy >= area.height {
            break;
        }
        let y = area.y + dy;
        match label {
            Some(label) => {
                canvas.key_value(area.x, y, end, label, value);
            }
            None => {
                canvas.spans(area.x, y, end, value);
            }
        }
    }
}

/// The selected pane folded under the map: the name and its parent's path,
/// then the facts two to a row.
pub fn folded(canvas: &mut Canvas<'_>, area: Rect, ctx: &Context<'_>, id: u32) {
    let roles = canvas.pack.roles;
    let node = ctx.view.node(id);
    let parent = ctx.parent(&node);
    let root = ctx.view.node(0);
    let color = ctx.color(&node);
    let end = area.right();
    let half = area.x + area.width / 2;
    if area.height == 0 {
        return;
    }
    let at = canvas.spans(
        area.x,
        area.y,
        end,
        &[Span::bold(ctx.name(id, false), roles.text)],
    );
    canvas.spans(
        at.saturating_add(2),
        area.y,
        end,
        &[Span::new(parent_path(ctx, &node), roles.overlay1)],
    );
    let pairs: [[(&str, Vec<Span>); 2]; 3] = [
        [
            (
                "size",
                vec![Span::new(full_size(ctx.size(&node)), roles.text)],
            ),
            ("files", vec![Span::new(counts(&node), roles.text)]),
        ],
        [
            (
                "parent",
                share_spans(
                    canvas,
                    Context::share(ctx.size(&node), ctx.size(&parent)),
                    color,
                ),
            ),
            (
                "scan",
                share_spans(
                    canvas,
                    Context::share(ctx.size(&node), ctx.size(&root)),
                    color,
                ),
            ),
        ],
        [
            (
                "type",
                vec![
                    Span::new("● ", color),
                    Span::new(type_text(ctx, &node), roles.text),
                ],
            ),
            (
                "modified",
                vec![Span::new(minute(node.mtime_ns), roles.text)],
            ),
        ],
    ];
    for (row, [left, right]) in pairs.iter().enumerate() {
        let Ok(dy) = u16::try_from(row + 1) else {
            break;
        };
        if dy >= area.height {
            break;
        }
        let y = area.y + dy;
        canvas.key_value(area.x, y, half.saturating_sub(1), left.0, &left.1);
        canvas.key_value(half, y, end, right.0, &right.1);
    }
}

/// The siblings pane in `pane`, border included: the selection's parent's
/// children, largest first, the selection marked.
pub fn siblings(canvas: &mut Canvas<'_>, pane: Rect, ctx: &Context<'_>, id: u32) {
    let roles = canvas.pack.roles;
    let node = ctx.view.node(id);
    let parent = ctx.parent(&node);
    let title = format!(
        "in {} · {} · {} files",
        ctx.name(parent.id, false),
        full_size(ctx.size(&parent)),
        thousands(parent.file_count)
    );
    canvas.pane(pane, &title, border(canvas.pack, false));
    let area = inner(pane);
    let mut kids: Vec<Node> = if parent.is_dir() {
        children(ctx.view, &parent)
            .map(|k| ctx.view.node(k))
            .collect()
    } else {
        vec![parent]
    };
    kids.sort_by_key(|kid| Reverse(ctx.size(kid)));
    let whole = ctx.size(&parent);
    let name_width = area.width.saturating_sub(23).min(16);
    let shown = list_rows(kids.len(), SIBLINGS, usize::from(area.height));
    for (row, kid) in kids.iter().take(shown).enumerate() {
        let y = area.y + u16::try_from(row).unwrap_or(u16::MAX);
        let (name, size) = (ctx.name(kid.id, true), compact(ctx.size(kid)));
        canvas.list_row(
            area.x,
            y,
            area.right(),
            &ListRow {
                current: Some(kid.id == id),
                color: ctx.color(kid),
                name: &name,
                name_color: None,
                size: &size,
                share: Context::share(ctx.size(kid), whole),
            },
            name_width,
            SIBLING_BAR,
        );
    }
    if shown < kids.len() {
        let bytes: u64 = kids[shown..].iter().map(|k| ctx.size(k)).sum();
        let (label, size) = (format!("… {} more", kids.len() - shown), compact(bytes));
        let y = area.y + u16::try_from(shown).unwrap_or(u16::MAX);
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
    }
}

/// How many of `count` rows a list shows in `room` rows when it shows at
/// most `most` before a `… n more` row: all when they fit, else `most`, or
/// as many as leave room for the `more` row.
fn list_rows(count: usize, most: usize, room: usize) -> usize {
    if count <= most.min(room) {
        count
    } else {
        most.min(room.saturating_sub(1))
    }
}

/// The legend pane in `pane`, border included: `rows`, largest first, under
/// `title`.
pub fn legend(canvas: &mut Canvas<'_>, pane: Rect, title: &str, rows: &[LegendRow], total: u64) {
    let roles = canvas.pack.roles;
    canvas.pane(pane, title, border(canvas.pack, false));
    let area = inner(pane);
    let room = usize::from(area.height);
    let shown = if rows.len() <= LEGEND_ROWS.min(room) {
        rows.len()
    } else {
        LEGEND_ROWS.min(room).saturating_sub(1)
    };
    let draw = |canvas: &mut Canvas<'_>, row: usize, label: &str, bytes: u64, color: Color| {
        let y = area.y + u16::try_from(row).unwrap_or(u16::MAX);
        let size = compact(bytes);
        canvas.list_row(
            area.x,
            y,
            area.right(),
            &ListRow {
                current: None,
                color,
                name: label,
                name_color: None,
                size: &size,
                share: Context::share(bytes, total),
            },
            10,
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

/// The legend's title and rows for the colours `opts` paints with,
/// largest first: the classes' bytes for `type`; the files' `LS_COLORS`
/// colours (the class colour where it gives none) for `lscolors`; the root's
/// tiles' bytes for `folder`; the bytes at each depth for `level` and at
/// each age for `age`, a file below the `--depth` cap counted with the
/// folder the map draws whole.
pub fn legend_rows(ctx: &Context<'_>) -> (&'static str, Vec<LegendRow>) {
    let pack = ctx.opts.pack;
    let view = ctx.view;
    let size = ctx.opts.size;
    let files = || {
        (0..view.len())
            .map(|id| view.node(id))
            .filter(|n| !n.is_dir())
    };
    // The entry whose tile holds a file: the file, or above the `--depth`
    // cap the folder drawn as one tile.
    let cap = ctx.opts.depth.map(|d| d.max(1));
    let shown = |node: Node| {
        let mut at = node;
        while cap.is_some_and(|cap| at.depth > cap) {
            at = view.node(at.parent);
        }
        at
    };
    // Bytes by label, in the order labels first appear.
    let mut tally: Vec<LegendRow> = Vec::new();
    let mut add = |label: String, color: Color, bytes: u64| {
        if let Some(row) = tally.iter_mut().find(|r| r.label == label) {
            row.bytes += bytes;
        } else {
            tally.push(LegendRow {
                label,
                bytes,
                color,
            });
        }
    };
    let title = match &ctx.opts.color_by {
        ColorBy::Type => {
            for node in files() {
                let class = ctx.class(&node);
                add(
                    class.as_str().to_owned(),
                    pack.class(class),
                    node.counted(size),
                );
            }
            "by type"
        }
        ColorBy::LsColors(table) => {
            for node in files() {
                let (label, color) =
                    ls_key(table, &view.name(node.id), node.mode).unwrap_or_else(|| {
                        let class = ctx.class(&node);
                        (class.as_str().to_owned(), pack.class(class))
                    });
                add(label, color, node.counted(size));
            }
            "by LS_COLORS"
        }
        ColorBy::Folder => {
            tally = folder_rows(ctx);
            "by top folder"
        }
        ColorBy::Level => {
            for node in files() {
                let depth = shown(node).depth;
                add(
                    format!("depth {depth}"),
                    pack.cycle[depth as usize % pack.cycle.len()],
                    node.counted(size),
                );
            }
            "by depth"
        }
        ColorBy::Age => {
            const AGES: [&str; 5] = ["< 1 day", "< 1 week", "< 1 month", "< 1 year", "older"];
            for node in files() {
                let step = age_step(ctx.opts.now, shown(node).max_mtime_ns);
                add(AGES[step].to_owned(), pack.age[step], node.counted(size));
            }
            "by age"
        }
    };
    let mut rows: Vec<LegendRow> = tally.into_iter().filter(|r| r.bytes > 0).collect();
    // Stable: equal sizes keep their order.
    rows.sort_by_key(|row| Reverse(row.bytes));
    (title, rows)
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

/// The path of `node`'s parent, as the header writes the root's; for the
/// root, the folder it is in.
fn parent_path(ctx: &Context<'_>, node: &Node) -> String {
    if node.id == 0 {
        return ctx.root_parent.to_owned();
    }
    ctx.path(node.parent)
}

/// A time to the minute, in UTC: `2026-10-21 23:14`.
fn minute(ns: i64) -> String {
    let iso = iso8601(ns);
    iso.get(..16).unwrap_or(&iso).replace('T', " ")
}

#[cfg(test)]
mod tests;
