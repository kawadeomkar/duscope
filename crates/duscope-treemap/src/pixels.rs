//! `--treemap pixels`: the map drawn in pixels on a terminal that shows
//! images, through ratatui-image, behind the `pixels` Cargo feature.
//!
//! - **The probe** ([`probe`]) runs only when `--treemap pixels` asks for
//!   it, and only when stdin and stdout are both terminals: ratatui-image's
//!   picker writes its questions to stdout (Kitty graphics' `a=q` query, the
//!   device attributes, which name Sixel, the cell size in pixels, and the
//!   status report every terminal answers, which ends the wait early) and
//!   reads the answers from stdin, giving up after two seconds without
//!   one. Kitty graphics
//!   wins over Sixel; iTerm2 is known by the environment the picker reads
//!   (`TERM_PROGRAM`, `LC_TERMINAL`). A terminal that answers none of the
//!   three, or gives no cell size, gets `None`: the map in cells. The
//!   picker gives the terminal its modes back once the terminal answers;
//!   when it does not answer, the modes are put back here.
//! - **The picture** ([`Pixels::map`]) is `render`'s
//!   ([`crate::picture`]), at the map's size in the terminal's pixels
//!   (columns × the cell's width by rows × its height), without the legend
//!   and totals under it, which the terminal's own lines carry, and with
//!   its labels at the terminal's text size: a label's line is one row.
//! - **The cells** ([`PixelMap`]) are what the terminal is sent for each
//!   cell. Kitty's placeholders fill every cell, their colour naming the
//!   image, the image's transmission in front of the first; Sixel and
//!   iTerm2 draw the whole picture from the first cell, which covers the
//!   others. The viewer copies them into its frame, and the static map
//!   writes them with [`PixelMap::to_ansi`].
//!
//! Without the feature [`COMPILED`] is false, nothing here touches the
//! terminal, and [`probe`] answers
//! [`crate::TreemapError::PixelsNotCompiled`].

use std::fmt::Write as _;

/// Whether this build has the `pixels` feature.
pub const COMPILED: bool = cfg!(feature = "pixels");

/// What a terminal is sent for one cell of a map drawn in pixels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PixelCell {
    /// The text: an image's escapes, or Kitty's placeholder character, or
    /// both. It takes one column.
    pub symbol: String,
    /// Its colour, which Kitty reads an image's id from.
    pub fg: Option<(u8, u8, u8)>,
}

/// A map drawn in pixels: the cells a terminal is sent, row by row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PixelMap {
    width: u16,
    height: u16,
    /// `None` for a cell the picture drawn from an earlier one covers.
    cells: Vec<Option<PixelCell>>,
}

impl PixelMap {
    /// A map of `width` × `height` cells; `cells` row by row, `None` for a
    /// cell an earlier one's picture covers. Cells past the end are
    /// covered, and cells beyond `width` × `height` dropped.
    #[must_use]
    pub fn new(width: u16, height: u16, mut cells: Vec<Option<PixelCell>>) -> PixelMap {
        cells.resize(usize::from(width) * usize::from(height), None);
        PixelMap {
            width,
            height,
            cells,
        }
    }

    /// Columns.
    #[must_use]
    pub fn width(&self) -> u16 {
        self.width
    }

    /// Rows.
    #[must_use]
    pub fn height(&self) -> u16 {
        self.height
    }

    /// What column `x` of row `y` is sent; `None` outside the map and for
    /// a cell an earlier one's picture covers.
    #[must_use]
    pub fn get(&self, x: u16, y: u16) -> Option<&PixelCell> {
        if x >= self.width || y >= self.height {
            return None;
        }
        self.cells[usize::from(y) * usize::from(self.width) + usize::from(x)].as_ref()
    }

