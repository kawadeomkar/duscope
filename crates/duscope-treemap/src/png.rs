//! `render`'s PNG: the picture ([`crate::picture`]) rasterised in-tree,
//! with no graphics library.
//!
//! - **Tiles:** each pixel of a leaf takes its step of the colour's table
//!   by `GrandPerspective`'s own formula (`GradientRectangleDrawer.m`):
//!   `256 × min((y from the bottom + 0.5) / h, 1 − (x + 0.5) / w)`, so the
//!   top-left corner is lightest and the bottom and right edges darkest,
//!   pixel for pixel as `GrandPerspective` fills a rectangle. Tiles under
//!   3 px each way are flat, as in the SVG.
//! - **Labels** are drawn in the bundled Inter ([`crate::font`]): each
//!   glyph's quadratic outline flattened to lines and filled with
//!   anti-aliasing by signed-area accumulation (as `font-rs` does),
//!   blended over the tile and clipped to it; no hinting, no kerning.
//! - **The file:** 8-bit RGB, each row `Sub`-filtered, compressed with
//!   zlib and checksummed with CRC-32, both through `flate2`.

use std::collections::HashMap;
use std::io::{self, Write};

use duscope_core::query::TreeView;
use flate2::write::ZlibEncoder;
use flate2::{Compression, Crc};

use crate::Rect;
use crate::font::{Font, Point};
use crate::picture::{Canvas, Metrics, PictureOpts, Rgb, draw, rgb_of, shade};

/// A point in pixels, x right and y down.
type Spot = (f32, f32);

/// A line from one spot to another.
type Segment = (Spot, Spot);

/// The most pixels a PNG may have: 64 Mi, 192 MiB of RGB.
pub const MAX_PIXELS: u64 = 1 << 26;

/// Writes the picture of the scan in `view` as PNG; the number of tiles
/// painted.
///
/// # Errors
///
/// When `out` cannot be written, or the picture has more than
/// [`MAX_PIXELS`] pixels (`InvalidInput`).
pub fn write_png(
    view: &dyn TreeView,
    opts: &PictureOpts<'_>,
    out: &mut dyn Write,
) -> io::Result<u64> {
    let metrics = Metrics::of(opts);
    let (width, height) = (usize::from(metrics.width), usize::from(metrics.height));
    if (width * height) as u64 > MAX_PIXELS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("a PNG holds at most {MAX_PIXELS} pixels"),
        ));
    }
    let base = rgb_of(opts.paint.pack.roles.base);
    let mut canvas = Png {
        width,
        height,
        pixels: [base.0, base.1, base.2].repeat(width * height),
        tables: HashMap::new(),
        font: Font::inter(),
        font_px: f32::from(u16::try_from(metrics.font).unwrap_or(12)),
    };
    let tiles = draw(view, opts, metrics, &mut canvas)?;
    canvas.encode(out)?;
    Ok(tiles)
}

/// The pixels and what draws on them.
struct Png {
    width: usize,
    height: usize,
    /// RGB, row by row.
    pixels: Vec<u8>,
    /// Each colour's 256 steps.
    tables: HashMap<Rgb, Vec<Rgb>>,
    font: Option<Font>,
    /// The labels' size.
    font_px: f32,
}

impl Png {
    fn put(&mut self, x: usize, y: usize, rgb: Rgb) {
        if x < self.width && y < self.height {
            let at = 3 * (y * self.width + x);
            if let Some(pixel) = self.pixels.get_mut(at..at + 3) {
                pixel.copy_from_slice(&[rgb.0, rgb.1, rgb.2]);
            }
        }
    }

    /// Mixes `ink` over the pixel at `x`, `y` by `alpha` (0 to 1).
    fn blend(&mut self, x: usize, y: usize, ink: Rgb, alpha: f32) {
        if x >= self.width || y >= self.height {
            return;
        }
        let at = 3 * (y * self.width + x);
        let Some(pixel) = self.pixels.get_mut(at..at + 3) else {
            return;
        };
        for (channel, ink) in pixel.iter_mut().zip([ink.0, ink.1, ink.2]) {
            let mixed = f32::from(*channel) + (f32::from(ink) - f32::from(*channel)) * alpha;
            // Between the two bytes it mixes.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let mixed = mixed.round().clamp(0.0, 255.0) as u8;
            *channel = mixed;
        }
    }

