//! The static treemap: a scan painted into a [`CellBuffer`] by the drawing
//! rules of the proposal's design system.
//!
//! The map is laid out in half-cell units (a column wide, half a row tall),
//! squarified level by level until a tile is smaller than `--min-tile`
//! units, as `GrandPerspective` nests to file depth. Each directory's tile
//! is, by size:
//!
//! - a depth-1 folder of at least 6×3 cells: an outlined box titled
//!   `name · size` on its top border, its children inside a one-cell inset;
//! - a deeper folder of at least 7×4 cells: an outlined box without a title;
//! - at least 3×2 cells: a gutter group, a one-unit strip along its top and
//!   a one-column strip along its left in the crust colour, its children in
//!   the rest;
//! - smaller: a leaf in the colour of the class holding most of its bytes.
//!
//! Border colours step overlay1, surface2, surface1 by depth. Leaves fill
//! their tile with their colour, every second sibling mixed 20% toward the
//! base. A leaf of at least 5 columns is labelled `  name `, and one of at
//! least 8×2 cells gets a second row `  size `, in crust on a light fill and
//! in the text colour otherwise. What rounds below `--min-tile` is pooled
//! into the parent's `+N small` tile. With the `no-color` pack tiles are
//! `░▒▓█` shades by depth and labels carry the class letter.

use duscope_core::query::{Node, SizeKind, TreeView, children};
use duscope_core::rules::{Class, Classes, ext};

use crate::cells::{Cell, CellBuffer, Color};
use crate::packs::Pack;
use crate::{Algorithm, Rect, layout};

/// What colours the tiles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ColorBy {
    /// The file class (the default).
    Type,
    /// The top-level folder, through the pack's cycle.
    Folder,
    /// The depth, through the pack's cycle.
    Level,
    /// The newest modification time, through the pack's age ramp.
    Age,
    /// `LS_COLORS`' extension colours, the class colour where it has none.
    LsColors(Vec<(String, Color)>),
}

/// The characters a map is drawn with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Glyphs {
    /// Box drawing, half blocks and `░▒▓█`.
    Unicode,
    /// `+-|`, whole cells and `#=-+` shades, for terminals without UTF-8.
    Ascii,
}

/// How a map is painted.
#[derive(Debug, Clone)]
pub struct PaintOpts<'a> {
    /// The colours.
    pub pack: &'a Pack,
    /// What colours the tiles.
    pub color_by: ColorBy,
    /// The characters.
    pub glyphs: Glyphs,
    /// `--depth`: folders at this depth are painted as leaves.
    pub depth: Option<u32>,
    /// `--min-tile`: the smallest tile side drawn, in units.
    pub min_tile: u16,
    /// `--pool-files`: each directory's files make one tile.
    pub pool_files: bool,
    /// The size tiles are weighed by.
    pub size: SizeKind,
    /// Now, for `--color-by age`.
    pub now: i64,
    /// The classes table.
    pub classes: &'a Classes,
}

/// One half-cell unit of the map.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Unit {
    color: Color,
    /// The depth that painted it, for the `no-color` shades.
    depth: u8,
}

/// The painter's state: the units, the overlay of border and label cells,
/// and each entry's class.
struct Painter<'v, 'o, V: TreeView + ?Sized> {
    view: &'v V,
    opts: &'o PaintOpts<'o>,
    w: u16,
    h_units: u16,
    units: Vec<Unit>,
    overlay: Vec<Option<Cell>>,
    /// Each entry's class, by pre-order id.
    class_of: Vec<Class>,
}

/// Paints the scan in `view` as a map of `w` columns and `h` rows.
pub fn paint(view: &(impl TreeView + ?Sized), opts: &PaintOpts<'_>, w: u16, h: u16) -> CellBuffer {
    let h_units = h.saturating_mul(2);
    let mut painter = Painter {
        view,
        opts,
        w,
        h_units,
        units: vec![
            Unit {
                color: opts.pack.roles.base,
                depth: 0,
            };
            usize::from(w) * usize::from(h_units)
        ],
        overlay: vec![None; usize::from(w) * usize::from(h)],
        class_of: classes_of(view, opts.classes),
    };
    if !view.is_empty() && w > 0 && h > 0 {
        let root = view.node(0);
        let area = Rect::new(0, 0, w, h_units);
        if root.is_dir() {
            painter.block(&root, area, 0, None);
        } else {
            painter.leaf(&root, area, 1, 0, None);
        }
    }
    painter.compose(h)
}

