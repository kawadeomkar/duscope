//! `render`'s picture of the treemap, at any size in pixels: the layout
//! walked once and drawn on a [`Canvas`], the SVG writer's
//! ([`crate::svg`]) or the PNG rasteriser's ([`crate::png`]).
//!
//! - **Layout:** the static map's tiles, in pixels: a directory's children
//!   squarified ([`Algorithm::Squarify`]), or with [`Algorithm::Gp`] its
//!   files first cut from its subfolders (the files on the right or at the
//!   bottom) and each half laid out by the balanced tree, as
//!   `GrandPerspective` lays out a folder; a half that weighs nothing is
//!   laid out with the rest. Directories open down to `--depth`, while
//!   their tile is at least 2 px each way and something is under them; as
//!   in `GrandPerspective`, directories are not outlined and only leaves
//!   are painted. What lays out below a pixel is pooled into `+N small`.
//! - **Shading** (`GradientRectangleDrawer.m`): each colour's 256-step
//!   table ([`shade`]), lightest at a tile's top-left corner and darkest
//!   along its bottom and right edges, with L-shaped contours; tiles under
//!   3 px each way are flat.
//! - **Labels:** a cell is 0.6 em wide and 1.3 em tall at a font size of
//!   the picture's smaller side over 50 (10 to 16 px); a leaf at least 5
//!   cells wide and 1 tall shows its name, one at least 8 by 2 its size
//!   below, cut with `…`, in the static map's label colour for the fill
//!   behind it, clipped to its tile. Control characters show as escapes.
//!   The labels are drawn once every tile is.
//! - **Under the map**, where the picture is at least 40 by 12 cells: the
//!   legend (each class's share) and the totals.

use std::io;

use duscope_core::query::{Node, TreeView, children};
use duscope_core::rules::Class;

use crate::cells::Color;
use crate::gp::cut;
use crate::paint::{
    Glyphs, PaintOpts, classes_of, compact, dominant, readable, rgb, tile_color, truncate,
};
use crate::width::glyphs;
use crate::{Algorithm, Rect, layout};

/// The largest picture, in pixels each way.
pub const MAX_SIDE: u16 = 16_384;

/// A colour's red, green and blue.
pub(crate) type Rgb = (u8, u8, u8);

/// How the picture is drawn.
#[derive(Debug, Clone)]
pub struct PictureOpts<'a> {
    /// The map's colours, `--color-by`, `--depth`, `--pool-files`, the
    /// size the tiles weigh and the classes table.
    pub paint: PaintOpts<'a>,
    /// `--layout`.
    pub algorithm: Algorithm,
    /// The picture's width, in pixels (at most [`MAX_SIDE`]).
    pub width: u16,
    /// Its height.
    pub height: u16,
    /// The picture's title: the tree's path.
    pub title: String,
    /// Each class's share of the scan, largest first, for the legend.
    pub legend: Vec<(Class, f64)>,
    /// The totals line.
    pub summary: String,
}

/// What a picture is drawn on.
pub(crate) trait Canvas {
    /// A leaf filling `rect`, shaded in `rgb` (flat under 3 px each way).
    fn tile(&mut self, rect: Rect, rgb: Rgb) -> io::Result<()>;
    /// `lines`, the first at `baseline` from `left` and each next one
    /// `step` lower, in `ink`, clipped to `clip`.
    fn label(
        &mut self,
        clip: Rect,
        left: u32,
        baseline: u32,
        step: u32,
        lines: &[String],
        ink: Rgb,
    ) -> io::Result<()>;
    /// A square of `side` pixels at `x`, `y`, in `rgb`.
    fn swatch(&mut self, x: u32, y: u32, side: u32, rgb: Rgb) -> io::Result<()>;
    /// `text` from `left` at `baseline`, in `ink`.
    fn text(&mut self, left: u32, baseline: u32, text: &str, ink: Rgb) -> io::Result<()>;
}

