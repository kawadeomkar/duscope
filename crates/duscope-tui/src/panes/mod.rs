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

use std::collections::HashMap;

use duscope_core::query::timegrammar::iso8601;
use duscope_core::query::{Node, TreeView, children};
use duscope_core::rules::{Class, ext};
use duscope_treemap::cells::Color;
use duscope_treemap::paint::{ColorBy, PaintOpts, age_step, compact, dominant, tile_color};
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
    /// Each top-level folder's place among the root's children, by size:
    /// the index of its `--color-by folder` colour.
    pub ranks: &'a HashMap<u32, usize>,
    /// The root's path as the header shows it (`~/Library`).
    pub root: &'a str,
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
            dominant(self.view, self.class_of, self.opts.size, node.id, false)
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
            .then(|| self.ranks.get(&top.id).copied())
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
    kids.sort_by_key(|kid| std::cmp::Reverse(ctx.size(kid)));
    let whole = ctx.size(&parent);
    let rows: Vec<(ListRow<'_>, String, String)> = kids
        .iter()
        .map(|kid| {
            (
                ListRow {
                    current: Some(kid.id == id),
                    color: ctx.color(kid),
                    name: "",
                    name_color: None,
                    size: "",
                    share: Context::share(ctx.size(kid), whole),
                },
                ctx.name(kid.id, true),
                compact(ctx.size(kid)),
            )
        })
        .collect();
    let name_width = area.width.saturating_sub(23).min(16);
    let shown = list_rows(rows.len(), SIBLINGS, usize::from(area.height));
    for (row, (list, name, size)) in rows.iter().take(shown).enumerate() {
        let y = area.y + u16::try_from(row).unwrap_or(u16::MAX);
        canvas.list_row(
            area.x,
            y,
            area.right(),
            &ListRow {
                name,
                size,
                ..list.clone()
            },
            name_width,
            SIBLING_BAR,
        );
    }
    if shown < rows.len() {
        let bytes: u64 = kids[shown..].iter().map(|k| ctx.size(k)).sum();
        let (label, size, share) = (
            format!("… {} more", kids.len() - shown),
            compact(bytes),
            Context::share(bytes, whole),
        );
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
                share,
            },
            name_width,
            SIBLING_BAR,
        );
    }
}

/// How many of `count` rows a list shows in `room` rows when it shows at
/// most `most` before a `… n more` row: all that fit, or one fewer and the
/// `more` row.
fn list_rows(count: usize, most: usize, room: usize) -> usize {
    let fit = most.min(room);
    if count <= fit {
        count
    } else {
        fit.saturating_sub(1)
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

/// The legend's title and rows for the colours `opts` paints with: the
/// classes' bytes for `type` and `lscolors`, the top-level folders' for
/// `folder`, the bytes at each depth for `level`, at each age for `age`;
/// largest first.
pub fn legend_rows(ctx: &Context<'_>) -> (&'static str, Vec<LegendRow>) {
    let pack = ctx.opts.pack;
    let view = ctx.view;
    let files = || {
        (0..view.len())
            .map(|id| view.node(id))
            .filter(|n| !n.is_dir())
    };
    let size = ctx.opts.size;
    let (title, mut rows): (&'static str, Vec<LegendRow>) = match &ctx.opts.color_by {
        ColorBy::Type | ColorBy::LsColors(_) => {
            let mut bytes = [0u64; 7];
            for node in files() {
                let class = ctx.class(&node);
                let i = Class::ALL.iter().position(|&c| c == class).unwrap_or(6);
                bytes[i] += node.counted(size);
            }
            let rows = Class::ALL
                .iter()
                .zip(bytes)
                .filter(|&(_, b)| b > 0)
                .map(|(&class, bytes)| LegendRow {
                    label: class.as_str().to_owned(),
                    bytes,
                    color: pack.class(class),
                })
                .collect();
            ("by type", rows)
        }
        ColorBy::Folder => {
            let root = view.node(0);
            let mut rows: Vec<(usize, LegendRow)> = if root.is_dir() {
                children(view, &root)
                    .map(|id| {
                        let node = view.node(id);
                        let rank = ctx.ranks.get(&id).copied().unwrap_or(0);
                        (
                            rank,
                            LegendRow {
                                label: String::from_utf8_lossy(&view.name(id)).into_owned(),
                                bytes: node.counted(size),
                                color: pack.cycle[rank % pack.cycle.len()],
                            },
                        )
                    })
                    .collect()
            } else {
                Vec::new()
            };
            rows.sort_by_key(|&(rank, _)| rank);
            ("by top folder", rows.into_iter().map(|(_, r)| r).collect())
        }
        ColorBy::Level => {
            let mut bytes: Vec<u64> = Vec::new();
            for node in files() {
                let depth = node.depth as usize;
                if bytes.len() <= depth {
                    bytes.resize(depth + 1, 0);
                }
                bytes[depth] += node.counted(size);
            }
            let rows = bytes
                .iter()
                .enumerate()
                .filter(|&(_, &b)| b > 0)
                .map(|(depth, &bytes)| LegendRow {
                    label: format!("depth {depth}"),
                    bytes,
                    color: pack.cycle[depth % pack.cycle.len()],
                })
                .collect();
            ("by depth", rows)
        }
        ColorBy::Age => {
            const AGES: [&str; 5] = ["< 1 day", "< 1 week", "< 1 month", "< 1 year", "older"];
            let mut bytes = [0u64; 5];
            for node in files() {
                bytes[age_step(ctx.opts.now, node.max_mtime_ns)] += node.counted(size);
            }
            let rows = AGES
                .iter()
                .zip(bytes)
                .enumerate()
                .filter(|&(_, (_, b))| b > 0)
                .map(|(step, (label, bytes))| LegendRow {
                    label: (*label).to_owned(),
                    bytes,
                    color: pack.age[step],
                })
                .collect();
            ("by age", rows)
        }
    };
    // Stable: equal sizes keep their order.
    rows.sort_by_key(|row| std::cmp::Reverse(row.bytes));
    (title, rows)
}

/// Each top-level folder's place among the root's children by the size
/// the map weighs them by, largest first, equals in their order: the order
/// the map lays them out in, and so the index of each one's `--color-by
/// folder` colour.
pub fn ranks(view: &dyn TreeView, opts: &PaintOpts<'_>) -> HashMap<u32, usize> {
    let root = view.node(0);
    if view.is_empty() || !root.is_dir() {
        return HashMap::new();
    }
    let mut kids: Vec<(u64, u32)> = children(view, &root)
        .map(|id| (view.node(id).counted(opts.size), id))
        .collect();
    kids.sort_by_key(|&(bytes, _)| std::cmp::Reverse(bytes));
    kids.iter()
        .enumerate()
        .map(|(rank, &(_, id))| (id, rank))
        .collect()
}

/// The path of `node`'s parent, as the header writes the root's; for the
/// root, the folder it is in.
fn parent_path(ctx: &Context<'_>, node: &Node) -> String {
    if node.id == 0 {
        return match ctx.root.rsplit_once('/') {
            Some(("", _)) => "/".to_owned(),
            Some((parent, _)) => parent.to_owned(),
            None => ctx.root.to_owned(),
        };
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
