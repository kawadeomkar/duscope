//! What `i` shows: the selected pane grown into the full fact list. Beside
//! the column it takes the siblings' and the legend's rows; folded under
//! the map, the facts open over the map in a pane of their own, the rows
//! the folded pane already shows left out.
//!
//! The list is the selected pane's rows, then what `i` adds, then the
//! scan's. Each value is the entry's own, its children's, or `explain`'s
//! answer for it ([`duscope_core::query::explain`]): of the saved scan the
//! viewer reads, or the one a scan run for it was saved as, read once per
//! entry; for a scan not saved, what a fresh `stat` tells, as `explain` of
//! a path no saved scan holds answers.
//!
//! - the other size measure (`apparent`, or `on disk` under `--size
//!   apparent`);
//! - a folder's newest modification in its subtree, its child changed
//!   least recently (the oldest of its children's newest modifications: a
//!   whole subtree's oldest would read every entry in it) and its largest
//!   child with its share;
//! - hard links: a file's `nlink` and the path of its first link, which
//!   its bytes are charged to; a folder's links outside it;
//! - clones: whether a file is one, its group and the members outside; a
//!   folder's members outside;
//! - `freeable_now_bytes` and how exact it is, or why the scan did not
//!   enter the folder; the dataless files under a folder;
//! - a folder's `regenerable_by` and `CACHEDIR.TAG`;
//! - git: ignored, precious, tracked;
//! - the directories under a folder the scan could not read;
//! - the storage flags;
//! - the access and creation times, or in a basic scan `attrs: basic` and
//!   the hint;
//! - the scan: the root volume's size and free space, what it uses that
//!   the scan did not find, when the scan ran and for how long, the size
//!   measure and the threads.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use duscope_core::envelope::Explain;
use duscope_core::index::Index;
use duscope_core::query::{
    Dataless, ExplainQuery, SizeKind, TreeView, abs_path, children, explain, explain_unindexed,
};
use duscope_core::rules::Caches;
use ratatui_core::layout::Rect;

use super::selected::{mode_row, selected_rows};
use super::{Context, Row, draw_rows, minute};
use crate::layout::inner;
use crate::widgets::{Canvas, Span, border, full_size, percent, thousands};

/// The narrowest column of facts over the map.
const OVERLAY_COLUMN: u16 = 36;

/// Reads a variable of the environment.
pub type Env<'a> = Box<dyn Fn(&str) -> Option<OsString> + 'a>;

/// What the facts read besides the view.
pub struct Sources<'a> {
    /// The saved scan `explain` reads, and the id in it of the view's root:
    /// the scan the view reads, or the one a scan run for the viewer was
    /// saved as. `None` for a scan not saved.
    pub index: Option<(&'a Index, u32)>,
    /// The cache rules, the user's merged over the vendored ones.
    pub caches: Caches,
    /// Reads a variable: the environment git's ignore files are found
    /// through.
    pub env: Env<'a>,
    /// The scan as a whole.
    pub scan: ScanFacts,
}

impl std::fmt::Debug for Sources<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sources")
            .field(
                "index",
                &self.index.map(|(index, base)| (index.file(), base)),
            )
            .field("scan", &self.scan)
            .finish_non_exhaustive()
    }
}

impl<'a> Sources<'a> {
    /// What the facts of `view` read when nothing else is given: the saved
    /// scan the view reads, if it reads one, and its block; the vendored
    /// cache rules; no variables.
    #[must_use]
    pub fn of(view: &'a dyn TreeView) -> Sources<'a> {
        let index = view.as_index();
        Sources {
            index,
            caches: Caches::vendored(),
            env: Box::new(|_| None),
            scan: index.map_or_else(ScanFacts::default, |(index, _)| ScanFacts::of_index(index)),
        }
    }
}

/// What the facts say of the scan as a whole.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScanFacts {
    /// The root volume's size and the bytes free on it, as the scan found
    /// them.
    pub volume: Option<(u64, u64)>,
    /// What the root's volume uses that the scan did not find, where the
    /// root is a mount point (`unaccounted_bytes`).
    pub unaccounted_bytes: Option<u64>,
    /// When the scan started and finished, Unix nanoseconds.
    pub times: Option<(i64, i64)>,
    /// The walk's worker threads.
    pub threads: Option<u64>,
}

impl ScanFacts {
    /// What a saved scan records of itself.
    #[must_use]
    pub fn of_index(index: &Index) -> ScanFacts {
        let meta = index.meta();
        ScanFacts {
            volume: (meta.volume_total > 0).then_some((meta.volume_total, meta.volume_free)),
            unaccounted_bytes: meta.unaccounted_bytes,
            times: Some((meta.started_at, meta.finished_at)),
            threads: Some(meta.thread_count),
        }
    }
}