    /// The map as a command writes it, from the start of a line: room made
    /// first (a newline for each row, then back up), so that a picture
    /// drawn from the first row never scrolls the screen under it; then
    /// each row's cells, the covered ones stepped over, and a new line. A
    /// cell holding escapes is written between a saved and a restored
    /// cursor and followed by one step right, so the next cell starts one
    /// column on whatever the protocol did with the cursor. The cursor ends
    /// at the start of the line under the map.
    #[must_use]
    pub fn to_ansi(&self) -> String {
        let mut out = String::new();
        if self.width == 0 || self.height == 0 {
            return out;
        }
        out.push_str(&"\n".repeat(usize::from(self.height)));
        let _ = write!(out, "\x1b[{}A\r", self.height);
        for y in 0..self.height {
            let mut column = 0u16;
            let mut colour = None;
            for x in 0..self.width {
                let Some(cell) = self.get(x, y) else {
                    continue;
                };
                if x > column {
                    let _ = write!(out, "\x1b[{}C", x - column);
                }
                if colour != Some(cell.fg) {
                    match cell.fg {
                        Some((r, g, b)) => {
                            let _ = write!(out, "\x1b[0;38;2;{r};{g};{b}m");
                        }
                        None => out.push_str("\x1b[0m"),
                    }
                    colour = Some(cell.fg);
                }
                if cell.symbol.contains('\x1b') {
                    let _ = write!(out, "\x1b7{}\x1b8\x1b[C", cell.symbol);
                } else {
                    out.push_str(&cell.symbol);
                }
                column = x + 1;
            }
            out.push_str("\x1b[0m\r\n");
        }
        out
    }
}

pub use imp::{Pixels, probe};

#[cfg(feature = "pixels")]
mod imp {
    use std::io::{self, IsTerminal};

    use duscope_core::query::TreeView;
    use image::{DynamicImage, RgbImage};
    use ratatui_core::buffer::{Buffer, CellDiffOption};
    use ratatui_core::layout::{Rect, Size};
    use ratatui_core::style::Color;
    use ratatui_core::widgets::Widget;
    use ratatui_image::picker::{Picker, ProtocolType};
    use ratatui_image::{Image, Resize};
    use rustix::termios::{self, OptionalActions};

    use super::{PixelCell, PixelMap};
    use crate::paint::PaintOpts;
    use crate::picture::{MAX_SIDE, Metrics, PictureOpts};
    use crate::{Algorithm, TreemapError, png};

    /// What draws the map in pixels: ratatui-image's picker, holding the
    /// protocol (Kitty graphics, Sixel or iTerm2) and the cell size the
    /// terminal answered with.
    #[derive(Debug, Clone)]
    pub struct Pixels {
        picker: Picker,
    }

    /// Asks the terminal which image protocol it speaks; `None` where it
    /// speaks none of the three, or stdin and stdout are not both
    /// terminals, which leaves the map in cells.
    ///
    /// # Errors
    ///
    /// Never in a build with the feature.
    pub fn probe() -> Result<Option<Pixels>, TreemapError> {
        if !(io::stdin().is_terminal() && io::stdout().is_terminal()) {
            return Ok(None);
        }
        // The picker turns echo and line editing off while it waits, and
        // turns them back on only once the terminal answers.
        let modes = termios::tcgetattr(io::stdin()).ok();
        let answered = Picker::from_query_stdio();
        if let Some(modes) = &modes {
            let _ = termios::tcsetattr(io::stdin(), OptionalActions::Now, modes);
        }
        let Ok(picker) = answered else {
            return Ok(None);
        };
        // Half blocks are the picker's answer for a terminal that showed
        // no image protocol, or no cell size; the map has its own. A cell
        // of no pixels is no size either.
        let font = picker.font_size();
        if picker.protocol_type() == ProtocolType::Halfblocks || font.width == 0 || font.height == 0
        {
            return Ok(None);
        }
        Ok(Some(Pixels { picker }))
    }