/// The picture's sizes, in pixels.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Metrics {
    pub width: u16,
    pub height: u16,
    /// The font size.
    pub font: u32,
    /// A cell: 0.6 em wide, 1.3 em tall.
    pub cell_w: u32,
    pub cell_h: u32,
    /// The map's height: the picture's, less the band under it.
    pub map_h: u16,
    /// Whether the legend and the totals fit under the map.
    pub footer: bool,
}

impl Metrics {
    pub(crate) fn of(opts: &PictureOpts<'_>) -> Metrics {
        let width = opts.width.clamp(1, MAX_SIDE);
        let height = opts.height.clamp(1, MAX_SIDE);
        let font = u32::from(width.min(height) / 50).clamp(10, 16);
        let cell_w = (font * 3).div_ceil(5);
        let cell_h = (font * 13).div_ceil(10);
        let footer = u32::from(width) >= 40 * cell_w && u32::from(height) >= 12 * cell_h;
        let band = if footer { cell_h * 5 / 2 } else { 0 };
        Metrics {
            width,
            height,
            font,
            cell_w,
            cell_h,
            map_h: u16::try_from(u32::from(height) - band).unwrap_or(height),
            footer,
        }
    }
}

/// A label, drawn once the tiles are.
struct Label {
    clip: Rect,
    left: u32,
    baseline: u32,
    lines: Vec<String>,
    ink: Rgb,
}

/// Draws the picture of the scan in `view` on `canvas`: the tiles, their
/// labels, then the legend and the totals; the number of tiles.
pub(crate) fn draw(
    view: &dyn TreeView,
    opts: &PictureOpts<'_>,
    metrics: Metrics,
    canvas: &mut dyn Canvas,
) -> io::Result<u64> {
    let mut walk = Walk {
        view,
        opts,
        metrics,
        canvas,
        class_of: classes_of(view, opts.paint.classes),
        labels: Vec::new(),
        tiles: 0,
    };
    if !view.is_empty() {
        let root = view.node(0);
        let area = Rect::new(0, 0, metrics.width, metrics.map_h);
        if root.is_dir() {
            walk.block(&root, area, 0, None)?;
        } else {
            let class = walk.class_of[0];
            let color = tile_color(view, &opts.paint, &root, class, 0, None);
            walk.leaf(
                area,
                color,
                &name_of(view, &root),
                Some(root.size(opts.paint.size)),
            )?;
        }
    }
    let Walk {
        canvas,
        labels,
        tiles,
        ..
    } = walk;
    for label in &labels {
        canvas.label(
            label.clip,
            label.left,
            label.baseline,
            metrics.cell_h,
            &label.lines,
            label.ink,
        )?;
    }
    if metrics.footer {
        under(canvas, opts, metrics)?;
    }
    Ok(tiles)
}

/// The walk's state.
struct Walk<'v, 'o, 'c> {
    view: &'v dyn TreeView,
    opts: &'o PictureOpts<'o>,
    metrics: Metrics,
    canvas: &'c mut dyn Canvas,
    /// Each entry's class, by pre-order id.
    class_of: Vec<Class>,
    labels: Vec<Label>,
    tiles: u64,
}

/// A tile to place: an entry, or a directory's files under `--pool-files`.
#[derive(Debug, Clone, Copy)]
enum Item {
    Entry(u32),
    /// The directory's id, its file count.
    Files(u32, u64),
}

