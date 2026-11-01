//! `render`'s SVG: the picture ([`crate::picture`]) as a document a browser
//! draws.
//!
//! - **Tiles:** a leaf shaded by `GrandPerspective`'s table is two paths,
//!   the part below its top-left–bottom-right diagonal taking the table top
//!   to bottom and the part above it left to right, each a linear gradient
//!   through nine of the table's steps, shared by every tile of its
//!   colour; edges are crisp, as `GrandPerspective` draws without
//!   anti-aliasing, so the halves meet without a seam.
//! - **Labels** are text in the system's sans-serif (an SVG embeds no
//!   font), each group clipped to its tile.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::{self, Write};

use duscope_core::query::TreeView;

use crate::Rect;
use crate::picture::{Canvas, Metrics, PictureOpts, Rgb, draw, rgb_of, shade};

/// Writes the picture of the scan in `view` as SVG; the number of tiles
/// painted.
///
/// # Errors
///
/// When `out` cannot be written.
pub fn write_svg(
    view: &dyn TreeView,
    opts: &PictureOpts<'_>,
    out: &mut dyn Write,
) -> io::Result<u64> {
    let metrics = Metrics::of(opts);
    let Metrics {
        width,
        height,
        font,
        ..
    } = metrics;
    let base = hex(rgb_of(opts.paint.pack.roles.base));
    writeln!(out, r#"<?xml version="1.0" encoding="UTF-8"?>"#)?;
    writeln!(
        out,
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}" font-family="Inter, -apple-system, 'Segoe UI', Roboto, sans-serif" font-size="{font}">"#
    )?;
    writeln!(out, "<title>{}</title>", xml(&opts.title))?;
    writeln!(
        out,
        r#"<rect width="{width}" height="{height}" fill="{base}"/>"#
    )?;
    writeln!(out, r#"<g shape-rendering="crispEdges">"#)?;
    let mut canvas = Svg {
        out,
        colors: HashMap::new(),
        tiles_open: true,
        clips: 0,
    };
    let tiles = draw(view, opts, metrics, &mut canvas)?;
    canvas.close_tiles()?;
    let Svg { out, colors, .. } = canvas;
    gradients(out, &colors)?;
    writeln!(out, "</svg>")?;
    Ok(tiles)
}

/// The SVG canvas.
struct Svg<'w> {
    out: &'w mut dyn Write,
    /// The gradients' colours, each with its number.
    colors: HashMap<Rgb, usize>,
    /// Whether the tiles' group is still open.
    tiles_open: bool,
    /// The label clips so far, which number the next.
    clips: usize,
}

impl Svg<'_> {
    /// Ends the tiles' group, before the first label or legend.
    fn close_tiles(&mut self) -> io::Result<()> {
        if self.tiles_open {
            self.tiles_open = false;
            writeln!(self.out, "</g>")?;
        }
        Ok(())
    }
}

impl Canvas for Svg<'_> {
    fn tile(&mut self, rect: Rect, rgb: Rgb) -> io::Result<()> {
        let Rect { x, y, w, h } = rect;
        let (right, bottom) = (u32::from(x) + u32::from(w), u32::from(y) + u32::from(h));
        if w < 3 || h < 3 {
            writeln!(
                self.out,
                "<rect x=\"{x}\" y=\"{y}\" width=\"{w}\" height=\"{h}\" fill=\"{}\"/>",
                hex(rgb)
            )
        } else {
            let next = self.colors.len();
            let n = *self.colors.entry(rgb).or_insert(next);
            writeln!(
                self.out,
                "<path fill=\"url(#v{n})\" d=\"M{x} {y}V{bottom}H{right}Z\"/><path fill=\"url(#h{n})\" d=\"M{x} {y}H{right}V{bottom}Z\"/>"
            )
        }
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
        self.close_tiles()?;
        let n = self.clips;
        self.clips += 1;
        let ink = hex(ink);
        let mut group = format!(
            "<clipPath id=\"c{n}\"><rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\"/></clipPath><g clip-path=\"url(#c{n})\">",
            clip.x, clip.y, clip.w, clip.h
        );
        let mut baseline = baseline;
        for line in lines {
            let _ = write!(
                group,
                "<text x=\"{left}\" y=\"{baseline}\" fill=\"{ink}\">{}</text>",
                xml(line)
            );
            baseline += step;
        }
        group.push_str("</g>");
        writeln!(self.out, "{group}")
    }

    fn swatch(&mut self, x: u32, y: u32, side: u32, rgb: Rgb) -> io::Result<()> {
        self.close_tiles()?;
        writeln!(
            self.out,
            "<rect x=\"{x}\" y=\"{y}\" width=\"{side}\" height=\"{side}\" fill=\"{}\"/>",
            hex(rgb)
        )
    }

    fn text(&mut self, left: u32, baseline: u32, text: &str, ink: Rgb) -> io::Result<()> {
        self.close_tiles()?;
        writeln!(
            self.out,
            "<text x=\"{left}\" y=\"{baseline}\" fill=\"{}\">{}</text>",
            hex(ink),
            xml(text)
        )
    }
}

/// The two gradients of each colour: `v` top to bottom, `h` left to right.
fn gradients(out: &mut dyn Write, colors: &HashMap<Rgb, usize>) -> io::Result<()> {
    let mut colors: Vec<(&Rgb, &usize)> = colors.iter().collect();
    colors.sort_by_key(|&(_, n)| *n);
    writeln!(out, "<defs>")?;
    for (&rgb, n) in colors {
        let mut stops = String::new();
        for k in 0..=8u32 {
            let index = (256 - k * 32).min(255);
            let offset = f64::from(k) / 8.0;
            let _ = write!(
                stops,
                "<stop offset=\"{offset}\" stop-color=\"{}\"/>",
                hex(shade(rgb, index))
            );
        }
        writeln!(
            out,
            "<linearGradient id=\"v{n}\" x2=\"0\" y2=\"1\">{stops}</linearGradient><linearGradient id=\"h{n}\">{stops}</linearGradient>"
        )?;
    }
    writeln!(out, "</defs>")
}

/// `#rrggbb`.
fn hex((red, green, blue): Rgb) -> String {
    format!("#{red:02x}{green:02x}{blue:02x}")
}

/// `text` escaped for XML character data and attribute values; the
/// characters XML 1.0 cannot hold at all as `\u{…}`.
fn xml(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\t' | '\n' | '\r' => out.push(c),
            c if u32::from(c) < 0x20 || c == '\u{fffe}' || c == '\u{ffff}' => {
                let _ = write!(out, "\\u{{{:x}}}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::xml;

    #[test]
    fn p2_map_01_names_are_escaped_for_xml() {
        assert_eq!(xml("a & <b> \"c\""), "a &amp; &lt;b&gt; &quot;c&quot;");
        assert_eq!(xml("\u{1}\u{fffe}"), "\\u{1}\\u{fffe}");
    }
}
