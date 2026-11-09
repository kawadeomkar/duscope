//! The zoom: the map's visible root, the stack of what `z` replaced, and
//! the maps already painted, one for each root at the map's size.
//!
//! `z` makes the folder under the cursor (a file's folder) the visible
//! root, pushing the root and the cursor it replaced; `Z` pops them back;
//! `0` pops them all. A zoomed map is the map of its root, as `view` of
//! that folder would draw it (its children the titled boxes, `--depth`
//! counted from it), in the colours the whole map gives its tiles
//! (`--color-by folder` and `level` keep the scan's top-level folders and
//! depths).
//!
//! A [`Map`] is painted once for a root and a size and kept: moving the
//! cursor redraws only the marks on it, and zooming back out finds the
//! outer map where it was left. A resize, which changes every layout,
//! forgets them all; so must anything that changes how the map is painted
//! (the colour mode, the pack). With `--treemap pixels` the pictures are
//! kept the same way ([`Layouts`] of them), fewer, since each holds its
//! pixels: the scan root's and those of the three roots visited last.

use std::collections::HashMap;
use std::rc::Rc;

use duscope_core::query::{Subtree, TreeView};
use duscope_treemap::Rect;
use duscope_treemap::cells::CellBuffer;
use duscope_treemap::paint::{
    Base, Drawn, DrawnKind, classes_of, paint_viewer, paint_viewer_labelled,
};

use crate::panes::Context;

/// The maps kept at one size: the roots visited last.
const KEPT: usize = 16;

/// The pictures kept at one size (`--treemap pixels`): the scan root's and
/// the three other roots visited last.
pub const KEPT_PICTURES: usize = 4;

/// Where the map of `root` stands in the scan, as `ctx` paints it: its
/// depth, and below the scan's root the `--color-by folder` place of the
/// top-level folder it is in, so that a zoomed map, in cells or in pixels,
/// keeps the whole map's colours.
#[must_use]
pub fn base(ctx: &Context<'_>, root: u32) -> Base {
    let view = ctx.view;
    if root == 0 || view.is_empty() {
        return Base::default();
    }
    let node = view.node(root);
    let mut top = node;
    while top.depth > 1 {
        top = view.node(top.parent);
    }
    Base {
        depth: node.depth,
        folder: ctx.ranks.by_id.get(&top.id).copied(),
    }
}

/// The visible root and the zoom stack.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Zoom {
    /// The map's root.
    root: u32,
    /// What each `z` replaced: the root, and the cursor at the time.
    stack: Vec<(u32, u32)>,
}

impl Zoom {
    /// The map's root.
    #[must_use]
    pub fn root(&self) -> u32 {
        self.root
    }

    /// How many zooms deep the map is.
    #[cfg(test)]
    pub fn depth(&self) -> usize {
        self.stack.len()
    }

    /// Makes `root` the map's root, remembering the root it replaces and
    /// the `cursor` there.
    pub fn push(&mut self, root: u32, cursor: u32) {
        self.stack.push((self.root, cursor));
        self.root = root;
    }

    /// Goes back out one zoom: the root and the cursor `z` replaced, now
    /// the root again; `None` at the scan's root.
    pub fn pop(&mut self) -> Option<(u32, u32)> {
        let (root, cursor) = self.stack.pop()?;
        self.root = root;
        Some((root, cursor))
    }

    /// Goes back out to the scan's root: what the first `z` replaced;
    /// `None` at the scan's root.
    pub fn reset(&mut self) -> Option<(u32, u32)> {
        let first = *self.stack.first()?;
        self.stack.clear();
        self.root = first.0;
        Some(first)
    }
}

/// Where the map shows an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    /// It is the map's root: the whole map.
    Whole,
    /// The tile at this index of [`Map::tiles`]: the entry's own, or the
    /// tile that holds it (a pool, or a folder too small to open).
    Tile(usize),
}