/// Each entry's class: a directory's is that of the outermost directory
/// rule above or at it (`Other` when none), a file's that rule's or its
/// extension's.
fn classes_of(view: &(impl TreeView + ?Sized), classes: &Classes) -> Vec<Class> {
    let n = view.len() as usize;
    let mut out = vec![Class::Other; n];
    if n == 0 {
        return out;
    }
    let root = view.node(0);
    let mut components: Vec<&[u8]> = Vec::new();
    let mut open: Vec<(u32, Option<Class>)> = Vec::new();
    for id in 0..=root.last_id {
        let node = view.node(id);
        while open.last().is_some_and(|&(last, _)| id > last) {
            open.pop();
            components.pop();
        }
        let inherited = open.last().and_then(|&(_, class)| class);
        if node.is_dir() {
            components.push(node.name);
            let class = inherited.or_else(|| classes.of_dir(&components));
            open.push((node.last_id, class));
            out[id as usize] = class.unwrap_or(Class::Other);
            continue;
        }
        out[id as usize] = inherited.unwrap_or_else(|| {
            let suffix = ext(node.name);
            if suffix.is_empty() {
                if node.mode.is_some_and(|m| m & 0o111 != 0) {
                    Class::Binaries
                } else {
                    Class::Other
                }
            } else {
                classes.of_ext(&suffix).unwrap_or(Class::Other)
            }
        });
    }
    out
}

/// A tile to place: an entry, or the pooled files of a directory.
#[derive(Debug, Clone, Copy)]
enum Item {
    Entry(u32),
    /// A directory's files, under `--pool-files`: the directory's id, the
    /// number of files.
    Files(u32, u64),
}

impl<V: TreeView + ?Sized> Painter<'_, '_, V> {
    /// Lays out `dir`'s children in `area` (units) and paints each; `depth`
    /// is `dir`'s, `folder` the index of the top-level folder above.
    fn block(&mut self, dir: &Node<'_>, area: Rect, depth: u32, folder: Option<usize>) {
        if area.w == 0 || area.h == 0 {
            return;
        }
        let size = self.opts.size;
        let mut items: Vec<Item> = Vec::new();
        let mut weights: Vec<(u64, usize)> = Vec::new();
        let mut pooled_files = (0u64, 0u64);
        for id in children(self.view, dir) {
            let node = self.view.node(id);
            if self.opts.pool_files && !node.is_dir() {
                pooled_files.0 += node.size(size);
                pooled_files.1 += 1;
                continue;
            }
            weights.push((node.size(size), items.len()));
            items.push(Item::Entry(id));
        }
        if pooled_files.1 > 0 {
            weights.push((pooled_files.0, items.len()));
            items.push(Item::Files(dir.id, pooled_files.1));
        }
        let laid = layout(
            &weights,
            area,
            Algorithm::Squarify,
            self.opts.min_tile.max(1),
        );
        for (order, tile) in laid.tiles.iter().enumerate() {
            let tint = order % 2 == 1;
            match items[tile.id] {
                Item::Entry(id) => {
                    let node = self.view.node(id);
                    let folder = folder.or((depth == 0).then_some(order));
                    if node.is_dir() && self.opts.depth.is_none_or(|cap| node.depth < cap) {
                        self.folder(&node, tile.rect, folder, tint);
                    } else {
                        self.leaf(&node, tile.rect, node.depth, order, folder);
                    }
                }
                Item::Files(dir_id, count) => {
                    let class = self.dominant(dir_id, true);
                    let color = self.tinted(self.opts.pack.class(class), tint);
                    self.fill(tile.rect, color, clamp_depth(depth + 1));
                    let label = format!("{count} files");
                    self.label(tile.rect, &label, None, color, class);
                }
            }
        }
        if let Some(small) = laid.small {
            let color = self.opts.pack.class(Class::Other);
            self.fill(small.rect, color, clamp_depth(depth + 1));
            let label = format!("+{} small", small.ids.len());
            self.label(small.rect, &label, None, color, Class::Other);
        }
    }

