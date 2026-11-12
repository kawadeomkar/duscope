//! The selected path: the entries from the scan's root down to an item,
//! and the cursor, one of them, at the cursor depth. This is
//! `GrandPerspective`'s path model made keyboard-first: `⌫` moves the
//! cursor up the path and `⏎` back down it, so the part below the cursor
//! is remembered until the cursor moves sideways; where the path ends at
//! the cursor, `⏎` takes the folder's largest child. The visible root, the
//! third part of the model, is the zoom's ([`crate::zoom`]).
//!
//! A folder's children are read as the queries read them: its files are
//! the ids after it up to `files_end`, and its subfolders one query on the
//! index's `dirs` (`TreeView::child_dirs`), so descending reads the folder
//! descended into and nothing under it.

use duscope_core::query::{SizeKind, TreeView, children};

/// The selected path and the cursor on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Path {
    /// From the scan's root (0) down to the item, each the parent of the
    /// next: the entry at index `d` is at depth `d`.
    ids: Vec<u32>,
    /// The cursor's index in `ids`: its depth.
    at: usize,
}

impl Default for Path {
    /// The root alone, the cursor on it.
    fn default() -> Path {
        Path {
            ids: vec![0],
            at: 0,
        }
    }
}

impl Path {
    /// The path the viewer opens on: down from the root through the
    /// largest child at every level to the largest file, the cursor on it.
    #[must_use]
    pub fn largest(view: &dyn TreeView, size: SizeKind) -> Path {
        let mut path = Path::default();
        if !view.is_empty() {
            while path.down(view, size) {}
        }
        path
    }

    /// The path from the root to `id`, the cursor on `id`.
    #[must_use]
    pub fn to(view: &dyn TreeView, id: u32) -> Path {
        let mut ids = vec![id];
        let mut at = id;
        while at != 0 {
            let parent = view.node(at).parent;
            // A view's root is its own parent; an entry an index could not
            // read is a child of the root.
            if parent >= at {
                break;
            }
            ids.push(parent);
            at = parent;
        }
        if ids.last() != Some(&0) {
            ids.push(0);
        }
        ids.reverse();
        let at = ids.len() - 1;
        Path { ids, at }
    }

    /// The entry under the cursor.
    #[must_use]
    pub fn cursor(&self) -> u32 {
        self.ids[self.at]
    }

    /// The cursor depth.
    #[cfg(test)]
    pub fn depth(&self) -> usize {
        self.at
    }

    /// The path, root first, down to the item.
    #[cfg(test)]
    pub fn ids(&self) -> &[u32] {
        &self.ids
    }

    /// The cursor's ancestors, root first.
    #[must_use]
    pub fn ancestors(&self) -> &[u32] {
        &self.ids[..self.at]
    }

    /// The cursor's parent; `None` on the root.
    #[must_use]
    pub fn parent(&self) -> Option<u32> {
        self.at.checked_sub(1).map(|i| self.ids[i])
    }

    /// Moves the cursor up to its parent, remembering the path below;
    /// false on the root.
    pub fn up(&mut self) -> bool {
        if self.at == 0 {
            return false;
        }
        self.at -= 1;
        true
    }

    /// Moves the cursor down: along the path where it goes on below the
    /// cursor, else to the largest child of the folder under the cursor;
    /// false on a file or an empty folder.
    pub fn down(&mut self, view: &dyn TreeView, size: SizeKind) -> bool {
        if self.at + 1 < self.ids.len() {
            self.at += 1;
            return true;
        }
        match largest_child(view, self.cursor(), size) {
            Some(child) => {
                self.ids.push(child);
                self.at += 1;
                true
            }
            None => false,
        }
    }

    /// Puts the cursor on `id`, one of its siblings or itself: the path
    /// then ends there.
    pub fn select(&mut self, id: u32) {
        self.ids.truncate(self.at);
        self.ids.push(id);
    }

    /// Puts the cursor on `id`, anywhere in the scan: on the path where
    /// `id` is on it (the path below kept), else on a new path to `id`.
    pub fn jump(&mut self, view: &dyn TreeView, id: u32) {
        let depth = view.node(id).depth as usize;
        if self.ids.get(depth) == Some(&id) {
            self.at = depth;
        } else {
            *self = Path::to(view, id);
        }
    }
}

/// The children of `dir`, largest first by `size` and in their order
/// among equals, as the siblings pane lists them without a mask; none for
/// a file. The viewer reads them through [`crate::views::Viewing::listed`],
/// which keeps them; the tests check its order against this.
#[cfg(test)]
#[must_use]
pub fn siblings(view: &dyn TreeView, dir: u32, size: SizeKind) -> Vec<u32> {
    let node = view.node(dir);
    if !node.is_dir() {
        return Vec::new();
    }
    let mut kids: Vec<(u64, u32)> = children(view, &node)
        .map(|id| (view.node(id).size(size), id))
        .collect();
    // Stable: equals keep their order.
    kids.sort_by_key(|&(bytes, _)| std::cmp::Reverse(bytes));
    kids.into_iter().map(|(_, id)| id).collect()
}

/// The largest child of `dir` by `size`, the first of equals; `None` for
/// a file or an empty folder.
#[must_use]
pub fn largest_child(view: &dyn TreeView, dir: u32, size: SizeKind) -> Option<u32> {
    let node = view.node(dir);
    if !node.is_dir() {
        return None;
    }
    children(view, &node)
        .map(|id| view.node(id))
        .reduce(|best, n| {
            if n.size(size) > best.size(size) {
                n
            } else {
                best
            }
        })
        .map(|n| n.id)
}