    impl Pixels {
        /// The map of `view` painted with `paint` (`render`'s picture) in
        /// `columns` × `rows` cells.
        ///
        /// # Errors
        ///
        /// [`TreemapError::PixelsEncode`] when the picture would be wider
        /// or taller than [`MAX_SIDE`] pixels, or its encoder fails.
        pub fn map(
            &self,
            view: &dyn TreeView,
            paint: &PaintOpts<'_>,
            columns: u16,
            rows: u16,
        ) -> Result<PixelMap, TreemapError> {
            let font = self.picker.font_size();
            let (cell_w, cell_h) = (font.width, font.height);
            let side = |cells: u16, cell: u16| {
                u16::try_from(u32::from(cells) * u32::from(cell))
                    .ok()
                    .filter(|&px| px <= MAX_SIDE)
                    .ok_or_else(|| {
                        TreemapError::PixelsEncode(format!(
                            "{columns}×{rows} cells of {cell_w}×{cell_h} pixels pass {MAX_SIDE} pixels"
                        ))
                    })
            };
            let (width, height) = (side(columns, cell_w)?, side(rows, cell_h)?);
            let opts = PictureOpts {
                paint: paint.clone(),
                algorithm: Algorithm::Squarify,
                width,
                height,
                title: String::new(),
                legend: Vec::new(),
                summary: String::new(),
            };
            let encode = TreemapError::PixelsEncode;
            let (canvas, _) = png::raster(view, &opts, metrics(&opts, cell_h))
                .map_err(|e| encode(e.to_string()))?;
            let image = RgbImage::from_raw(u32::from(width), u32::from(height), canvas.pixels)
                .ok_or_else(|| encode("the picture's pixels do not fill it".to_owned()))?;
            let protocol = self
                .picker
                .new_protocol(
                    DynamicImage::ImageRgb8(image),
                    Size::new(columns, rows),
                    Resize::Fit(None),
                )
                .map_err(|e| encode(e.to_string()))?;
            let area = Rect::new(0, 0, columns, rows);
            let mut buf = Buffer::empty(area);
            Image::new(&protocol).render(area, &mut buf);
            Ok(cells(&buf))
        }
    }

    /// The picture's sizes: `render`'s, with no band under the map and
    /// the labels' font at the terminal's text size, so that a label's
    /// line (1.3 em) is one row of `cell_h` pixels.
    fn metrics(opts: &PictureOpts<'_>, cell_h: u16) -> Metrics {
        let mut metrics = Metrics::of(opts);
        metrics.font = (u32::from(cell_h) * 10 / 13).max(8);
        metrics.cell_w = (metrics.font * 3).div_ceil(5);
        metrics.cell_h = (metrics.font * 13).div_ceil(10);
        metrics.footer = false;
        metrics.map_h = metrics.height;
        metrics
    }

    /// What `buf` holds, cell by cell: the cells the diff skips are
    /// covered.
    fn cells(buf: &Buffer) -> PixelMap {
        let area = buf.area;
        let mut cells = Vec::with_capacity(usize::from(area.width) * usize::from(area.height));
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                let cell = &buf[(x, y)];
                let covered = matches!(cell.diff_option, CellDiffOption::Skip);
                cells.push((!covered).then(|| PixelCell {
                    symbol: cell.symbol().to_owned(),
                    fg: match cell.fg {
                        Color::Rgb(r, g, b) => Some((r, g, b)),
                        _ => None,
                    },
                }));
            }
        }
        PixelMap::new(area.width, area.height, cells)
    }
}

#[cfg(not(feature = "pixels"))]
mod imp {
    use std::convert::Infallible;

    use duscope_core::query::TreeView;

    use super::PixelMap;
    use crate::TreemapError;
    use crate::paint::PaintOpts;

    /// What draws the map in pixels; none exists without the `pixels`
    /// feature.
    #[derive(Debug, Clone)]
    pub struct Pixels {
        never: Infallible,
    }

    /// Asks the terminal which image protocol it speaks; without the
    /// feature, nothing is asked.
    ///
    /// # Errors
    ///
    /// Always [`TreemapError::PixelsNotCompiled`].
    pub fn probe() -> Result<Option<Pixels>, TreemapError> {
        Err(TreemapError::PixelsNotCompiled)
    }