    /// Paints directory `node` in `rect`: a box, a gutter group or a leaf,
    /// by its size in cells.
    fn folder(&mut self, node: &Node<'_>, rect: Rect, folder: Option<usize>, tint: bool) {
        let (top, rows) = cell_rows(rect);
        let cols = rect.w;
        let depth = node.depth;
        let boxed = if depth <= 1 {
            cols >= 6 && rows >= 3
        } else {
            cols >= 7 && rows >= 4
        };
        if boxed {
            // The box's own background, with the half units its cells do
            // not cover.
            self.fill(rect, self.opts.pack.roles.base, clamp_depth(depth));
            let border = match depth {
                0 | 1 => self.opts.pack.roles.overlay1,
                2 => self.opts.pack.roles.surface2,
                _ => self.opts.pack.roles.surface1,
            };
            self.outline(rect.x, top, cols, rows, border);
            if depth <= 1 {
                self.title(node, rect.x, top, cols);
            }
            let inner = Rect::new(
                rect.x + 1,
                (top + 1) * 2,
                cols.saturating_sub(2),
                rows.saturating_sub(2) * 2,
            );
            self.block(node, inner, depth, folder);
        } else if cols >= 3 && rows >= 2 {
            let crust = self.opts.pack.roles.crust;
            self.fill(
                Rect::new(rect.x, rect.y, rect.w, 1),
                crust,
                clamp_depth(depth),
            );
            self.fill(
                Rect::new(rect.x, rect.y, 1, rect.h),
                crust,
                clamp_depth(depth),
            );
            let inner = Rect::new(rect.x + 1, rect.y + 1, rect.w - 1, rect.h - 1);
            self.block(node, inner, depth, folder);
        } else {
            let order = usize::from(tint);
            self.leaf(node, rect, depth, order, folder);
        }
    }

    /// Paints `node` as one tile: a file, or a folder too small to open.
    fn leaf(
        &mut self,
        node: &Node<'_>,
        rect: Rect,
        depth: u32,
        order: usize,
        folder: Option<usize>,
    ) {
        let class = if node.is_dir() {
            self.dominant(node.id, false)
        } else {
            self.class_of[node.id as usize]
        };
        let pack = self.opts.pack;
        let base = match &self.opts.color_by {
            ColorBy::Type => pack.class(class),
            ColorBy::Folder => pack.cycle[folder.unwrap_or(0) % pack.cycle.len()],
            ColorBy::Level => pack.cycle[depth as usize % pack.cycle.len()],
            ColorBy::Age => pack.age[age_step(self.opts.now, node.max_mtime_ns)],
            ColorBy::LsColors(table) => {
                let suffix = ext(node.name);
                table
                    .iter()
                    .find(|(e, _)| *e == suffix)
                    .map_or_else(|| pack.class(class), |&(_, c)| c)
            }
        };
        let color = self.tinted(base, order % 2 == 1);
        self.fill(rect, color, clamp_depth(depth));
        let mut name = String::from_utf8_lossy(node.name).into_owned();
        if node.is_dir() {
            name.push('/');
        }
        let size = compact(node.size(self.opts.size));
        self.label(rect, &name, Some(&size), color, class);
    }

    /// The class holding most of the bytes under directory `id`; only its
    /// own files with `own_files`.
    fn dominant(&self, id: u32, own_files: bool) -> Class {
        let dir = self.view.node(id);
        let last = if own_files {
            dir.files_end
        } else {
            dir.last_id
        };
        let mut bytes = [0u64; 7];
        for child in id + 1..=last {
            let node = self.view.node(child);
            if !node.is_dir() && node.charged {
                let i = Class::ALL
                    .iter()
                    .position(|&c| c == self.class_of[child as usize])
                    .unwrap_or(6);
                bytes[i] += node.size(self.opts.size);
            }
        }
        let best = (0..7)
            .max_by_key(|&i| (bytes[i], usize::MAX - i))
            .unwrap_or(6);
        if bytes[best] == 0 {
            Class::Other
        } else {
            Class::ALL[best]
        }
    }