/// The facts of one entry beyond the selected pane's, read once when `i`
/// shows them.
#[derive(Debug, Clone)]
pub struct Facts {
    /// `explain`'s answer, or why there is none.
    pub explain: Result<Explain, String>,
    /// Whether `explain` read a saved scan, rather than a fresh `stat`.
    pub saved: bool,
    /// A folder's largest child and its size.
    pub largest: Option<(u32, u64)>,
    /// A folder's child changed least recently, and its newest
    /// modification.
    pub oldest: Option<(u32, i64)>,
    /// A hard-linked file's first link, which its bytes are charged to, as
    /// the panes write paths; `None` where the file is that link.
    pub first_link: Option<String>,
    /// The directories at or under a folder that could not be read.
    pub unreadable: Option<u64>,
    /// The dataless files under a folder.
    pub dataless: Option<Dataless>,
    /// Why the scan did not enter a folder, as the envelope names it.
    pub not_entered: Option<String>,
}

impl Facts {
    /// The facts of entry `id` of `ctx`'s view: its children's in one pass
    /// over them, and `explain`'s of the saved scan in `sources`, or of a
    /// fresh `stat`.
    #[must_use]
    pub fn read(ctx: &Context<'_>, sources: &Sources<'_>, id: u32) -> Facts {
        let view = ctx.view;
        let node = view.node(id);
        let size = ctx.opts.size;
        let mut largest: Option<(u32, u64)> = None;
        let mut oldest: Option<(u32, i64)> = None;
        if node.is_dir() {
            for kid in children(view, &node) {
                let kid_node = view.node(kid);
                let bytes = kid_node.size(size);
                if largest.is_none_or(|(_, most)| bytes > most) {
                    largest = Some((kid, bytes));
                }
                if oldest.is_none_or(|(_, least)| kid_node.max_mtime_ns < least) {
                    oldest = Some((kid, kid_node.max_mtime_ns));
                }
            }
        }
        let q = ExplainQuery {
            caches: &sources.caches,
            env: &*sources.env,
        };
        let mut facts = Facts {
            explain: Err(String::new()),
            saved: false,
            largest,
            oldest,
            first_link: None,
            unreadable: None,
            dataless: None,
            not_entered: None,
        };
        let Some((index, base)) = sources.index else {
            let path = abs_path(view, id);
            facts.explain = explain_unindexed(Path::new(OsStr::from_bytes(&path)), &q)
                .map_err(|e| e.to_string());
            return facts;
        };
        let at = base + id;
        facts.saved = true;
        facts.explain = explain(index, at, &q).map_err(|e| e.to_string());
        if node.is_dir() {
            facts.unreadable = Some(index.unreadable_dirs(at, base + node.last_id));
            facts.dataless = Some(index.dataless(at));
            if !index.entered(at) {
                facts.not_entered = Some(not_entered(index, at, node.unreadable));
            }
        } else if node.nlink > 1 {
            facts.first_link = index
                .first_link(at)
                .filter(|&first| first != at)
                .map(|first| match first.checked_sub(base) {
                    Some(inside) if inside < view.len() => ctx.path(inside),
                    _ => String::from_utf8_lossy(&abs_path(index, first)).into_owned(),
                });
        }
        facts
    }
}

/// Why the scan did not enter folder `at` of `index`, as the envelope
/// names it: an exclude's reason (`excluded`, `excluded_cache`,
/// `skipped_containers`), `unreadable`, or the reason of the volume
/// mounted there (`one_file_system`, `network`, …).
fn not_entered(index: &Index, at: u32, unreadable: bool) -> String {
    if let Some((_, reason)) = index.excluded().into_iter().find(|&(id, _)| id == at) {
        return reason.to_owned();
    }
    if unreadable {
        return "unreadable".to_owned();
    }
    let path = abs_path(index, at);
    index
        .volumes()
        .into_iter()
        .find(|v| !v.entered && v.mount_point == path)
        .and_then(|v| v.reason)
        .unwrap_or_else(|| "mount_point".to_owned())
}