impl Walk<'_, '_, '_> {
    /// Lays out `dir`'s children in `area` and paints each; `depth` is
    /// `dir`'s, `folder` the rank of the top-level folder above.
    fn block(
        &mut self,
        dir: &Node,
        area: Rect,
        depth: u32,
        folder: Option<usize>,
    ) -> io::Result<()> {
        let size = self.opts.paint.size;
        let mut files: Vec<(Item, u64)> = Vec::new();
        let mut dirs: Vec<(Item, u64)> = Vec::new();
        let mut pooled = (0u64, 0u64);
        for id in children(self.view, dir) {
            let node = self.view.node(id);
            let weight = node.counted(size);
            if node.is_dir() {
                dirs.push((Item::Entry(id), weight));
            } else if self.opts.paint.pool_files {
                pooled.0 += weight;
                pooled.1 += 1;
            } else {
                files.push((Item::Entry(id), weight));
            }
        }
        if pooled.1 > 0 {
            files.push((Item::Files(dir.id, pooled.1), pooled.0));
        }
        // A top-level folder's rank among the root's children, largest
        // first, as the static map numbers them.
        let ranks: Vec<usize> = if depth == 0 {
            let mut all: Vec<(u64, usize)> = files
                .iter()
                .chain(&dirs)
                .enumerate()
                .map(|(i, &(_, w))| (w, i))
                .collect();
            all.sort_by(|a, b| b.0.cmp(&a.0));
            let mut ranks = vec![0; all.len()];
            for (rank, &(_, i)) in all.iter().enumerate() {
                ranks[i] = rank;
            }
            ranks
        } else {
            Vec::new()
        };
        let rank = |i: usize| folder.or_else(|| ranks.get(i).copied());
        let files_n = files.len();
        let file_weight: u64 = files.iter().map(|&(_, w)| w).sum();
        let dir_weight: u64 = dirs.iter().map(|&(_, w)| w).sum();
        // A half that weighs nothing is not cut off: laid out with the
        // rest, its entries go to the pool.
        if self.opts.algorithm == Algorithm::Gp && file_weight > 0 && dir_weight > 0 {
            let whole = u128::from(file_weight) + u128::from(dir_weight);
            let (files_area, dirs_area) = cut(area, u128::from(file_weight), whole);
            self.lay(&files, files_area, &|i| rank(i))?;
            self.lay(&dirs, dirs_area, &|i| rank(files_n + i))?;
        } else {
            let all: Vec<(Item, u64)> = files.into_iter().chain(dirs).collect();
            self.lay(&all, area, &rank)?;
        }
        Ok(())
    }

    /// Lays `items` out in `area` with the algorithm asked for and paints
    /// each tile, then the pool; `folder` gives an item's top-level folder.
    fn lay(
        &mut self,
        items: &[(Item, u64)],
        area: Rect,
        folder: &dyn Fn(usize) -> Option<usize>,
    ) -> io::Result<()> {
        if items.is_empty() || area.w == 0 || area.h == 0 {
            return Ok(());
        }
        let weights: Vec<(u64, usize)> = items
            .iter()
            .enumerate()
            .map(|(i, &(_, w))| (w, i))
            .collect();
        let laid = layout(&weights, area, self.opts.algorithm, 1);
        let paint = &self.opts.paint;
        for tile in &laid.tiles {
            match items[tile.id].0 {
                Item::Entry(id) => {
                    let node = self.view.node(id);
                    let folder = folder(tile.id);
                    // A directory with nothing under it (empty, or unread)
                    // is a leaf, whatever it weighs.
                    let open = paint.depth.is_none_or(|cap| node.depth < cap.max(1))
                        && node.last_id > node.id;
                    if node.is_dir() && open && tile.rect.w >= 2 && tile.rect.h >= 2 {
                        self.block(&node, tile.rect, node.depth, folder)?;
                    } else {
                        let class = if node.is_dir() {
                            dominant(self.view, &self.class_of, paint.size, id, false)
                        } else {
                            self.class_of[id as usize]
                        };
                        let color = tile_color(self.view, paint, &node, class, node.depth, folder);
                        let name = name_of(self.view, &node);
                        self.leaf(tile.rect, color, &name, Some(node.size(paint.size)))?;
                    }
                }
                Item::Files(dir, count) => {
                    let class = dominant(self.view, &self.class_of, paint.size, dir, true);
                    let color = paint.pack.class(class);
                    self.leaf(tile.rect, color, &format!("{count} files"), None)?;
                }
            }
        }
        if let Some(small) = &laid.small {
            let color = paint.pack.class(Class::Other);
            let label = format!("+{} small", small.ids.len());
            self.leaf(small.rect, color, &label, None)?;
        }
        Ok(())
    }

    /// Paints one leaf in `rect`, shaded in `color`, and queues its label.
    fn leaf(&mut self, rect: Rect, color: Color, name: &str, size: Option<u64>) -> io::Result<()> {
        if rect.w == 0 || rect.h == 0 {
            return Ok(());
        }
        self.tiles += 1;
        let rgb = rgb(color).unwrap_or((128, 128, 128));
        self.canvas.tile(rect, rgb)?;
        self.label(rect, rgb, name, size);
        Ok(())
    }

    /// Queues `rect`'s label: the name at 5 cells by 1 and more, the size
    /// below it at 8 by 2.
    fn label(&mut self, rect: Rect, rgb: Rgb, name: &str, size: Option<u64>) {
        let Metrics {
            font,
            cell_w,
            cell_h,
            ..
        } = self.metrics;
        let (width, height) = (u32::from(rect.w), u32::from(rect.h));
        if width < 5 * cell_w || height < cell_h {
            return;
        }
        // The label sits where the gradient is light, near the top left.
        let light = shade(rgb, 200);
        let ink = rgb_of(readable(
            Color::Rgb(light.0, light.1, light.2),
            self.opts.paint.pack,
        ));
        let room = usize::try_from(width / cell_w)
            .unwrap_or(0)
            .saturating_sub(3);
        let mut lines = vec![truncate(&shown(name), room, Glyphs::Unicode)];
        if let Some(size) = size
            && width >= 8 * cell_w
            && height >= 2 * cell_h
        {
            lines.push(compact(size));
        }
        self.labels.push(Label {
            clip: rect,
            left: u32::from(rect.x) + cell_w,
            baseline: u32::from(rect.y) + cell_h.midpoint(font),
            lines,
            ink,
        });
    }
}