    /// `color`, mixed 20% toward the base for every second sibling.
    fn tinted(&self, color: Color, tint: bool) -> Color {
        if tint {
            color.mix(self.opts.pack.roles.base, 20)
        } else {
            color
        }
    }

    /// Fills the units of `rect` with `color`.
    fn fill(&mut self, rect: Rect, color: Color, depth: u8) {
        for y in rect.y..rect.y.saturating_add(rect.h).min(self.h_units) {
            for x in rect.x..rect.x.saturating_add(rect.w).min(self.w) {
                self.units[usize::from(y) * usize::from(self.w) + usize::from(x)] =
                    Unit { color, depth };
            }
        }
    }

    /// Sets the overlay cell at column `x`, row `y`.
    fn put(&mut self, x: u16, y: u16, cell: Cell) {
        if x < self.w && y < self.h_units / 2 {
            self.overlay[usize::from(y) * usize::from(self.w) + usize::from(x)] = Some(cell);
        }
    }

    /// Outlines the cells from column `x`, row `top`, `cols` by `rows`.
    fn outline(&mut self, x: u16, top: u16, cols: u16, rows: u16, color: Color) {
        let (h, v, corners) = match self.opts.glyphs {
            Glyphs::Unicode => ('─', '│', ['┌', '┐', '└', '┘']),
            Glyphs::Ascii => ('-', '|', ['+', '+', '+', '+']),
        };
        let base = self.opts.pack.roles.base;
        let cell = |ch| Cell {
            ch,
            fg: color,
            bg: base,
            bold: false,
        };
        let (right, bottom) = (x + cols - 1, top + rows - 1);
        for cx in x + 1..right {
            self.put(cx, top, cell(h));
            self.put(cx, bottom, cell(h));
        }
        for cy in top + 1..bottom {
            self.put(x, cy, cell(v));
            self.put(right, cy, cell(v));
        }
        self.put(x, top, cell(corners[0]));
        self.put(right, top, cell(corners[1]));
        self.put(x, bottom, cell(corners[2]));
        self.put(right, bottom, cell(corners[3]));
    }

    /// The title of a depth-1 box: ` name · size ` on its top border, bold
    /// on the base colour, the name alone (then truncated with `…`) when
    /// the whole title does not fit in the width less 6.
    fn title(&mut self, node: &Node<'_>, x: u16, top: u16, cols: u16) {
        let name = String::from_utf8_lossy(node.name).into_owned();
        let room = usize::from(cols.saturating_sub(6));
        let dot = match self.opts.glyphs {
            Glyphs::Unicode => "·",
            Glyphs::Ascii => "-",
        };
        let full = format!("{name} {dot} {}", compact(node.size(self.opts.size)));
        let text = if full.chars().count() <= room {
            full
        } else {
            truncate(&name, room, self.opts.glyphs)
        };
        if text.is_empty() {
            return;
        }
        let pack = self.opts.pack;
        let label = format!(" {text} ");
        for (i, ch) in label.chars().enumerate() {
            let Ok(i) = u16::try_from(i) else { break };
            self.put(
                x + 2 + i,
                top,
                Cell {
                    ch,
                    fg: pack.roles.text,
                    bg: pack.roles.base,
                    bold: true,
                },
            );
        }
    }