/// The rows `i` adds for `id`.
#[allow(clippy::too_many_lines)] // one fact a row, in the list's order
fn item_rows(canvas: &Canvas<'_>, ctx: &Context<'_>, facts: &Facts, id: u32) -> Vec<Row> {
    let roles = canvas.pack.roles;
    let text = |s: String| vec![Span::new(s, roles.text)];
    let dash = || vec![Span::new("—", roles.overlay1)];
    let hinted = |value: &str, hint: &str| {
        vec![
            Span::new(value.to_owned(), roles.text),
            Span::new(format!(" · {hint}"), roles.overlay1),
        ]
    };
    let node = ctx.view.node(id);
    let dir = node.is_dir();
    let e = facts.explain.as_ref().ok();
    let mut rows = Vec::new();
    rows.push(match ctx.opts.size {
        SizeKind::Allocated => Row::Fact("apparent", text(full_size(node.apparent))),
        SizeKind::Apparent => Row::Fact("on disk", text(full_size(node.allocated))),
    });
    if dir {
        rows.push(Row::Fact("newest", text(minute(node.max_mtime_ns))));
        rows.push(Row::Fact(
            "oldest",
            facts.oldest.map_or_else(dash, |(kid, ns)| {
                text(format!("{} · {}", minute(ns), ctx.name(kid, true)))
            }),
        ));
        rows.push(Row::Fact(
            "largest",
            facts.largest.map_or_else(dash, |(kid, bytes)| {
                let share = Context::share(bytes, ctx.size(&node));
                text(format!(
                    "{} · {} · {}%",
                    ctx.name(kid, true),
                    full_size(bytes),
                    percent(share)
                ))
            }),
        ));
    }
    let links = if dir {
        match e.and_then(|e| e.links_outside_subtree) {
            None => dash(),
            Some(0) => text("none outside".to_owned()),
            Some(n) => text(format!("{} outside", thousands(n))),
        }
    } else {
        let nlink = e
            .and_then(|e| e.nlink)
            .unwrap_or(u64::from(node.nlink))
            .max(1);
        match (&facts.first_link, nlink) {
            (_, 1) => text("1".to_owned()),
            (Some(first), n) => text(format!("{} · first {first}", thousands(n))),
            (None, n) if facts.saved => text(format!("{} · this is the first", thousands(n))),
            (None, n) => text(thousands(n)),
        }
    };
    rows.push(Row::Fact("links", links));
    let clones = match e {
        None => dash(),
        Some(e) if dir => match e.clone_members_outside {
            None => dash(),
            Some(0) => text("none outside".to_owned()),
            Some(n) => text(format!("{} members outside", thousands(n))),
        },
        Some(e) => match (e.is_clone, e.clone_group_size, e.clone_members_outside) {
            (None, ..) => dash(),
            (Some(false), ..) => text("not a clone".to_owned()),
            (Some(true), Some(size), outside) => text(format!(
                "group of {} · {} outside",
                thousands(size),
                outside.map_or_else(|| "—".to_owned(), thousands)
            )),
            (Some(true), None, _) => text("shares extents".to_owned()),
        },
    };
    rows.push(Row::Fact("clones", clones));
    let freeable = match (
        e.and_then(|e| e.freeable_now_bytes),
        e.and_then(|e| e.freeable_kind),
    ) {
        (Some(bytes), Some(kind)) => {
            text(format!("{} · {}", full_size(bytes), kind.replace('_', " ")))
        }
        _ => match &facts.not_entered {
            Some(why) => hinted("—", &format!("not entered ({why})")),
            None => dash(),
        },
    };
    rows.push(Row::Fact("freeable", freeable));
    if let Some(dataless) = facts.dataless.filter(|d| d.files > 0) {
        rows.push(Row::Fact(
            "dataless",
            text(format!(
                "{} files · {} not on this disk",
                thousands(dataless.files),
                full_size(dataless.apparent)
            )),
        ));
    }
    if dir {
        rows.push(Row::Fact(
            "regen by",
            e.and_then(|e| e.regenerable_by)
                .map_or_else(dash, |by| text(by.to_owned())),
        ));
        rows.push(Row::Fact(
            "cachedir",
            if e.is_some_and(|e| e.cachedir_tag) {
                text("CACHEDIR.TAG".to_owned())
            } else {
                dash()
            },
        ));
    }
    let git = match e.map(|e| &e.git) {
        None => dash(),
        Some(None) => text("not in a repository".to_owned()),
        Some(Some(git)) => {
            let mut parts = vec![if git.ignored {
                "ignored"
            } else {
                "not ignored"
            }];
            if git.precious {
                parts.push("precious");
            }
            parts.push(match (dir, git.tracked_inside) {
                (true, true) => "tracked inside",
                (true, false) => "nothing tracked",
                (false, true) => "tracked",
                (false, false) => "untracked",
            });
            text(parts.join(" · "))
        }
    };
    rows.push(Row::Fact("git", git));
    if dir {
        rows.push(Row::Fact(
            "unread",
            match facts.unreadable {
                None => dash(),
                Some(0) => text("none inside".to_owned()),
                Some(n) => text(format!("{} directories inside", thousands(n))),
            },
        ));
    }
    if let Some(e) = e.filter(|e| !e.flags.is_empty()) {
        let flags: Vec<String> = e.flags.iter().map(|f| f.replace('_', " ")).collect();
        rows.push(Row::Fact("flags", text(flags.join(" · "))));
    }
    if ctx.view.full_attrs() {
        rows.push(Row::Fact(
            "accessed",
            node.atime_ns.map_or_else(dash, |ns| text(minute(ns))),
        ));
        rows.push(Row::Fact(
            "created",
            node.btime_ns.map_or_else(dash, |ns| text(minute(ns))),
        ));
    } else {
        rows.push(Row::Fact(
            "attrs",
            hinted("basic", "rerun with --attrs full"),
        ));
    }
    if !facts.saved {
        rows.push(Row::Fact("saved", hinted("no", "rerun with --save")));
    }
    if let Err(why) = &facts.explain {
        rows.push(Row::Fact(
            "explain",
            vec![Span::new(why.clone(), roles.red)],
        ));
    }
    rows
}

