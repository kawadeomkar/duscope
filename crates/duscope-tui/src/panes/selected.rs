//! The selected pane: the name (bold), the parent's path, then key–value
//! rows: the size in full, the counts, the share of the parent and of the
//! scan as 16-cell bars with percentages, the type, the modification time,
//! and the mode and owner (`—` where the scan did not collect them, a basic
//! scan). Folded under the map, the same facts two to a row, as many as
//! its four rows hold. Each value is the entry's own, or its parent's.

use duscope_core::query::Node;
use duscope_core::rules::{Class, ext};
use duscope_treemap::cells::Color;
use ratatui_core::layout::Rect;

use super::{Context, Row, draw_rows, minute, parent_path};
use crate::widgets::{Canvas, Span, full_size, thousands};

/// The bar of a selected-pane share.
const SELECTED_BAR: u16 = 16;

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
pub(super) fn mode_text(mode: Option<u32>, dir: bool) -> String {
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

/// The mode and owner line: the mode as `ls -l` writes it, then the owner
/// and group as the index holds them, numbers as `ls -n` prints them
/// (`-rw-r--r-- · 501:20`); `—` where the scan did not collect them.
pub(super) fn mode_owner(mode: Option<u32>, owner: Option<(u32, u32)>, dir: bool) -> String {
    let mode_text = mode_text(mode, dir);
    match (mode, owner) {
        (Some(_), Some((uid, gid))) => format!("{mode_text} · {uid}:{gid}"),
        _ => mode_text,
    }
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

/// The selected pane's rows for `id`, one fact a row: what `i` expands.
pub(super) fn selected_rows(canvas: &Canvas<'_>, ctx: &Context<'_>, id: u32) -> Vec<Row> {
    let roles = canvas.pack.roles;
    let node = ctx.view.node(id);
    let parent = ctx.parent(&node);
    let root = ctx.view.node(0);
    let color = ctx.color(&node);
    vec![
        Row::Text(vec![Span::bold(ctx.name(id, false), roles.text)]),
        Row::Text(vec![Span::new(parent_path(ctx, &node), roles.overlay1)]),
        Row::Blank,
        Row::Fact(
            "size",
            vec![Span::new(full_size(ctx.size(&node)), roles.text)],
        ),
        Row::Fact("files", vec![Span::new(counts(&node), roles.text)]),
        Row::Fact(
            "parent",
            share_spans(
                canvas,
                Context::share(ctx.size(&node), ctx.size(&parent)),
                color,
            ),
        ),
        Row::Fact(
            "scan",
            share_spans(
                canvas,
                Context::share(ctx.size(&node), ctx.size(&root)),
                color,
            ),
        ),
        Row::Fact(
            "type",
            vec![
                Span::new("● ", color),
                Span::new(type_text(ctx, &node), roles.text),
            ],
        ),
        Row::Fact(
            "modified",
            vec![Span::new(minute(node.mtime_ns), roles.text)],
        ),
        mode_row(canvas, ctx, id),
    ]
}

/// The mode and owner row of `id`.
pub(super) fn mode_row(canvas: &Canvas<'_>, ctx: &Context<'_>, id: u32) -> Row {
    let node = ctx.view.node(id);
    Row::Fact(
        "mode",
        vec![Span::new(
            mode_owner(node.mode, ctx.view.owner(id), node.is_dir()),
            canvas.pack.roles.text,
        )],
    )
}

/// The selected pane's rows for `id` inside `area`, one fact a row, as many
/// as fit.
pub fn selected(canvas: &mut Canvas<'_>, area: Rect, ctx: &Context<'_>, id: u32) {
    let rows = selected_rows(canvas, ctx, id);
    draw_rows(canvas, area, &rows);
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