    /// Labels a tile: `  name ` on its first full row when it is at least 5
    /// columns wide, `  size ` on the next when it is at least 8×2 cells.
    /// With shades, the name carries the class letter.
    fn label(&mut self, rect: Rect, name: &str, size: Option<&str>, fill: Color, class: Class) {
        let (top, rows) = cell_rows(rect);
        if rect.w < 5 || rows == 0 {
            return;
        }
        let pack = self.opts.pack;
        let fg = if pack.shades {
            Color::Reset
        } else if fill.luminance().is_some_and(|l| l > 0.18) {
            pack.roles.crust
        } else {
            pack.roles.text
        };
        let room = usize::from(rect.w.saturating_sub(3));
        let name = if pack.shades {
            format!("{} {name}", class_key(class))
        } else {
            name.to_owned()
        };
        let mut lines = vec![truncate(&name, room, self.opts.glyphs)];
        if let Some(size) = size
            && rect.w >= 8
            && rows >= 2
        {
            lines.push(truncate(size, room, self.opts.glyphs));
        }
        for (row, text) in lines.iter().enumerate() {
            let y = top + u16::try_from(row).unwrap_or(0);
            let line = format!("  {text} ");
            for (i, ch) in line.chars().take(usize::from(rect.w)).enumerate() {
                let Ok(i) = u16::try_from(i) else { break };
                let x = rect.x + i;
                let bg = if pack.shades {
                    Color::Reset
                } else {
                    self.units[usize::from(y * 2) * usize::from(self.w) + usize::from(x)].color
                };
                self.put(
                    x,
                    y,
                    Cell {
                        ch,
                        fg,
                        bg,
                        bold: false,
                    },
                );
            }
        }
    }

    /// The units and the overlay as cells: half blocks, whole cells or
    /// shades, the overlay on top.
    fn compose(&self, h: u16) -> CellBuffer {
        let mut buffer = CellBuffer::new(self.w, h);
        let shades = match self.opts.glyphs {
            Glyphs::Unicode => ['░', '▒', '▓', '█'],
            Glyphs::Ascii => ['-', '+', '=', '#'],
        };
        for y in 0..h {
            for x in 0..self.w {
                let i = usize::from(y) * usize::from(self.w) + usize::from(x);
                let top = self.units[usize::from(y * 2) * usize::from(self.w) + usize::from(x)];
                let bottom =
                    self.units[usize::from(y * 2 + 1) * usize::from(self.w) + usize::from(x)];
                let cell = if let Some(cell) = self.overlay[i] {
                    cell
                } else if self.opts.pack.shades {
                    let ch = if top.depth == 0 {
                        ' '
                    } else {
                        shades[usize::from(top.depth - 1) % shades.len()]
                    };
                    Cell {
                        ch,
                        ..Cell::default()
                    }
                } else if top.color == bottom.color || self.opts.glyphs == Glyphs::Ascii {
                    Cell {
                        ch: ' ',
                        fg: Color::Reset,
                        bg: top.color,
                        bold: false,
                    }
                } else {
                    Cell {
                        ch: '▀',
                        fg: top.color,
                        bg: bottom.color,
                        bold: false,
                    }
                };
                if let Some(slot) = buffer.get_mut(x, y) {
                    *slot = cell;
                }
            }
        }
        buffer
    }
}

/// The whole cell rows inside `rect`: the first, and how many.
fn cell_rows(rect: Rect) -> (u16, u16) {
    let top = rect.y.div_ceil(2);
    // The bottom edge, (y + h) / 2, without overflow.
    let bottom = rect.y.midpoint(rect.h);
    (top, bottom.saturating_sub(top))
}

fn clamp_depth(depth: u32) -> u8 {
    u8::try_from(depth.clamp(1, 255)).unwrap_or(u8::MAX)
}

/// The age ramp's step for a time: a day, a week, a month, a year, older.
fn age_step(now: i64, mtime: i64) -> usize {
    const DAY: i64 = 86_400_000_000_000;
    let age = now.saturating_sub(mtime);
    [DAY, 7 * DAY, 30 * DAY, 365 * DAY]
        .iter()
        .position(|&limit| age < limit)
        .unwrap_or(4)
}

/// A class's key letter, as the legend and the shades labels show it.
pub fn class_key(class: Class) -> char {
    match class {
        Class::Code => 'c',
        Class::Media => 'm',
        Class::Archives => 'a',
        Class::Data => 'd',
        Class::Binaries => 'b',
        Class::Docs => 'o',
        Class::Other => 'x',
    }
}