    /// Draws `text` from `left` at `baseline`, in `ink`, inside `clip`.
    fn draw_text(&mut self, left: u32, baseline: u32, text: &str, ink: Rgb, clip: Rect) {
        let Some(font) = self.font.clone() else {
            return;
        };
        let scale = self.font_px / f32::from(font.units_per_em());
        let mut pen = to_f32(left);
        let base = to_f32(baseline);
        for c in text.chars() {
            let glyph = font.glyph(c);
            let contours = font.contours(glyph);
            if !contours.is_empty() {
                self.fill_glyph(&contours, scale, pen, base, ink, clip);
            }
            pen += f32::from(font.advance(glyph)) * scale;
            if pen > f32::from(clip.x) + f32::from(clip.w) {
                break;
            }
        }
    }

    /// Fills a glyph's `contours` (font units, y up), scaled by `scale`,
    /// its origin at `ox`, `oy`, in `ink`, clipped to `clip`.
    fn fill_glyph(
        &mut self,
        contours: &[Vec<Point>],
        scale: f32,
        ox: f32,
        oy: f32,
        ink: Rgb,
        clip: Rect,
    ) {
        let mut lines = Vec::new();
        for contour in contours {
            flatten(contour, &mut |a: (f32, f32), b: (f32, f32)| {
                lines.push((
                    (ox + a.0 * scale, oy - a.1 * scale),
                    (ox + b.0 * scale, oy - b.1 * scale),
                ));
            });
        }
        let Some((min_x, min_y, max_x, max_y)) = bounds(&lines) else {
            return;
        };
        // The glyph's own cells: a column to spare on the right, where the
        // accumulation spills.
        // The glyph's cells start at its box's whole-pixel corner, which may
        // lie left of or above the picture; a column to spare on the right,
        // where the accumulation spills.
        let (gx, gy) = (min_x.floor(), min_y.floor());
        let cols = to_usize(f64::from(max_x.ceil() - gx)) + 2;
        let rows = to_usize(f64::from(max_y.ceil() - gy)) + 1;
        let mut acc = vec![0.0f32; cols * rows];
        for &(a, b) in &lines {
            accumulate(
                &mut acc,
                cols,
                rows,
                (a.0 - gx, a.1 - gy),
                (b.0 - gx, b.1 - gy),
            );
        }
        let (left, top) = (whole(gx), whole(gy));
        let (clip_left, clip_top) = (usize::from(clip.x), usize::from(clip.y));
        let (clip_right, clip_bottom) = (
            clip_left + usize::from(clip.w),
            clip_top + usize::from(clip.h),
        );
        for (row, cells) in acc.chunks_exact(cols).enumerate() {
            let mut cover = 0.0f32;
            for (col, &cell) in cells.iter().enumerate() {
                cover += cell;
                let alpha = cover.abs().min(1.0);
                let (Some(x), Some(y)) = (offset(left, col), offset(top, row)) else {
                    continue;
                };
                if alpha > 0.0
                    && (clip_left..clip_right).contains(&x)
                    && (clip_top..clip_bottom).contains(&y)
                {
                    self.blend(x, y, ink, alpha);
                }
            }
        }
    }

    /// The file: signature, header, the compressed rows, end.
    fn encode(&self, out: &mut dyn Write) -> io::Result<()> {
        let mut header = Vec::with_capacity(13);
        header.extend_from_slice(&u32::try_from(self.width).unwrap_or(0).to_be_bytes());
        header.extend_from_slice(&u32::try_from(self.height).unwrap_or(0).to_be_bytes());
        // 8-bit RGB, deflate, adaptive filtering, no interlace.
        header.extend_from_slice(&[8, 2, 0, 0, 0]);
        let mut z = ZlibEncoder::new(Vec::new(), Compression::default());
        let stride = 3 * self.width;
        let mut filtered = vec![0u8; stride + 1];
        filtered[0] = 1;
        for row in self.pixels.chunks_exact(stride) {
            // Sub: each byte less the one a pixel to its left.
            for (i, &byte) in row.iter().enumerate() {
                let left = if i >= 3 { row[i - 3] } else { 0 };
                filtered[i + 1] = byte.wrapping_sub(left);
            }
            z.write_all(&filtered)?;
        }
        let data = z.finish()?;
        out.write_all(b"\x89PNG\r\n\x1a\n")?;
        chunk(out, *b"IHDR", &header)?;
        chunk(out, *b"IDAT", &data)?;
        chunk(out, *b"IEND", &[])
    }
}

