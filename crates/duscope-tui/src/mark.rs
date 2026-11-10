//! `d`, `D` and `yes`: the viewer's way to the Trash.
//!
//! `d` marks the selection and `d` on a marked entry unmarks it; a folder's
//! mark covers what is under it, so marking a folder drops the marks inside
//! it and an entry inside a marked folder is not marked again. `D` opens the
//! mark pane: the plan [`clean_plan`] makes of the marked paths, each
//! planned at its own path while its folder still resolves to itself (a
//! folder that leads elsewhere now fails the plan, so it never holds an
//! entry but the one marked), printed as `clean` prints it: the same
//! table, the same line under it and the same question, asked once. Only
//! `yes` typed in full (and `⏎`) moves anything, through [`clean_execute`],
//! to the Trash and nowhere else; any other answer closes the pane and
//! moves nothing. A plan with a refused item moves nothing, as `clean`
//! without `--skip-refused` does, and a plan that cannot be made says why.
//! A plan that measured items from the tree, since they changed after the
//! saved scan, says so under its line, as `clean` warns of it
//! (`changed_since_scan`): the map shows the scan's sizes, the pane what is
//! there now.
//!
//! The marks cross a reload by their absolute paths: each lands on the
//! entry at the same path in the new scan, or is dropped.
//!
//! What moved stays on the map for the session as a freed tile (the saved
//! scan is not changed; a rescan measures the tree again) and is counted for
//! the summary the binary prints when the viewer closes. Neither it nor a
//! folder that holds it can be marked again before a rescan: the saved
//! scan still counts its bytes, which a second move would count twice.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fmt::Write as _;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use duscope_core::clean::{CleanItem, CleanOutcome, Method, Plan, clean_execute, clean_plan};
use duscope_core::envelope::{CleanPlan, PlanItem};
use duscope_core::query::{ExplainQuery, SizeKind, TreeView, abs_path};
use duscope_core::resolve::resolve_path;
use duscope_treemap::cells::{Cell, Color};
use duscope_treemap::packs::Roles;
use duscope_treemap::paint::{DrawnKind, Glyphs};
use ratatui_core::layout::Rect;

use crate::cursor::outline;
use crate::layout::inner;
use crate::mask::{self, BOTTOM, TOP};
use crate::panes::Sources;
use crate::widgets::{Canvas, Span, thousands};
use crate::zoom::Map;

/// What `d` found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Toggled {
    /// The entry is marked now.
    Marked,
    /// The entry was marked and is not now.
    Unmarked,
    /// Nothing changed: the map's root cannot be marked.
    Root,
    /// Nothing changed: this marked folder covers the entry.
    Inside(u32),
}

/// The marked entries, each a subtree no other mark is inside.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Marks {
    ids: BTreeSet<u32>,
}

impl Marks {
    /// Whether nothing is marked.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// How many marks there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    /// Whether `id` itself carries a mark.
    #[must_use]
    pub fn holds(&self, id: u32) -> bool {
        self.ids.contains(&id)
    }

    /// The marked ids, in pre-order: the plan's order.
    #[must_use]
    pub fn ids(&self) -> Vec<u32> {
        self.ids.iter().copied().collect()
    }

    /// Takes the mark off `id`, when it has one.
    pub fn remove(&mut self, id: u32) {
        self.ids.remove(&id);
    }

    /// The marks as absolute paths, to carry them across a reload: a new
    /// scan may be rooted elsewhere than `view`, and a path relative to a
    /// root would name another entry there.
    #[must_use]
    pub fn paths(&self, view: &dyn TreeView) -> Vec<Vec<u8>> {
        self.ids.iter().map(|&id| abs_path(view, id)).collect()
    }