/// A map painted for a root at a size, and the tiles on it: the cell map,
/// or with `--treemap pixels` the picture's tiles, in pixels.
#[derive(Debug)]
pub struct Map {
    /// The root.
    root: u32,
    /// The cells, as wide and tall as the map; none for a picture's.
    pub cells: CellBuffer,
    /// The whole map: in half-cell units, or a picture's in pixels.
    area: Rect,
    /// The tiles drawn, with the scan's ids.
    tiles: Vec<Drawn>,
    /// The tile of each entry drawn as its own.
    own: HashMap<u32, usize>,
    /// The tiles of each folder's block: its children's, its pool, its
    /// files'. A folder whose block was laid out has an entry.
    blocks: HashMap<u32, Vec<usize>>,
}

impl Map {
    /// The map of `root` (a folder, or the scan's root) at `w`×`h` cells,
    /// as `ctx` paints it, with `t`'s labels when `labelled`.
    #[must_use]
    pub fn paint(ctx: &Context<'_>, root: u32, w: u16, h: u16, labelled: bool) -> Map {
        let view = ctx.view;
        let base = base(ctx, root);
        let (cells, drawn) = if root == 0 || view.is_empty() {
            if labelled {
                paint_viewer_labelled(view, ctx.opts, w, h, ctx.class_of, base)
            } else {
                paint_viewer(view, ctx.opts, w, h, ctx.class_of, base)
            }
        } else {
            let node = view.node(root);
            let sub = Subtree::new(view, root);
            let range = root as usize..=node.last_id as usize;
            let classes;
            let class_of = if let Some(found) = ctx.class_of.get(range) {
                found
            } else {
                classes = classes_of(&sub, ctx.opts.classes);
                &classes
            };
            if labelled {
                paint_viewer_labelled(&sub, ctx.opts, w, h, class_of, base)
            } else {
                paint_viewer(&sub, ctx.opts, w, h, class_of, base)
            }
        };
        let area = Rect::new(0, 0, cells.width(), cells.height() * 2);
        Map::of(root, cells, area, drawn)
    }

    /// The map of `root` whose tiles are `drawn`, with the ids of the
    /// subtree at `root` (the scan's where `root` is 0), over `area`: the
    /// cell map in `cells`, or a picture's tiles (with no cells).
    #[must_use]
    pub fn of(
        root: u32,
        cells: CellBuffer,
        area: Rect,
        drawn: impl IntoIterator<Item = Drawn>,
    ) -> Map {
        let tiles: Vec<Drawn> = drawn
            .into_iter()
            .map(|d| Drawn {
                id: d.id + root,
                parent: d.parent + root,
                ..d
            })
            .collect();
        let mut own = HashMap::new();
        let mut blocks: HashMap<u32, Vec<usize>> = HashMap::new();
        for (i, tile) in tiles.iter().enumerate() {
            if matches!(
                tile.kind,
                DrawnKind::Leaf | DrawnKind::Box { .. } | DrawnKind::Gutter
            ) {
                own.insert(tile.id, i);
            }
            // A block's tiles are drawn, and pushed, in the order they
            // were laid out.
            blocks.entry(tile.parent).or_default().push(i);
        }
        Map {
            root,
            cells,
            area,
            tiles,
            own,
            blocks,
        }
    }

    /// The root it was painted for.
    #[must_use]
    pub fn root(&self) -> u32 {
        self.root
    }

    /// Its tiles.
    #[must_use]
    pub fn tiles(&self) -> &[Drawn] {
        &self.tiles
    }

    /// The whole map, in half-cell units (a picture's in pixels).
    #[must_use]
    pub fn area(&self) -> Rect {
        self.area
    }

    /// The tile drawn as `id`'s own.
    #[must_use]
    pub fn own(&self, id: u32) -> Option<usize> {
        self.own.get(&id).copied()
    }

    /// The tiles of `dir`'s block, its children laid out inside it; empty
    /// where it was not opened (too small, at the `--depth` cap, outside
    /// the map).
    #[must_use]
    pub fn block(&self, dir: u32) -> &[usize] {
        self.blocks.get(&dir).map_or(&[], Vec::as_slice)
    }