impl Canvas for Png {
    fn tile(&mut self, rect: Rect, rgb: Rgb) -> io::Result<()> {
        let (x0, y0) = (usize::from(rect.x), usize::from(rect.y));
        let (w, h) = (usize::from(rect.w), usize::from(rect.h));
        if w < 3 || h < 3 {
            for y in y0..y0 + h {
                for x in x0..x0 + w {
                    self.put(x, y, rgb);
                }
            }
            return Ok(());
        }
        let table = self
            .tables
            .entry(rgb)
            .or_insert_with(|| (0..256).map(|i| shade(rgb, i)).collect())
            .clone();
        for dy in 0..h {
            // 256 × (y from the bottom + 0.5) / h, in whole steps.
            let rise = 256 * (2 * (h - 1 - dy) + 1) / (2 * h);
            for dx in 0..w {
                // 256 × (1 − (x + 0.5) / w).
                let fall = 256 * (2 * (w - dx) - 1) / (2 * w);
                let step = rise.min(fall).min(255);
                self.put(x0 + dx, y0 + dy, table[step]);
            }
        }
        Ok(())
    }

    fn label(
        &mut self,
        clip: Rect,
        left: u32,
        baseline: u32,
        step: u32,
        lines: &[String],
        ink: Rgb,
    ) -> io::Result<()> {
        let mut baseline = baseline;
        for line in lines {
            self.draw_text(left, baseline, line, ink, clip);
            baseline += step;
        }
        Ok(())
    }

    fn swatch(&mut self, x: u32, y: u32, side: u32, rgb: Rgb) -> io::Result<()> {
        let (x, y, side) = (
            to_usize(f64::from(x)),
            to_usize(f64::from(y)),
            to_usize(f64::from(side)),
        );
        for py in y..y + side {
            for px in x..x + side {
                self.put(px, py, rgb);
            }
        }
        Ok(())
    }

    fn text(&mut self, left: u32, baseline: u32, text: &str, ink: Rgb) -> io::Result<()> {
        let all = Rect::new(
            0,
            0,
            u16::try_from(self.width).unwrap_or(u16::MAX),
            u16::try_from(self.height).unwrap_or(u16::MAX),
        );
        self.draw_text(left, baseline, text, ink, all);
        Ok(())
    }
}

/// Calls `line` for each segment of a closed quadratic contour, its curves
/// cut into short lines.
fn flatten(contour: &[Point], line: &mut dyn FnMut(Spot, Spot)) {
    let n = contour.len();
    if n < 2 {
        return;
    }
    let at = |i: usize| {
        let p = contour[i % n];
        (f32::from(p.x), f32::from(p.y), p.on)
    };
    let mid = |a: Spot, b: Spot| (a.0.midpoint(b.0), a.1.midpoint(b.1));
    // Start on the curve: the first on-curve point, else the midpoint of
    // the first two (both control points).
    let first_on = (0..n).find(|&i| contour[i].on);
    let (start, offset) = match first_on {
        Some(i) => ((at(i).0, at(i).1), i),
        None => (mid((at(0).0, at(0).1), (at(1).0, at(1).1)), 1),
    };
    let mut current = start;
    let mut control: Option<(f32, f32)> = None;
    for k in 1..=n {
        let (x, y, on) = at(offset + k);
        let point = (x, y);
        match (on, control) {
            (true, None) => {
                line(current, point);
                current = point;
            }
            (true, Some(c)) => {
                quad(current, c, point, line);
                current = point;
                control = None;
            }
            (false, None) => control = Some(point),
            (false, Some(c)) => {
                let between = mid(c, point);
                quad(current, c, between, line);
                current = between;
                control = Some(point);
            }
        }
    }
    if let Some(c) = control {
        quad(current, c, start, line);
    } else if current != start {
        line(current, start);
    }
}

