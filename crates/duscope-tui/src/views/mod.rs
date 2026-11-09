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
use std::rc::Rc;

use duscope_core::query::{SortKey, sort_ids};
use ratatui_core::layout::Rect;

use crate::mask::Mask;
use crate::panes::Context;
use crate::path::siblings;
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
        let key = (dir, self.sort);
        if let Some(found) = self.rows.list(key) {
            return found;
        }
        let mut ids = if self.sort == Sort::Size {
            siblings(ctx.view, dir, ctx.opts.size)
        } else {
            let mut ids = siblings(ctx.view, dir, ctx.opts.size);
            sort_ids(ctx.view, &mut ids, self.sort.key(), ctx.opts.size, false);
            ids
        };
        if let Some(mask) = self.mask {
            ids.retain(|&id| mask.shows(ctx, id));
        }
        let ids = Rc::new(ids);
        self.rows.keep_list(key, Rc::clone(&ids));
        ids
    }
}

/// A folder's children in a sort, as a table keeps them.
type Listed = ((u32, Sort), Rc<Vec<u32>>);

/// The rows the tables have worked out: a list of the folders visited, the
/// largest files and the classes of the map's root, each read once and
/// kept while the root and the mask stay the same.
#[derive(Debug, Default)]
pub struct Rows {
    lists: RefCell<Vec<Listed>>,
    largest: RefCell<Option<(u32, Rc<Vec<u32>>)>>,
    types: RefCell<Option<(u32, Rc<Vec<types::Row>>)>>,
}

impl Rows {
    fn list(&self, key: (u32, Sort)) -> Option<Rc<Vec<u32>>> {
        self.lists
            .borrow()
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, ids)| Rc::clone(ids))
    }

    fn keep_list(&self, key: (u32, Sort), ids: Rc<Vec<u32>>) {
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