    /// Where the map shows `id`, an entry under its root: its own tile; in
    /// a folder whose block was laid out, the pool it went into (a file
    /// under `--pool-files`, the files' tile); else wherever its folder is
    /// shown.
    #[must_use]
    pub fn mark(&self, view: &dyn TreeView, id: u32) -> Mark {
        let mut at = id;
        loop {
            if at == self.root || at == 0 {
                return Mark::Whole;
            }
            if let Some(i) = self.own(at) {
                return Mark::Tile(i);
            }
            let node = view.node(at);
            let block = self.block(node.parent);
            let find = |kind| block.iter().copied().find(|&i| self.tiles[i].kind == kind);
            // A file under `--pool-files` is in the files' tile when it has
            // one, and that tile in the pool when it does not.
            let files = if node.is_dir() {
                None
            } else {
                find(DrawnKind::Files)
            };
            if let Some(i) = files.or_else(|| find(DrawnKind::Small)) {
                return Mark::Tile(i);
            }
            if node.parent >= at {
                return Mark::Whole;
            }
            at = node.parent;
        }
    }

    /// The rectangle of `mark`, in half-cell units.
    #[must_use]
    pub fn rect(&self, mark: Mark) -> Rect {
        match mark {
            Mark::Whole => self.area(),
            Mark::Tile(i) => self.tiles[i].rect,
        }
    }
}

/// The maps painted at the map's current size, the roots visited last
/// ([`KEPT`]); or the pictures (`--treemap pixels`), the scan root's kept
/// whatever else is ([`Layouts::pinned`]).
#[derive(Debug)]
pub struct Layouts<T = Map> {
    /// The size they were painted at.
    size: (u16, u16),
    /// Each with its root, the latest first.
    maps: Vec<(u32, Rc<T>)>,
    /// How many are kept.
    bound: usize,
    /// Whether the scan root's is kept while another is forgotten.
    pin: bool,
}

impl<T> Default for Layouts<T> {
    fn default() -> Self {
        Layouts {
            size: (0, 0),
            maps: Vec::new(),
            bound: KEPT,
            pin: false,
        }
    }
}

impl<T> Layouts<T> {
    /// At most `bound` kept, the scan root's among them once painted: when
    /// one more is painted, the least recent of the others is forgotten.
    #[must_use]
    pub fn pinned(bound: usize) -> Self {
        Layouts {
            bound: bound.max(2),
            pin: true,
            ..Layouts::default()
        }
    }

    /// The map of `root` at `w`×`h`: the one kept, or one `paint` paints
    /// (and keeps). Another size forgets the maps kept.
    pub fn get(&mut self, root: u32, w: u16, h: u16, paint: impl FnOnce() -> T) -> Rc<T> {
        if self.size != (w, h) {
            self.maps.clear();
            self.size = (w, h);
        }
        if let Some(i) = self.maps.iter().position(|(at, _)| *at == root) {
            let kept = self.maps.remove(i);
            let map = Rc::clone(&kept.1);
            self.maps.insert(0, kept);
            return map;
        }
        let map = Rc::new(paint());
        self.maps.insert(0, (root, Rc::clone(&map)));
        if self.maps.len() > self.bound {
            let last = if self.pin {
                self.maps.iter().rposition(|(at, _)| *at != 0)
            } else {
                Some(self.maps.len() - 1)
            };
            if let Some(i) = last {
                self.maps.remove(i);
            }
        }
        map
    }

    /// Forgets the maps kept: something changed how the map is painted.
    pub fn clear(&mut self) {
        self.maps.clear();
    }

    /// The size they were painted at.
    #[cfg(test)]
    pub fn size(&self) -> (u16, u16) {
        self.size
    }

    /// How many maps are kept.
    #[cfg(test)]
    pub fn kept(&self) -> usize {
        self.maps.len()
    }
}