/// A quadratic curve from `from` through the control point `via` to `to`,
/// cut into lines, more for a longer one.
fn quad(from: Spot, via: Spot, to: Spot, line: &mut dyn FnMut(Spot, Spot)) {
    // Its length in font units; a glyph is about 2,000 units an em.
    let length = (via.0 - from.0).hypot(via.1 - from.1) + (to.0 - via.0).hypot(to.1 - via.1);
    let steps = (length / 64.0).ceil().clamp(2.0, 16.0);
    let mut previous = from;
    let mut step = 0.0;
    while step < steps {
        step += 1.0;
        let along = step / steps;
        let rest = 1.0 - along;
        let point = (
            rest * rest * from.0 + 2.0 * rest * along * via.0 + along * along * to.0,
            rest * rest * from.1 + 2.0 * rest * along * via.1 + along * along * to.1,
        );
        line(previous, point);
        previous = point;
    }
}

/// The lines' bounding box.
fn bounds(lines: &[Segment]) -> Option<(f32, f32, f32, f32)> {
    let mut points = lines.iter().flat_map(|&(a, b)| [a, b]);
    let first = points.next()?;
    Some(points.fold(
        (first.0, first.1, first.0, first.1),
        |(x0, y0, x1, y1), (x, y)| (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
    ))
}

/// Adds the line from `p0` to `p1` to the coverage accumulation `acc`
/// (`cols` by `rows`): each cell gets the signed area the line sweeps in
/// it, so that a row's running sum is each pixel's coverage.
fn accumulate(acc: &mut [f32], cols: usize, rows: usize, p0: Spot, p1: Spot) {
    if (p0.1 - p1.1).abs() <= f32::EPSILON {
        return;
    }
    let (dir, p0, p1) = if p0.1 < p1.1 {
        (1.0, p0, p1)
    } else {
        (-1.0, p1, p0)
    };
    let dxdy = (p1.0 - p0.0) / (p1.1 - p0.1);
    let mut edge = p0.0;
    let mut add = |i: usize, value: f32| {
        if let Some(cell) = acc.get_mut(i) {
            *cell += value;
        }
    };
    let first = to_usize(f64::from(p0.1.max(0.0)));
    let last = to_usize(f64::from(p1.1.ceil())).min(rows);
    if p0.1 < 0.0 {
        edge -= p0.1 * dxdy;
    }
    for y in first..last {
        let row = y * cols;
        let top = to_f32(u32::try_from(y).unwrap_or(u32::MAX));
        let dy = (top + 1.0).min(p1.1) - top.max(p0.1);
        let next = edge + dxdy * dy;
        let signed = dy * dir;
        let (x0, x1) = if edge < next {
            (edge, next)
        } else {
            (next, edge)
        };
        let (x0, x1) = (x0.max(0.0), x1.max(0.0));
        let x0_floor = x0.floor();
        let x0i = to_usize(f64::from(x0_floor));
        let x1_ceil = x1.ceil();
        let x1i = to_usize(f64::from(x1_ceil));
        if x1i <= x0i + 1 {
            let middle = 0.5 * (edge + next) - x0_floor;
            add(row + x0i, signed - signed * middle);
            add(row + x0i + 1, signed * middle);
        } else {
            let inverse = (x1 - x0).recip();
            let frac_start = x0 - x0_floor;
            let a0 = 0.5 * inverse * (1.0 - frac_start) * (1.0 - frac_start);
            let frac_end = x1 - x1_ceil + 1.0;
            let am = 0.5 * inverse * frac_end * frac_end;
            add(row + x0i, signed * a0);
            if x1i == x0i + 2 {
                add(row + x0i + 1, signed * (1.0 - a0 - am));
            } else {
                let a1 = inverse * (1.5 - frac_start);
                add(row + x0i + 1, signed * (a1 - a0));
                for xi in x0i + 2..x1i - 1 {
                    add(row + xi, signed * inverse);
                }
                let a2 = a1 + to_f32(u32::try_from(x1i - x0i - 3).unwrap_or(0)) * inverse;
                add(row + x1i - 1, signed * (1.0 - a2 - am));
            }
            add(row + x1i, signed * am);
        }
        edge = next;
    }
}

/// A chunk: its length, type, data and the CRC of type and data.
fn chunk(out: &mut dyn Write, kind: [u8; 4], data: &[u8]) -> io::Result<()> {
    let length = u32::try_from(data.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "a PNG chunk over 4 GiB"))?;
    let mut crc = Crc::new();
    crc.update(&kind);
    crc.update(data);
    out.write_all(&length.to_be_bytes())?;
    out.write_all(&kind)?;
    out.write_all(data)?;
    out.write_all(&crc.sum().to_be_bytes())
}