/// An entry's label: its name, a directory's with a `/`.
fn name_of(view: &dyn TreeView, node: &Node) -> String {
    let mut name = String::from_utf8_lossy(&view.name(node.id)).into_owned();
    if node.is_dir() && !name.ends_with('/') {
        name.push('/');
    }
    name
}

/// The legend and the totals, in the band under the map.
fn under(canvas: &mut dyn Canvas, opts: &PictureOpts<'_>, metrics: Metrics) -> io::Result<()> {
    let Metrics {
        font,
        cell_w,
        cell_h,
        map_h,
        ..
    } = metrics;
    let pack = opts.paint.pack;
    let text = rgb_of(pack.roles.text);
    let width = u32::from(metrics.width);
    let top = u32::from(map_h) + cell_h / 4;
    let baseline = top + cell_h.midpoint(font);
    let mut x = cell_w;
    for &(class, pct) in &opts.legend {
        let label = format!("{} {pct:.0}%", class.as_str());
        let needed = (u32::try_from(label.chars().count()).unwrap_or(0) + 4) * cell_w;
        if x + needed > width {
            break;
        }
        let side = font * 3 / 4;
        canvas.swatch(x, baseline - side, side, rgb_of(pack.class(class)))?;
        canvas.text(x + side + cell_w / 2, baseline, &label, text)?;
        x += needed;
    }
    let room = usize::try_from(width / cell_w)
        .unwrap_or(0)
        .saturating_sub(2);
    canvas.text(
        cell_w,
        baseline + cell_h,
        &truncate(&shown(&opts.summary), room, Glyphs::Unicode),
        text,
    )
}

/// A colour's red, green and blue; grey for the terminal's own colour,
/// which a picture has not.
pub(crate) fn rgb_of(color: Color) -> Rgb {
    rgb(color).unwrap_or((128, 128, 128))
}