    /// Marks the entry of `view` at each of `paths` (absolute, as
    /// [`Marks::paths`] gives them, already free of nesting): the entry at
    /// that same path, or none when `view` no longer holds one there. How
    /// many were dropped.
    pub fn restore(&mut self, view: &dyn TreeView, paths: &[Vec<u8>]) -> usize {
        let mut dropped = 0;
        for path in paths {
            match entry_at(view, path) {
                Some(id) => {
                    self.ids.insert(id);
                }
                None => dropped += 1,
            }
        }
        dropped
    }

    /// `d` on `id`: the entry is marked, or unmarked if it was; a folder's
    /// mark replaces the marks inside it. The root and an entry inside a
    /// marked folder are left as they are.
    pub fn toggle(&mut self, view: &dyn TreeView, id: u32) -> Toggled {
        if id == 0 {
            return Toggled::Root;
        }
        if self.ids.remove(&id) {
            return Toggled::Unmarked;
        }
        let node = view.node(id);
        let mut at = node.parent;
        while at != 0 {
            if self.ids.contains(&at) {
                return Toggled::Inside(at);
            }
            at = view.node(at).parent;
        }
        if node.last_id > id {
            let inside: Vec<u32> = self.ids.range(id + 1..=node.last_id).copied().collect();
            for marked in inside {
                self.ids.remove(&marked);
            }
        }
        self.ids.insert(id);
        Toggled::Marked
    }

    /// The bytes the marks cover, in `size`.
    #[must_use]
    pub fn total(&self, view: &dyn TreeView, size: SizeKind) -> u64 {
        self.ids.iter().map(|&id| view.node(id).size(size)).sum()
    }

    /// Outlines the marked tiles of `map`, drawn in `area`, in the pack's
    /// red, a `✕` on the top edge where the tile is wide enough.
    pub fn paint(&self, canvas: &mut Canvas<'_>, area: Rect, map: &Map) {
        let roles = canvas.pack.roles;
        let glyph = if canvas.glyphs == Glyphs::Ascii {
            " x "
        } else {
            " ✕ "
        };
        for &id in &self.ids {
            let Some(index) = map.own(id) else {
                continue;
            };
            if let Some((x, y, w, _)) = outline(canvas, area, map.tiles()[index].rect, roles.red)
                && w >= 5
            {
                canvas.spans(x + 1, y, x + w - 1, &[Span::bold(glyph, roles.red)]);
            }
        }
    }
}

/// The entry of `view` at the absolute path `path`: `None` where `view`'s
/// root does not hold it, where it is the root itself (which is never
/// marked), or where there is no such entry.
fn entry_at(view: &dyn TreeView, path: &[u8]) -> Option<u32> {
    let root = view.root_path().as_os_str().as_bytes();
    let rest = path.strip_prefix(root)?;
    let rel = if root.ends_with(b"/") {
        rest
    } else {
        rest.strip_prefix(b"/")?
    };
    if rel.is_empty() {
        return None;
    }
    crate::cursor::lookup(view, rel)
}

/// What the viewer moved to the Trash this session: each freed subtree's
/// first id and last, which the map paints as freed tiles until a rescan.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Freed {
    ranges: BTreeMap<u32, u32>,
}

impl Freed {
    /// Whether nothing has been freed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }

    /// Records `id` and what is under it, moved to the Trash.
    pub fn add(&mut self, view: &dyn TreeView, id: u32) {
        self.ranges.insert(id, view.node(id).last_id);
    }

    /// Whether `id` was freed, itself or with a folder above it.
    #[must_use]
    pub fn covers(&self, id: u32) -> bool {
        self.ranges
            .range(..=id)
            .next_back()
            .is_some_and(|(_, &last)| id <= last)
    }

    /// Whether something below `id`, whose subtree ends at `last`, was
    /// freed: the folder holds what moved.
    #[must_use]
    pub fn below(&self, id: u32, last: u32) -> bool {
        id < last && self.ranges.range(id + 1..=last).next().is_some()
    }