/// A coordinate that is not negative, as an index.
fn to_usize(value: f64) -> usize {
    // Not negative and at most a picture's side, by the callers.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let index = value.max(0.0) as usize;
    index
}

/// `value`, a whole number of pixels, as an integer that may be negative.
fn whole(value: f32) -> i64 {
    // A glyph's corner: within a few em of the picture.
    #[allow(clippy::cast_possible_truncation)]
    let value = value as i64;
    value
}

/// The pixel `by` from `start`, where it is on the picture's side of 0.
fn offset(start: i64, by: usize) -> Option<usize> {
    usize::try_from(start.checked_add(i64::try_from(by).ok()?)?).ok()
}

/// A pixel position as a float.
fn to_f32(value: u32) -> f32 {
    // Positions stay below 2^24, where f32 is exact.
    #[allow(clippy::cast_precision_loss)]
    let value = value as f32;
    value
}

#[cfg(test)]
mod tests {
    use super::{Png, accumulate, flatten};
    use crate::Rect;
    use crate::font::Font;
    use std::collections::HashMap;

    fn canvas(width: usize, height: usize) -> Png {
        Png {
            width,
            height,
            pixels: vec![0; 3 * width * height],
            tables: HashMap::new(),
            font: Font::inter(),
            font_px: 16.0,
        }
    }

    #[test]
    fn p2_map_02_a_tile_is_lightest_top_left_and_darkest_bottom_right() {
        use crate::picture::Canvas;
        let mut png = canvas(40, 30);
        png.tile(Rect::new(0, 0, 40, 30), (0x99, 0x33, 0x00))
            .unwrap();
        let pixel = |x: usize, y: usize| {
            let at = 3 * (y * 40 + x);
            u32::from(png.pixels[at])
                + u32::from(png.pixels[at + 1])
                + u32::from(png.pixels[at + 2])
        };
        assert!(pixel(0, 0) > pixel(20, 15));
        assert!(pixel(20, 15) > pixel(39, 29));
        // L-shaped contours: along the top row brightness falls to the
        // right, along the left column it falls downward.
        assert!(pixel(0, 0) >= pixel(30, 0) && pixel(0, 0) >= pixel(0, 25));
    }

    #[test]
    fn p2_map_02_a_square_covers_its_pixels_and_text_draws() {
        // A 2×2 square from (1,1): coverage 1 inside, 0 outside.
        let mut acc = vec![0.0f32; 5 * 4];
        let square = [(1.0, 1.0), (3.0, 1.0), (3.0, 3.0), (1.0, 3.0), (1.0, 1.0)];
        for pair in square.windows(2) {
            accumulate(&mut acc, 5, 4, pair[0], pair[1]);
        }
        let mut cover = Vec::new();
        for row in 0..4 {
            let mut sum = 0.0f32;
            for col in 0..5 {
                sum += acc[row * 5 + col];
                cover.push((sum.abs().min(1.0) * 10.0).round() / 10.0);
            }
        }
        let near = |a: f32, b: f32| (a - b).abs() < 0.01;
        assert!(near(cover[5 + 1], 1.0) && near(cover[2 * 5 + 2], 1.0));
        assert!(near(cover[0], 0.0) && near(cover[5 + 3], 0.0));
        // A contour of control points only closes on itself.
        let mut segments = 0;
        let points =
            [(0, 0), (100, 0), (100, 100)].map(|(x, y)| crate::font::Point { x, y, on: false });
        flatten(&points, &mut |_, _| segments += 1);
        assert!(segments > 3);
        // Text inks pixels inside its clip and none outside.
        let mut png = canvas(120, 40);
        png.draw_text(4, 24, "Hello", (255, 255, 255), Rect::new(0, 0, 60, 40));
        let inked = |x0: usize, x1: usize| {
            (0..40)
                .flat_map(|y| (x0..x1).map(move |x| (x, y)))
                .filter(|&(x, y)| png.pixels[3 * (y * 120 + x)] > 0)
                .count()
        };
        assert!(inked(0, 60) > 40);
        assert_eq!(inked(60, 120), 0);
    }
}
