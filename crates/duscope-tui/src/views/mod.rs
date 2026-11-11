//! The viewer's four views, chosen with `1`–`4`, and what the keys set for
//! them: the tab, the pane in focus, the sort of the siblings and the list.
//!
//! - **treemap** (`1`): the map ([`crate::cursor`]).
//! - **list** (`2`, [`list`]): the current folder as a table, in the `s`
//!   sort.
//! - **largest** (`3`, [`largest`]): the largest files under the map's
//!   root, the in-app `top --files`.
//! - **types** (`4`, [`types`]): the classes as `by-type` reads them, under
//!   the map's root.
//!
//! The tables are the design system's ([`table`]) and stand where the map
//! does; the selected, siblings and legend panes stay. A mask ([`crate::mask`])
//! keeps a row out of the list and the largest files as it keeps a tile off
//! the map. `?` shows the key map ([`help`]) over the panes.

pub mod help;
pub mod largest;
pub mod list;
pub mod table;
pub mod types;

use std::cell::RefCell;
use std::cmp::Reverse;
use std::ops::Range;
use std::rc::Rc;

use duscope_core::query::{SortKey, children, sort_ids};
use ratatui_core::layout::Rect;

use crate::mask::{Mask, WALK_BUDGET};
use crate::panes::Context;
use crate::widgets::Canvas;

/// The rows of a list kept: the folders visited last.
const KEPT_LISTS: usize = 8;

/// A view of the scan (the `1`–`4` tabs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    /// The map.
    #[default]
    Treemap,
    /// The current folder as a table.
    List,
    /// The largest files under the map's root.
    Largest,
    /// The classes as `by-type` reads them.
    Types,
}

impl Tab {
    /// The tab's place in the header (0–3).
    #[must_use]
    pub fn index(self) -> usize {
        match self {
            Tab::Treemap => 0,
            Tab::List => 1,
            Tab::Largest => 2,
            Tab::Types => 3,
        }
    }
}

/// The pane `⇥` focuses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Focus {
    /// The map, or the table standing in its place.
    #[default]
    Map,
    /// The selection's siblings.
    Siblings,
    /// The colours' shares.
    Legend,
}

impl Focus {
    /// The next pane, or the previous with `back`.
    #[must_use]
    pub fn next(self, back: bool) -> Focus {
        match (self, back) {
            (Focus::Map, false) | (Focus::Legend, true) => Focus::Siblings,
            (Focus::Siblings, false) | (Focus::Map, true) => Focus::Legend,
            (Focus::Legend, false) | (Focus::Siblings, true) => Focus::Map,
        }
    }
}

/// How `s` orders the siblings and the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Sort {
    /// Largest first, the viewer's start.
    #[default]
    Size,
    /// By name.
    Name,
    /// The newest change in the subtree first.
    Modified,
    /// The most files first.
    Count,
}

impl Sort {
    /// The next sort.
    #[must_use]
    pub fn next(self) -> Sort {
        match self {
            Sort::Size => Sort::Name,
            Sort::Name => Sort::Modified,
            Sort::Modified => Sort::Count,
            Sort::Count => Sort::Size,
        }
    }

    /// The sort's word (`size`, `name`, `modified`, `count`).
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Sort::Size => "size",
            Sort::Name => "name",
            Sort::Modified => "modified",
            Sort::Count => "count",
        }
    }

    fn key(self) -> SortKey {
        match self {
            Sort::Size => SortKey::Size,
            Sort::Name => SortKey::Path,
            Sort::Modified => SortKey::Mtime,
            Sort::Count => SortKey::Count,
        }
    }
}

/// What the keys have set for the viewer's lists and maps: the sort, the
/// mask and the rows worked out for them.
#[derive(Debug, Clone, Copy)]
pub struct Viewing<'a> {
    /// The sort of the siblings and the list.
    pub sort: Sort,
    /// The mask, where one is on.
    pub mask: Option<&'a Mask>,
    /// The filter's text for the pane titles.
    pub filter: Option<&'a str>,
    /// The rows worked out so far.
    pub rows: &'a Rows,
}