    /// Paints what was freed: every tile drawn for a freed entry takes the
    /// pack's surface colour (hatched where the cells have no colour to
    /// read), half a cell at a time at a tile's top and bottom edges, and a
    /// freed entry's own tile says so where it is wide enough.
    pub fn paint(&self, canvas: &mut Canvas<'_>, area: Rect, map: &Map) {
        if self.ranges.is_empty() {
            return;
        }
        let roles = canvas.pack.roles;
        let ascii = canvas.glyphs == Glyphs::Ascii;
        let halves = !ascii && !canvas.pack.shades;
        let hatch = match (canvas.pack.shades, ascii) {
            (false, _) => ' ',
            (true, false) => '╱',
            (true, true) => '/',
        };
        let (w, h) = (map.cells.width(), map.cells.height());
        let mut flags = vec![0u8; usize::from(w) * usize::from(h)];
        for tile in map.tiles().iter().filter(|tile| self.covers(tile.id)) {
            mask::mark(&mut flags, tile.rect, w, h);
        }
        for y in 0..h.min(area.height) {
            for x in 0..w.min(area.width) {
                let flag = flags[usize::from(y) * usize::from(w) + usize::from(x)];
                if flag == 0 {
                    continue;
                }
                let Some(cell) = canvas.cells.get_mut(area.x + x, area.y + y) else {
                    continue;
                };
                *cell = if flag == TOP | BOTTOM {
                    Cell {
                        ch: hatch,
                        fg: roles.overlay1,
                        bg: roles.surface1,
                        ..Cell::default()
                    }
                } else {
                    mask::masked(*cell, flag, roles.surface1, halves)
                };
            }
        }
        for tile in map.tiles() {
            let open_rows = matches!(tile.kind, DrawnKind::Leaf | DrawnKind::Box { .. });
            if !open_rows || !self.ranges.contains_key(&tile.id) {
                continue;
            }
            let top = tile.rect.y.div_ceil(2);
            let rows = tile.rect.y.midpoint(tile.rect.h);
            let word = " freed ";
            let wide = u16::try_from(word.len()).unwrap_or(u16::MAX);
            if rows <= top || tile.rect.w < wide {
                continue;
            }
            let x = area.x + tile.rect.x + (tile.rect.w - wide) / 2;
            let y = area.y + top + rows.saturating_sub(top).saturating_sub(1) / 2;
            if y < area.bottom() {
                canvas.spans(x, y, x + wide, &[Span::new(word, roles.text)]);
            }
        }
    }
}

/// How a size is printed in the pane: as `clean` prints it, which the
/// binary's `--si` and `--bytes` choose.
pub type SizeText<'a> = dyn Fn(u64) -> String + 'a;

/// What the pane holds.
#[derive(Debug)]
enum State {
    /// A plan waiting for the answer.
    Planned(Box<Plan>),
    /// The plan acted on, with what happened: the pane stays open only when
    /// something failed.
    Acted(Box<Plan>, CleanOutcome),
    /// No plan could be made: why.
    Unplanned(String),
}

/// The mark pane: the plan of the marks, and the answer being typed.
#[derive(Debug)]
pub struct Pane {
    /// The marked ids, in the plan's order.
    ids: Vec<u32>,
    state: State,
    /// What has been typed.
    input: String,
}

/// What the viewer says when the answer was not `yes`: the pane has
/// closed, as `clean` asks once.
pub const DECLINED: &str = "the answer was not yes, so nothing was moved";

/// What `⏎` in the pane came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// Nothing: the pane asks nothing (a plan with a refused item, one
    /// that could not be made, one acted on already).
    Unasked,
    /// The answer was not `yes`: nothing moved, and the pane closes.
    Declined,
    /// `yes`: what the plan moved.
    Moved(Moved),
}

/// What `yes` did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Moved {
    /// The ids moved to the Trash.
    pub ids: Vec<u32>,
    /// The line `clean` prints for it.
    pub line: String,
    /// The bytes moved.
    pub bytes: u64,
}