/// The scan's rows: its volume, what it did not find, when it ran, the
/// size measure and the threads.
fn scan_rows(canvas: &Canvas<'_>, ctx: &Context<'_>, scan: &ScanFacts) -> Vec<Row> {
    let roles = canvas.pack.roles;
    let text = |s: String| vec![Span::new(s, roles.text)];
    let dash = || vec![Span::new("—", roles.overlay1)];
    let mut rows = vec![Row::Fact(
        "volume",
        scan.volume.map_or_else(dash, |(total, free)| {
            text(format!("{} · {} free", full_size(total), full_size(free)))
        }),
    )];
    if let Some(bytes) = scan.unaccounted_bytes {
        rows.push(Row::Fact("not found", text(full_size(bytes))));
    }
    rows.push(Row::Fact(
        "scanned",
        scan.times.map_or_else(dash, |(started, finished)| {
            // Nanoseconds as f64: exact to far beyond any scan's length.
            #[allow(clippy::cast_precision_loss)]
            let seconds = finished.saturating_sub(started).max(0) as f64 / 1e9;
            text(format!("{} · {seconds:.1}s", minute(started)))
        }),
    ));
    rows.push(Row::Fact(
        "measure",
        text(ctx.opts.size.as_str().to_owned()),
    ));
    rows.push(Row::Fact(
        "threads",
        scan.threads.map_or_else(dash, |n| text(thousands(n))),
    ));
    rows
}

/// The selected pane grown over the right column into `pane`, border
/// included: its rows, a blank row, what `i` adds, a blank row and the
/// scan's; as many as fit.
pub fn expanded(
    canvas: &mut Canvas<'_>,
    pane: Rect,
    ctx: &Context<'_>,
    sources: &Sources<'_>,
    facts: &Facts,
    id: u32,
) {
    canvas.pane(pane, "selected", border(canvas.pack, false));
    let mut rows = selected_rows(canvas, ctx, id);
    rows.push(Row::Blank);
    rows.extend(item_rows(canvas, ctx, facts, id));
    rows.push(Row::Blank);
    rows.extend(scan_rows(canvas, ctx, &sources.scan));
    draw_rows(canvas, inner(pane), &rows);
}

/// The facts over the map, in `pane` (the map's pane), with the selected
/// pane folded under it: the mode and owner and what `i` adds, then the
/// scan's rows, in as many columns of at least [`OVERLAY_COLUMN`] as fit
/// (two at most), the scan's in a column of their own where the rest fit
/// in one.
pub fn overlay(
    canvas: &mut Canvas<'_>,
    pane: Rect,
    ctx: &Context<'_>,
    sources: &Sources<'_>,
    facts: &Facts,
    id: u32,
) {
    let base = canvas.pack.roles.base;
    canvas.fill(pane, base);
    canvas.pane(pane, "facts", border(canvas.pack, false));
    let area = inner(pane);
    let height = usize::from(area.height);
    if height == 0 {
        return;
    }
    let mut item = vec![mode_row(canvas, ctx, id)];
    item.extend(item_rows(canvas, ctx, facts, id));
    let scan = scan_rows(canvas, ctx, &sources.scan);
    let columns = (area.width / OVERLAY_COLUMN).clamp(1, 2);
    let mut flow: Vec<Vec<Row>> = if columns == 2 && item.len() <= height {
        vec![item, scan]
    } else {
        let mut all = item;
        all.push(Row::Blank);
        all.extend(scan);
        all.chunks(height).map(<[Row]>::to_vec).collect()
    };
    flow.truncate(usize::from(columns));
    let width = area.width / columns;
    for (column, rows) in (0..columns).zip(&flow) {
        let x = area.x + column * width;
        // A cell between the columns.
        let w = if column + 1 < columns {
            width.saturating_sub(1)
        } else {
            area.right() - x
        };
        draw_rows(canvas, Rect::new(x, area.y, w, area.height), rows);
    }
}

#[cfg(test)]
mod tests;