/// Step `index` (0 darkest to 255 lightest, 128 the colour itself) of
/// `GrandPerspective`'s gradient table for `rgb`, with its default gradient
/// of 0.5: darker below 128 by up to half the brightness; lighter above,
/// the brightness raised first and then the saturation lowered.
pub(crate) fn shade(rgb: Rgb, index: u32) -> Rgb {
    const GRADIENT: f64 = 0.5;
    let (hue, saturation, brightness) = hsb(rgb);
    let index = f64::from(index.min(255));
    let (saturation, brightness) = if index < 128.0 {
        (
            saturation,
            brightness * (1.0 - GRADIENT * (128.0 - index) / 128.0),
        )
    } else {
        let lifted = brightness + GRADIENT * (index - 128.0) / 128.0;
        if lifted > 1.0 {
            ((saturation - (lifted - 1.0)).max(0.0), 1.0)
        } else {
            (saturation, lifted)
        }
    };
    from_hsb(hue, saturation, brightness)
}

/// Hue (0 to 6), saturation and brightness (0 to 1).
fn hsb((red, green, blue): Rgb) -> (f64, f64, f64) {
    // The channels compared as bytes, exactly; the arithmetic in floats.
    let (top, bottom) = (red.max(green).max(blue), red.min(green).min(blue));
    let unit = |channel: u8| f64::from(channel) / 255.0;
    let (max, delta) = (unit(top), unit(top) - unit(bottom));
    let hue = if top == bottom {
        0.0
    } else if top == red {
        ((unit(green) - unit(blue)) / delta).rem_euclid(6.0)
    } else if top == green {
        (unit(blue) - unit(red)) / delta + 2.0
    } else {
        (unit(red) - unit(green)) / delta + 4.0
    };
    let saturation = if top == 0 { 0.0 } else { delta / max };
    (hue, saturation, max)
}

fn from_hsb(hue: f64, saturation: f64, brightness: f64) -> Rgb {
    let chroma = brightness * saturation;
    let middle = chroma * (1.0 - (hue.rem_euclid(2.0) - 1.0).abs());
    let (red, green, blue) = match hue {
        sector if sector < 1.0 => (chroma, middle, 0.0),
        sector if sector < 2.0 => (middle, chroma, 0.0),
        sector if sector < 3.0 => (0.0, chroma, middle),
        sector if sector < 4.0 => (0.0, middle, chroma),
        sector if sector < 5.0 => (middle, 0.0, chroma),
        _ => (chroma, 0.0, middle),
    };
    let floor = brightness - chroma;
    let byte = |value: f64| {
        let value = ((value + floor) * 255.0).round().clamp(0.0, 255.0);
        // In range after the clamp.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let value = value as u8;
        value
    };
    (byte(red), byte(green), byte(blue))
}

/// `text` as the static map shows it: control characters, separators and
/// bidirectional controls as escapes (`\x1b`).
pub(crate) fn shown(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for glyph in glyphs(text, false) {
        out.push(glyph.ch);
        out.extend(glyph.marks.iter().filter(|&&m| m != '\0'));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{shade, shown};

    #[test]
    fn p2_map_01_the_gradient_darkens_below_128_and_lightens_above() {
        let coffee = (0x99, 0x33, 0x00);
        assert_eq!(shade(coffee, 128), coffee);
        // Half the brightness at the darkest step.
        let dark = shade(coffee, 0);
        assert_eq!(dark, (0x4d, 0x1a, 0x00));
        // Brighter first, then paler, at the lightest.
        let light = shade(coffee, 255);
        assert!(
            light.0 == 0xff && light.1 > coffee.1 && light.2 > coffee.2,
            "{light:?}"
        );
        // Grey stays grey.
        let grey = shade((100, 100, 100), 0);
        assert!(grey.0 == grey.1 && grey.1 == grey.2);
    }

    #[test]
    fn p2_map_01_names_show_control_characters_as_escapes() {
        assert_eq!(shown("a\u{1b}b"), "a\\x1bb");
    }
}