/// `n` things, the noun in the plural past one, as `clean` counts them.
fn count(n: usize, noun: &str) -> String {
    format!(
        "{} {noun}{}",
        thousands(n as u64),
        if n == 1 { "" } else { "s" }
    )
}

/// `text` with its control characters written out, as the output layer
/// prints a path on one line.
fn printable(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_control() {
            let _ = write!(out, "\\x{:02x}", u32::from(c));
        } else {
            out.push(c);
        }
    }
    out
}

/// One plan row's cell for each column, as `clean`'s table prints them.
fn row(item: &PlanItem, size: &SizeText<'_>) -> [String; 4] {
    let action = match (item.reason, item.outcome) {
        (Some(reason), _) => format!("refused: {}", reason.replace('_', " ")),
        (None, Some(outcome)) => outcome.to_owned(),
        (None, None) => item.action.to_owned(),
    };
    [
        action,
        size(item.allocated_bytes),
        item.frees_bytes.map_or_else(|| "-".to_owned(), size),
        printable(&item.path),
    ]
}

/// The columns of `clean`'s table.
const COLUMNS: [&str; 4] = ["action", "size", "frees", "path"];

/// The width of each right-aligned column of the table.
fn widths(rows: &[[String; 4]]) -> [usize; 3] {
    let mut widths = [0usize; 3];
    for (c, width) in widths.iter_mut().enumerate() {
        *width = rows
            .iter()
            .map(|row| row[c].chars().count())
            .chain([COLUMNS[c].chars().count()])
            .max()
            .unwrap_or(0);
    }
    widths
}

/// The right-aligned columns of a row, as the grid prints them, up to the
/// last column.
fn left_of_path(cells: [&str; 3], widths: [usize; 3]) -> String {
    let mut out = String::new();
    for (c, cell) in cells.iter().enumerate() {
        let _ = write!(out, "{cell:>width$}  ", width = widths[c]);
    }
    out
}

/// A row of the table: its first three columns, and its path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanRow {
    /// The action, size and frees columns, each with its two spaces.
    pub columns: String,
    /// The path, whole.
    pub path: String,
}

impl PlanRow {
    /// The row as `clean` prints it, whole.
    #[cfg(test)]
    #[must_use]
    pub fn whole(&self) -> String {
        format!("{}{}", self.columns, self.path)
    }
}

/// The plan as `clean` prints it: the table's rows (the header first) and
/// the line under it.
#[must_use]
pub fn plan_rows(plan: &CleanPlan, size: &SizeText<'_>) -> (Vec<PlanRow>, String) {
    let cells: Vec<[String; 4]> = plan.items.iter().map(|item| row(item, size)).collect();
    let widths = widths(&cells);
    let mut rows = Vec::with_capacity(cells.len() + 1);
    rows.push(PlanRow {
        columns: left_of_path([COLUMNS[0], COLUMNS[1], COLUMNS[2]], widths),
        path: COLUMNS[3].to_owned(),
    });
    for cell in &cells {
        rows.push(PlanRow {
            columns: left_of_path([&cell[0], &cell[1], &cell[2]], widths),
            path: cell[3].clone(),
        });
    }
    (rows, plan_line(plan, size))
}

/// The line under the table: what the plan would move and what it frees.
fn plan_line(plan: &CleanPlan, size: &SizeText<'_>) -> String {
    let refused = refused_count(plan);
    let mut line = format!(
        "{} to move to the Trash: {} on disk, {} freed once the Trash is emptied",
        count(plan.items.len() - refused, "item"),
        size(plan.total_allocated_bytes),
        size(plan.total_frees_bytes),
    );
    if refused > 0 {
        let _ = write!(line, " · {} refused", thousands(refused as u64));
    }
    line
}

/// The plan's refused items.
fn refused_count(plan: &CleanPlan) -> usize {
    plan.items
        .iter()
        .filter(|item| item.action == "refused")
        .count()
}