impl Viewing<'_> {
    /// What the pane titles add for the filter: nothing without one.
    #[must_use]
    pub fn filter_suffix(&self) -> String {
        self.filter
            .map_or_else(String::new, |text| format!(" · filter: {text}"))
    }

    /// The children of `dir` in the sort, those the mask keeps: worked out
    /// once for each folder and sort.
    #[must_use]
    pub fn children(&self, ctx: &Context<'_>, dir: u32) -> Rc<Vec<u32>> {
        Rc::clone(&self.listed(ctx, dir).ids)
    }

    /// [`Viewing::children`], with their sizes added up as they run.
    #[must_use]
    pub fn listed(&self, ctx: &Context<'_>, dir: u32) -> Rc<Children> {
        let key = (dir, self.sort);
        if let Some(found) = self.rows.list(key) {
            return found;
        }
        let listed = Rc::new(self.list(ctx, dir));
        self.rows.keep_list(key, Rc::clone(&listed));
        listed
    }

    /// The children of `dir`, read in id order, the order the index keeps
    /// them in, so that whatever the sort a block of entries is read once:
    /// each one's size, and whether the mask keeps it, within one budget
    /// for the list. Then sorted: by size from what was read; in another
    /// sort as `sort_ids` sorts them, which reads them again in the same
    /// order.
    fn list(&self, ctx: &Context<'_>, dir: u32) -> Children {
        let (view, size) = (ctx.view, ctx.opts.size);
        let node = view.node(dir);
        let mut budget = WALK_BUDGET;
        let mut read: Vec<(u32, u64)> = if node.is_dir() {
            children(view, &node)
                .filter_map(|id| {
                    let kid = view.node(id);
                    let shows = self
                        .mask
                        .is_none_or(|mask| mask.shows(ctx, &kid, &mut budget));
                    shows.then(|| (id, kid.size(size)))
                })
                .collect()
        } else {
            Vec::new()
        };
        let sorted = if self.sort == Sort::Size {
            // Stable: equals keep their order, as `path::siblings` has it.
            read.sort_by_key(|&(_, bytes)| Reverse(bytes));
            read
        } else {
            let mut ids: Vec<u32> = read.iter().map(|&(id, _)| id).collect();
            sort_ids(view, &mut ids, self.sort.key(), size, false);
            read.sort_unstable_by_key(|&(id, _)| id);
            ids.into_iter()
                .map(|id| {
                    let at = read.binary_search_by_key(&id, |&(id, _)| id);
                    (id, at.map_or(0, |at| read[at].1))
                })
                .collect()
        };
        let mut sums = Vec::with_capacity(sorted.len() + 1);
        let mut total = 0u64;
        sums.push(total);
        for &(_, bytes) in &sorted {
            total = total.saturating_add(bytes);
            sums.push(total);
        }
        Children {
            ids: Rc::new(sorted.into_iter().map(|(id, _)| id).collect()),
            sums,
        }
    }
}

/// A folder's children in a sort, those the mask keeps, and the running
/// total of their sizes: what a `… n more` row adds up, without reading
/// them again.
#[derive(Debug)]
pub struct Children {
    /// The ids, in the sort.
    pub ids: Rc<Vec<u32>>,
    /// The bytes of the first `i` ids, at `i`: one more than the ids.
    sums: Vec<u64>,
}

impl Children {
    /// The bytes of the ids in `range`.
    #[must_use]
    pub fn bytes(&self, range: Range<usize>) -> u64 {
        let last = self.sums.len().saturating_sub(1);
        let sum = |i: usize| self.sums.get(i.min(last)).copied().unwrap_or(0);
        sum(range.end).saturating_sub(sum(range.start))
    }
}

/// A folder's children in a sort, as the lists keep them.
type Listed = ((u32, Sort), Rc<Children>);

/// The rows the tables have worked out: a list of the folders visited, the
/// largest files and the classes of the map's root, each read once and
/// kept while the root and the mask stay the same.
#[derive(Debug, Default)]
pub struct Rows {
    lists: RefCell<Vec<Listed>>,
    largest: RefCell<Option<(u32, largest::Found)>>,
    types: RefCell<Option<(u32, Rc<Vec<types::Row>>)>>,
}

impl Rows {
    fn list(&self, key: (u32, Sort)) -> Option<Rc<Children>> {
        self.lists
            .borrow()
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, ids)| Rc::clone(ids))
    }

    fn keep_list(&self, key: (u32, Sort), ids: Rc<Children>) {
        let mut lists = self.lists.borrow_mut();
        if lists.len() >= KEPT_LISTS {
            lists.remove(0);
        }
        lists.push((key, ids));
    }

    /// Forgets the lists and the largest files: the mask changed.
    pub fn forget(&self) {
        self.lists.borrow_mut().clear();
        *self.largest.borrow_mut() = None;
    }
}

/// A pane title for `pane`: `head · <path> · tail`, the middle `path` cut
/// from the front so that the head and the tail are seen whole.
#[must_use]
pub fn path_title(canvas: &Canvas<'_>, pane: Rect, head: &str, path: &str, tail: &str) -> String {
    // The room a pane's title has inside its corners and padding.
    let room = pane.width.saturating_sub(6);
    let used = canvas.width(head) + canvas.width(tail);
    let path = canvas.cut_left(path, room.saturating_sub(used));
    format!("{head}{path}{tail}")
}

/// `text` with no control characters and no newlines: what a table may
/// print on one row of the screen.
#[must_use]
pub fn one_line(canvas: &Canvas<'_>, text: &str, room: u16) -> String {
    canvas.cut(&text.replace(['\n', '\r', '\t'], " "), room)
}

#[cfg(test)]
pub(crate) mod tests;