    impl Pixels {
        /// The map of `view` in `columns` × `rows` cells.
        ///
        /// # Errors
        ///
        /// None: no [`Pixels`] exists without the feature.
        pub fn map(
            &self,
            _view: &dyn TreeView,
            _paint: &PaintOpts<'_>,
            _columns: u16,
            _rows: u16,
        ) -> Result<PixelMap, TreemapError> {
            match self.never {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{COMPILED, PixelCell, PixelMap};

    fn cell(symbol: &str, fg: Option<(u8, u8, u8)>) -> PixelCell {
        PixelCell {
            symbol: symbol.to_owned(),
            fg,
        }
    }

    #[test]
    fn p4_tui_07_a_build_without_the_feature_answers_not_compiled() {
        assert_eq!(COMPILED, cfg!(feature = "pixels"));
        // With the feature the probe would ask the terminal the tests run
        // in; the PTY tests ask a mocked one.
        if !COMPILED {
            assert_eq!(
                super::probe().unwrap_err(),
                crate::TreemapError::PixelsNotCompiled
            );
        }
    }

    #[test]
    fn p4_tui_07_sixel_and_iterm2_draw_the_picture_from_the_first_cell() {
        // The first cell holds the picture's escapes and covers the rest:
        // room for two rows, back up, the escapes between a saved and a
        // restored cursor, one step right, then the covered rows.
        let picture = "\x1b[3X\x1bPq#0~\x1b\\";
        let map = PixelMap::new(3, 2, vec![Some(cell(picture, None))]);
        assert_eq!(map.get(0, 0).unwrap().symbol, picture);
        assert_eq!(map.get(1, 0), None);
        assert_eq!(map.get(2, 1), None);
        assert_eq!(
            map.to_ansi(),
            format!("\n\n\x1b[2A\r\x1b[0m\x1b7{picture}\x1b8\x1b[C\x1b[0m\r\n\x1b[0m\r\n")
        );
    }

    #[test]
    fn p4_tui_07_kitty_placeholders_fill_their_cells_in_the_image_colour() {
        // Every cell a placeholder in the colour that names the image, the
        // first after the transmission; the colour is set once a row.
        let id = Some((0, 1, 2));
        let first = "\x1b_Gi=258,a=T,U=1;AAAA\x1b\\\u{10EEEE}\u{305}\u{305}\u{305}";
        let map = PixelMap::new(
            2,
            2,
            vec![
                Some(cell(first, id)),
                Some(cell("\u{10EEEE}", id)),
                Some(cell("\u{10EEEE}\u{30D}\u{305}\u{305}", id)),
                Some(cell("\u{10EEEE}", id)),
            ],
        );
        let colour = "\x1b[0;38;2;0;1;2m";
        assert_eq!(
            map.to_ansi(),
            format!(
                "\n\n\x1b[2A\r{colour}\x1b7{first}\x1b8\x1b[C\u{10EEEE}\x1b[0m\r\n\
                 {colour}\u{10EEEE}\u{30D}\u{305}\u{305}\u{10EEEE}\x1b[0m\r\n"
            )
        );
    }

    #[test]
    fn p4_tui_07_covered_cells_are_stepped_over() {
        let map = PixelMap::new(
            4,
            1,
            vec![None, None, Some(cell("x", None)), Some(cell("y", None))],
        );
        assert_eq!(map.to_ansi(), "\n\x1b[1A\r\x1b[2C\x1b[0mxy\x1b[0m\r\n");
        // Outside the map nothing is sent, and an empty map writes nothing.
        assert_eq!(map.get(4, 0), None);
        assert_eq!(map.get(0, 1), None);
        assert_eq!(PixelMap::new(0, 3, Vec::new()).to_ansi(), "");
        // Cells past the end are covered, and extra cells dropped.
        let short = PixelMap::new(2, 1, vec![Some(cell("a", None))]);
        assert_eq!(short.get(1, 0), None);
        let long = PixelMap::new(1, 1, vec![Some(cell("a", None)), Some(cell("b", None))]);
        assert_eq!(long.to_ansi(), "\n\x1b[1A\r\x1b[0ma\x1b[0m\r\n");
    }
}