/// The line `clean` prints once it has moved them.
fn moved_line(outcome: &CleanOutcome, size: &SizeText<'_>) -> String {
    let mut line = format!(
        "moved {} to the Trash, {} (not yet freed)",
        count(
            usize::try_from(outcome.trashed_count).unwrap_or(usize::MAX),
            "item"
        ),
        size(outcome.trashed_bytes)
    );
    if outcome.failed_count > 0 {
        let _ = write!(line, " · {} not moved", thousands(outcome.failed_count));
    }
    line
}

/// The question `clean` asks on a terminal, the pane's as well.
const QUESTION: &str = "Type yes to move them to the Trash: ";

/// What `clean` warns of a plan whose `changed` items changed since the
/// saved scan, so that it measured them from the tree; none when none did.
fn changed_note(changed: usize) -> Option<String> {
    (changed > 0).then(|| {
        format!(
            "{} changed since the saved scan, so the plan measured {} from the tree",
            count(changed, "item"),
            if changed == 1 { "it" } else { "them" }
        )
    })
}

/// What a plan with a refused item says: `clean` moves nothing either.
fn refused_note(plan: &CleanPlan) -> String {
    let refused = refused_count(plan);
    format!(
        "{} of the {} items cannot be moved to the Trash, so nothing is moved: unmark {}",
        thousands(refused as u64),
        thousands(plan.items.len() as u64),
        if refused == 1 {
            "it with d"
        } else {
            "them with d"
        }
    )
}

/// Everything the pane shows: built from a state, so that a test can look
/// at any of them.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Body {
    /// How many entries the pane is about.
    count: usize,
    /// The table, its header first.
    rows: Vec<PlanRow>,
    /// The lines under it, each with its colour.
    under: Vec<(String, Color)>,
    /// The answer typed so far, when the pane asks for one.
    answer: Option<String>,
}

impl Body {
    /// A plan waiting for its answer: its table and the line under it, and
    /// the question if it moves anything and refuses nothing.
    pub(crate) fn planned(
        plan: &CleanPlan,
        count: usize,
        input: &str,
        size: &SizeText<'_>,
        roles: &Roles,
    ) -> Body {
        let (rows, line) = plan_rows(plan, size);
        let refused = refused_count(plan) > 0 || plan.items.is_empty();
        let mut under = vec![(line, roles.text)];
        if refused {
            under.push((refused_note(plan), roles.red));
        }
        Body {
            count,
            rows,
            under,
            answer: (!refused).then(|| input.to_owned()),
        }
    }

    /// A plan acted on: its table with what happened, each failure and the
    /// line `clean` prints.
    pub(crate) fn acted(
        plan: &CleanPlan,
        outcome: &CleanOutcome,
        count: usize,
        size: &SizeText<'_>,
        roles: &Roles,
    ) -> Body {
        let (rows, _) = plan_rows(plan, size);
        let mut under: Vec<(String, Color)> = plan
            .items
            .iter()
            .filter_map(|item| item.error.as_ref().map(|error| (item, error)))
            .map(|(item, error)| {
                (
                    format!("failed: {}: {}", printable(&item.path), error.message),
                    roles.red,
                )
            })
            .collect();
        under.push((moved_line(outcome, size), roles.text));
        Body {
            count,
            rows,
            under,
            answer: None,
        }
    }

    /// No plan could be made.
    pub(crate) fn unplanned(why: &str, count: usize, roles: &Roles) -> Body {
        Body {
            count,
            rows: Vec::new(),
            under: vec![
                (why.to_owned(), roles.red),
                ("nothing was moved".to_owned(), roles.subtext),
            ],
            answer: None,
        }
    }