/// A size as the map prints it: `1.8G`, `932M`, `12K`, `512B`.
pub fn compact(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "K", "M", "G", "T", "P"];
    let mut unit = 0;
    let mut divisor = 1u64;
    while bytes / divisor >= 1024 && unit + 1 < UNITS.len() {
        divisor *= 1024;
        unit += 1;
    }
    if unit > 0 && bytes / divisor < 10 {
        let tenths = (u128::from(bytes) * 10 + u128::from(divisor) / 2) / u128::from(divisor);
        if tenths < 100 {
            return format!("{}.{}{}", tenths / 10, tenths % 10, UNITS[unit]);
        }
    }
    let whole = (u128::from(bytes) + u128::from(divisor) / 2) / u128::from(divisor);
    format!("{whole}{}", UNITS[unit])
}

/// `text` cut to `room` characters, the last one `…` (`~` in ASCII) when
/// cut.
fn truncate(text: &str, room: usize, glyphs: Glyphs) -> String {
    if text.chars().count() <= room {
        return text.to_owned();
    }
    if room == 0 {
        return String::new();
    }
    let mut cut: String = text.chars().take(room - 1).collect();
    cut.push(match glyphs {
        Glyphs::Unicode => '…',
        Glyphs::Ascii => '~',
    });
    cut
}

/// The lines under the map.
#[derive(Debug, Clone, Default)]
pub struct Footer {
    /// Each class's share of the scan, largest first.
    pub legend: Vec<(Class, f64)>,
    /// The scan's totals.
    pub summary: String,
    /// The first warning, if any.
    pub warning: Option<String>,
    /// The next commands.
    pub next: String,
}

/// The whole frame: the map of `w` columns and `rows − 5` rows, then the
/// legend, the summary, a warning and the next commands, leaving the last
/// row for the prompt; below 60×12, the summary alone.
pub fn frame(
    view: &(impl TreeView + ?Sized),
    opts: &PaintOpts<'_>,
    footer: &Footer,
    w: u16,
    rows: u16,
) -> CellBuffer {
    let pack = opts.pack;
    if w < 60 || rows < 12 {
        let mut buffer = CellBuffer::new(w, 1);
        buffer.text(0, 0, &footer.summary, w, pack.roles.text, false);
        return buffer;
    }
    let map_rows = rows - 5;
    let map = paint(view, opts, w, map_rows);
    let mut buffer = CellBuffer::new(w, map_rows + 4);
    for y in 0..map_rows {
        for x in 0..w {
            if let (Some(cell), Some(slot)) = (map.get(x, y), buffer.get_mut(x, y)) {
                *slot = *cell;
            }
        }
    }
    // The legend: `■ code 61%` per class, the swatch in its colour.
    let mut x = 2u16;
    let y = map_rows;
    for &(class, pct) in &footer.legend {
        let swatch = match (pack.shades, opts.glyphs) {
            (true, _) => class_key(class),
            (false, Glyphs::Unicode) => '■',
            (false, Glyphs::Ascii) => '#',
        };
        let text = format!(" {} {pct:.0}%  ", class.as_str());
        let needed = u16::try_from(text.chars().count() + 1).unwrap_or(u16::MAX);
        if x + needed > w {
            break;
        }
        buffer.text(x, y, &swatch.to_string(), 1, pack.class(class), false);
        buffer.text(x + 1, y, &text, w - x - 1, pack.roles.subtext, false);
        x += needed;
    }
    buffer.text(0, y + 1, &footer.summary, w, pack.roles.text, false);
    if let Some(warning) = &footer.warning {
        buffer.text(0, y + 2, warning, w, pack.roles.red, false);
    }
    buffer.text(0, y + 3, &footer.next, w, pack.roles.subtext, false);
    buffer
}

#[cfg(test)]
mod tests {
    use super::{Glyphs, compact, truncate};

    #[test]
    fn p1_map_02_sizes_print_compactly() {
        assert_eq!(compact(0), "0B");
        assert_eq!(compact(512), "512B");
        assert_eq!(compact(1_900_000_000), "1.8G");
        assert_eq!(compact(977_272_832), "932M");
        assert_eq!(compact(12 << 20), "12M");
        assert_eq!(compact(10_239), "10K");
        assert_eq!(truncate("node_modules", 6, Glyphs::Unicode), "node_…");
        assert_eq!(truncate("node_modules", 6, Glyphs::Ascii), "node_~");
        assert_eq!(truncate("src", 6, Glyphs::Unicode), "src");
    }
}