    /// Draws the pane into `pane`, border included: the table's rows that
    /// fit, a `… n more` row for the rest, the lines under them and the
    /// question.
    pub(crate) fn draw(&self, canvas: &mut Canvas<'_>, pane: Rect) {
        let roles = canvas.pack.roles;
        let ascii = canvas.glyphs == Glyphs::Ascii;
        canvas.fill(pane, roles.base);
        let title = format!("mark pane · {} marked", thousands(self.count as u64));
        canvas.pane(pane, &title, roles.accent);
        let area = inner(pane);
        if area.width < 24 || area.height == 0 {
            return;
        }
        let end = area.right();
        // The table's room: what the lines under it and the question leave
        // of the height, a blank line before those counted.
        let question = usize::from(self.answer.is_some());
        let below = self.under.len() + question;
        let height = usize::from(area.height);
        let more = if self.rows.len() + 1 + below > height {
            self.rows
                .len()
                .saturating_sub(height.saturating_sub(below + 2))
        } else {
            0
        };
        let mut y = area.y;
        for (i, row) in self.rows.iter().take(self.rows.len() - more).enumerate() {
            if y >= area.bottom() {
                break;
            }
            let (color, bold) = if i == 0 {
                (roles.title, true)
            } else {
                (roles.text, false)
            };
            let text = fit_row(canvas, row, area.width);
            let span = if bold {
                Span::bold(text, color)
            } else {
                Span::new(text, color)
            };
            canvas.spans(area.x, y, end, &[span]);
            y += 1;
        }
        if more > 0 && y < area.bottom() {
            let text = format!("… {} more", thousands(more as u64));
            canvas.spans(area.x, y, end, &[Span::new(text, roles.subtext)]);
            y += 1;
        }
        if !self.rows.is_empty() {
            y = (y + 1).min(area.bottom());
        }
        for (text, color) in &self.under {
            if y >= area.bottom() {
                break;
            }
            let shown = canvas.cut(text, area.width);
            canvas.spans(area.x, y, end, &[Span::new(shown, *color)]);
            y += 1;
        }
        if let Some(input) = &self.answer
            && y < area.bottom()
        {
            let cursor = if ascii { "_" } else { "▌" };
            canvas.spans(
                area.x,
                y,
                end,
                &[
                    Span::new(QUESTION, roles.subtext),
                    Span::new(input.as_str(), roles.text),
                    Span::new(cursor, roles.accent),
                ],
            );
        }
    }
}

/// `row` cut to `room`: the columns whole and the path from its left, the
/// end of a long path being what names it.
fn fit_row(canvas: &Canvas<'_>, row: &PlanRow, room: u16) -> String {
    let wide = canvas.width(&row.columns);
    if wide >= room {
        return canvas.cut(&row.columns, room);
    }
    format!("{}{}", row.columns, canvas.cut_left(&row.path, room - wide))
}

impl Pane {
    /// The pane of the marks in `view`, planned against the saved scan in
    /// `sources`, or saying why it cannot be planned. Each mark is planned
    /// at its own path ([`marked_item`]), and a mark whose folder leads
    /// elsewhere now fails the plan, as a changed item from a handle fails
    /// `clean`'s. The saved scan holds no entry's inode, so the plan checks
    /// what is at the path when the pane opens, as `clean` does for a path.
    pub fn open(marks: &Marks, view: &dyn TreeView, sources: &Sources<'_>) -> Pane {
        let ids = marks.ids();
        let state = match sources.index {
            None => State::Unplanned(format!(
                "no saved scan holds this map, and clean plans from a saved scan: run duscope \
                 scan --save {}",
                view.root_path().display()
            )),
            Some((index, _)) => {
                let query = ExplainQuery {
                    caches: &sources.caches,
                    env: &*sources.env,
                };
                let items: Result<Vec<CleanItem>, String> = ids
                    .iter()
                    .map(|&id| marked_item(&abs_path(view, id)))
                    .collect();
                match items.and_then(|items| {
                    clean_plan(&items, index, &query, Method::Trash).map_err(|e| e.to_string())
                }) {
                    Ok(plan) => State::Planned(Box::new(plan)),
                    Err(why) => State::Unplanned(why),
                }
            }
        };
        Pane {
            ids,
            state,
            input: String::new(),
        }
    }

    /// Whether `yes` is what the pane is waiting for: a plan with nothing
    /// refused and something to move.
    fn asks(&self) -> bool {
        matches!(&self.state, State::Planned(plan) if plan.refused_count() == 0 && plan.acting_count() > 0)
    }

    /// A character typed into the answer; true when it counted.
    pub fn typed(&mut self, c: char) -> bool {
        if self.asks() && self.input.chars().count() < 16 {
            self.input.push(c);
            return true;
        }
        false
    }

    /// `⌫` in the answer.
    pub fn delete(&mut self) -> bool {
        self.input.pop().is_some()
    }

    /// `⏎`: moves the plan when the answer is `yes` in full, and only then;
    /// any other answer is [`Answer::Declined`], and the viewer closes the
    /// pane: the question is asked once, as `clean` asks it.
    pub fn answer(&mut self, size: &SizeText<'_>) -> Answer {
        if !self.asks() {
            return Answer::Unasked;
        }
        if self.input != "yes" {
            self.input.clear();
            return Answer::Declined;
        }
        let State::Planned(mut plan) =
            std::mem::replace(&mut self.state, State::Unplanned(String::new()))
        else {
            return Answer::Unasked;
        };
        let outcome = clean_execute(&mut plan);
        let ids = plan
            .plan
            .items
            .iter()
            .zip(&self.ids)
            .filter(|(item, _)| item.outcome == Some("trashed"))
            .map(|(_, &id)| id)
            .collect();
        let line = moved_line(&outcome, size);
        let bytes = outcome.trashed_bytes;
        self.state = State::Acted(plan, outcome);
        self.input.clear();
        Answer::Moved(Moved { ids, line, bytes })
    }

    /// Whether the pane should stay open after it acted: an item failed.
    #[must_use]
    pub fn stays_open(&self) -> bool {
        matches!(&self.state, State::Acted(_, outcome) if outcome.failed_count > 0)
    }

    /// Draws the pane over `pane`, border included.
    pub fn draw(&self, canvas: &mut Canvas<'_>, pane: Rect, size: &SizeText<'_>) {
        let roles = canvas.pack.roles;
        let count = self.ids.len();
        let body = match &self.state {
            State::Planned(plan) => {
                let mut body = Body::planned(&plan.plan, count, &self.input, size, &roles);
                if let Some(note) = changed_note(plan.changed_count()) {
                    body.under.insert(1, (note, roles.subtext));
                }
                body
            }
            State::Acted(plan, outcome) => Body::acted(&plan.plan, outcome, count, size, &roles),
            State::Unplanned(why) => Body::unplanned(why, count, &roles),
        };
        body.draw(canvas, pane);
    }
}

/// The item to plan for the mark at `path`, absolute as the scan names it,
/// or why there is none. `clean` plans a path it is given with every
/// symbolic link above its last component resolved; a mark names an entry
/// of the scan, not a path to follow, so it is planned at its own path, and
/// only while its folder still resolves to itself. A folder that leads
/// elsewhere (replaced by a link since the scan, or a link the scan
/// followed) would put another entry in the plan, `other/b.bin` for a mark
/// on `dir/b.bin` once `dir` is a link to `other`, and the freed tile on the
/// mark: the mark fails the plan instead, as a changed item from a handle
/// fails `clean`'s (`path_changed`).
fn marked_item(path: &[u8]) -> Result<CleanItem, String> {
    let path = PathBuf::from(OsStr::from_bytes(path));
    if let Some(parent) = path.parent() {
        let real = resolve_path(Path::new("/"), parent).real;
        if real != parent {
            return Err(format!(
                "the folder of a mark leads elsewhere now: {} leads to {}",
                parent.display(),
                real.display()
            ));
        }
    }
    Ok(CleanItem { path, handle: None })
}

#[cfg(test)]
mod tests;
