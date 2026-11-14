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
//! - **The picture** ([`Picture::draw`]) is `render`'s
//!   ([`crate::picture`]), at the map's size in the terminal's pixels
//!   (columns × the cell's width by rows × its height), without the legend
//!   and totals under it, which the terminal's own lines carry, and with
//!   its labels at the terminal's text size: a label's line is one row.
//!   The viewer's picture may be of the folder it zooms to, in the whole
//!   map's colours ([`Base`]), and notes the tiles it lays out, so that the
//!   viewer marks its cursor in the picture itself ([`Picture::marked`]):
//!   an accent outline around the cursor's tile and its ancestors'
//!   outlines in the link colour, as the cell map marks them. A cursor
//!   move encodes the marked picture again; the picture is drawn once.
//! - **The cells** ([`PixelMap`]) are what the terminal is sent for each
//!   cell. Kitty's placeholders fill every cell, their colour naming the
//!   image, the image's transmission in front of the first; Sixel and
//!   iTerm2 draw the whole picture from the first cell, which covers the
//!   others. The viewer copies them into its frame, and the static map
//!   writes them with [`PixelMap::to_ansi`].
//! - **Inside tmux** ([`inside_tmux`]) the probe is not run: the picker
//!   would first run `tmux set -p allow-passthrough on`, another program,
//!   which the binary never runs, changing an option of the pane that
//!   outlives duscope.
//!
//! Without the feature [`COMPILED`] is false, nothing here touches the
//! terminal, and [`probe`] answers
//! [`crate::TreemapError::PixelsNotCompiled`]. A [`Picture`] is drawn and
//! marked either way; only its encoding needs the feature.

use std::ffi::OsString;
use std::fmt::Write as _;

use duscope_core::query::TreeView;
use duscope_core::rules::Class;

use crate::paint::{Base, Drawn, PaintOpts, Worked};
use crate::picture::{MAX_SIDE, Metrics, PictureOpts, Rgb, Within, rgb_of};
use crate::{Algorithm, Rect, TreemapError, png};

/// Whether this build has the `pixels` feature.
pub const COMPILED: bool = cfg!(feature = "pixels");

/// Whether duscope runs inside tmux, read from the environment through
/// `var`: `TERM` starting with `tmux` or `TERM_PROGRAM` of `tmux`, as
/// ratatui-image's picker reads it, or `TMUX`, which tmux sets in each of
/// its panes. There the picker runs `tmux set -p allow-passthrough on`,
/// and waits for it, before it asks the terminal anything; so [`probe`]
/// does not ask, and the map stays in cells.
#[must_use]
pub fn inside_tmux(var: impl Fn(&str) -> Option<OsString>) -> bool {
    var("TMUX").is_some_and(|v| !v.is_empty())
        || var("TERM").is_some_and(|v| v.as_encoded_bytes().starts_with(b"tmux"))
        || var("TERM_PROGRAM").is_some_and(|v| v == "tmux")
}

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

/// An image protocol a terminal speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    /// Kitty graphics, through Unicode placeholders.
    Kitty,
    /// Sixel.
    Sixel,
    /// iTerm2's inline images.
    Iterm2,
}

/// A picture's pixels, ready to be encoded for the terminal: `columns` ×
/// `rows` cells of it, in 8-bit RGB, row by row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Raster {
    columns: u16,
    rows: u16,
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl Raster {
    /// Its width in pixels.
    #[must_use]
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Its height in pixels.
    #[must_use]
    pub fn height(&self) -> u32 {
        self.height
    }

    /// The colour of the pixel at `x`, `y`; `None` outside.
    #[must_use]
    pub fn pixel(&self, x: u32, y: u32) -> Option<(u8, u8, u8)> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let at = 3 * (y as usize * self.width as usize + x as usize);
        let rgb = self.pixels.get(at..at + 3)?;
        Some((rgb[0], rgb[1], rgb[2]))
    }

    /// Fills the pixels from `x0`, `y0` up to `x1`, `y1` (not included)
    /// with `rgb`, as far as the picture goes.
    fn fill(&mut self, (x0, y0): (u32, u32), (x1, y1): (u32, u32), rgb: Rgb) {
        let (x1, y1) = (x1.min(self.width), y1.min(self.height));
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        let width = self.width as usize;
        for y in y0 as usize..y1 as usize {
            let row = 3 * (y * width);
            let Some(span) = self
                .pixels
                .get_mut(row + 3 * x0 as usize..row + 3 * x1 as usize)
            else {
                continue;
            };
            for pixel in span.chunks_exact_mut(3) {
                pixel.copy_from_slice(&[rgb.0, rgb.1, rgb.2]);
            }
        }
    }

    /// Draws the band `thickness` pixels wide just inside `rect`'s edges
    /// in `rgb`.
    fn frame(&mut self, rect: Rect, thickness: u32, rgb: Rgb) {
        let (x, y) = (u32::from(rect.x), u32::from(rect.y));
        let (right, bottom) = (x + u32::from(rect.w), y + u32::from(rect.h));
        let t = thickness.min(u32::from(rect.w)).min(u32::from(rect.h));
        if t == 0 {
            return;
        }
        self.fill((x, y), (right, y + t), rgb);
        self.fill((x, bottom - t), (right, bottom), rgb);
        self.fill((x, y + t), (x + t, bottom - t), rgb);
        self.fill((right - t, y + t), (right, bottom - t), rgb);
    }
}

/// The map in pixels before it is encoded: the picture of a tree at a
/// size in cells, the tiles it laid out, and the colours it marks a cursor
/// in.
#[derive(Debug, Clone)]
pub struct Picture {
    /// A cell, in pixels.
    cell: (u16, u16),
    raster: Raster,
    /// The tiles laid out, in pixels, with the picture's ids.
    tiles: Vec<Drawn>,
    /// The cursor's outline and fill.
    accent: Rgb,
    /// The cursor's ancestors' outlines.
    link: Rgb,
    /// The line inside the cursor's outline, which sets it off the tile.
    crust: Rgb,
}

impl Picture {
    /// The picture of `view` painted with `paint` (`render`'s) in
    /// `columns` × `rows` cells of `cell` pixels (width, height): `class_of`
    /// holds the classes of `view`'s entries by id where they are known
    /// already, and `base` says where `view`'s root stands in the scan
    /// (the viewer's zoom), so that its tiles keep the whole map's colours.
    ///
    /// # Errors
    ///
    /// [`TreemapError::PixelsEncode`] when the picture would be wider or
    /// taller than [`MAX_SIDE`] pixels, or hold more than
    /// [`png::MAX_PIXELS`].
    pub fn draw(
        view: &dyn TreeView,
        paint: &PaintOpts<'_>,
        columns: u16,
        rows: u16,
        cell: (u16, u16),
        class_of: Option<&[Class]>,
        base: Base,
    ) -> Result<Picture, TreemapError> {
        let within = Within {
            base,
            class_of,
            dominant: None,
            note: true,
        };
        Picture::drawn(view, paint, columns, rows, cell, &within)
    }

    /// The picture [`Picture::draw`] draws, with the classes the viewer
    /// worked out: each entry's, and each folder's dominant ones where it
    /// knows them, so that a folder drawn as one tile is not walked.
    ///
    /// # Errors
    ///
    /// As [`Picture::draw`]'s.
    pub fn draw_worked(
        view: &dyn TreeView,
        paint: &PaintOpts<'_>,
        columns: u16,
        rows: u16,
        cell: (u16, u16),
        worked: Worked<'_>,
        base: Base,
    ) -> Result<Picture, TreemapError> {
        let within = Within {
            base,
            class_of: Some(worked.class_of),
            dominant: Some(worked.dominant),
            note: true,
        };
        Picture::drawn(view, paint, columns, rows, cell, &within)
    }

    /// The picture of `view` in `columns` × `rows` cells of `cell` pixels,
    /// drawn `within` the scan.
    fn drawn(
        view: &dyn TreeView,
        paint: &PaintOpts<'_>,
        columns: u16,
        rows: u16,
        cell: (u16, u16),
        within: &Within<'_>,
    ) -> Result<Picture, TreemapError> {
        let (cell_w, cell_h) = cell;
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
        let (canvas, drawing) = png::raster(view, &opts, metrics(&opts, cell_h), within)
            .map_err(|e| TreemapError::PixelsEncode(e.to_string()))?;
        let roles = paint.pack.roles;
        Ok(Picture {
            cell,
            raster: Raster {
                columns,
                rows,
                width: u32::from(width),
                height: u32::from(height),
                pixels: canvas.pixels,
            },
            tiles: drawing.drawn,
            accent: rgb_of(roles.accent),
            link: rgb_of(roles.link),
            crust: rgb_of(roles.crust),
        })
    }

    /// The tiles laid out, in pixels: each entry's own (a folder whose
    /// children are laid out inside it as an open box), each folder's pool
    /// and files, with the ids of the view drawn.
    #[must_use]
    pub fn tiles(&self) -> &[Drawn] {
        &self.tiles
    }

    /// The whole picture, in pixels.
    #[must_use]
    pub fn area(&self) -> Rect {
        let side = |px: u32| u16::try_from(px).unwrap_or(u16::MAX);
        Rect::new(0, 0, side(self.raster.width), side(self.raster.height))
    }

    /// The pixels, unmarked.
    #[must_use]
    pub fn raster(&self) -> &Raster {
        &self.raster
    }

    /// The pixels, unmarked, for the encoder.
    #[must_use]
    pub fn into_raster(self) -> Raster {
        self.raster
    }

    /// The pixels with a cursor's marks, as the cell map draws them: the
    /// outlines of `links` (its ancestors' tiles) in the link colour, then
    /// an accent outline just inside `cursor` (its tile), set off the tile
    /// by a line of crust. A tile too small to hold the outline is filled
    /// with the accent over every cell it touches, as the cell map fills a
    /// tile under 2×2 cells. The outline is an eighth of a cell's height
    /// thick (at least 2 pixels), the links half that.
    #[must_use]
    pub fn marked(&self, cursor: Rect, links: &[Rect]) -> Raster {
        let mut raster = self.raster.clone();
        let (cell_w, cell_h) = (u32::from(self.cell.0), u32::from(self.cell.1));
        let thick = cell_h.div_ceil(8).max(2);
        for &link in links {
            raster.frame(link, (thick / 2).max(1), self.link);
        }
        let room = 2 * thick + 2;
        if u32::from(cursor.w) >= room && u32::from(cursor.h) >= room {
            raster.frame(cursor, thick, self.accent);
            // Inside the outline: the cursor's tile is at least 2 × thick
            // + 2 pixels each way.
            let t = u16::try_from(thick).unwrap_or(0);
            let inner = Rect::new(
                cursor.x + t,
                cursor.y + t,
                cursor.w.saturating_sub(2 * t),
                cursor.h.saturating_sub(2 * t),
            );
            raster.frame(inner, 1, self.crust);
        } else if cell_w > 0 && cell_h > 0 {
            // The cells it touches, at least one.
            let (x, y) = (u32::from(cursor.x), u32::from(cursor.y));
            let right = (x + u32::from(cursor.w).max(1)).div_ceil(cell_w) * cell_w;
            let bottom = (y + u32::from(cursor.h).max(1)).div_ceil(cell_h) * cell_h;
            raster.fill(
                (x / cell_w * cell_w, y / cell_h * cell_h),
                (right, bottom),
                self.accent,
            );
        }
        raster
    }
}

/// The picture's sizes: `render`'s, with no band under the map and the
/// labels' font at the terminal's text size, so that a label's line
/// (1.3 em) is one row of `cell_h` pixels.
fn metrics(opts: &PictureOpts<'_>, cell_h: u16) -> Metrics {
    let mut metrics = Metrics::of(opts);
    metrics.font = (u32::from(cell_h) * 10 / 13).max(8);
    metrics.cell_w = (metrics.font * 3).div_ceil(5);
    metrics.cell_h = (metrics.font * 13).div_ceil(10);
    metrics.footer = false;
    metrics.map_h = metrics.height;
    metrics
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

    use super::{Picture, PixelCell, PixelMap, Protocol, Raster};
    use crate::TreemapError;
    use crate::paint::{Base, PaintOpts};

    /// What draws the map in pixels: ratatui-image's picker, holding the
    /// protocol (Kitty graphics, Sixel or iTerm2) and the cell size the
    /// terminal answered with.
    #[derive(Debug, Clone)]
    pub struct Pixels {
        picker: Picker,
    }

    /// Asks the terminal which image protocol it speaks; `None` where it
    /// speaks none of the three, where stdin and stdout are not both
    /// terminals, and inside tmux ([`inside_tmux`](super::inside_tmux)),
    /// where nothing is asked, which leaves the map in cells.
    ///
    /// # Errors
    ///
    /// Never in a build with the feature.
    pub fn probe() -> Result<Option<Pixels>, TreemapError> {
        if !(io::stdin().is_terminal() && io::stdout().is_terminal()) {
            return Ok(None);
        }
        // The picker would run tmux, and turn on an option of the pane
        // that outlives the process.
        if super::inside_tmux(|name| std::env::var_os(name)) {
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
        /// or taller than [`crate::picture::MAX_SIDE`] pixels, or its encoder fails.
        pub fn map(
            &self,
            view: &dyn TreeView,
            paint: &PaintOpts<'_>,
            columns: u16,
            rows: u16,
        ) -> Result<PixelMap, TreemapError> {
            let picture = Picture::draw(
                view,
                paint,
                columns,
                rows,
                self.cell(),
                None,
                Base::default(),
            )?;
            self.encode(picture.into_raster())
        }

        /// The terminal's cell, in pixels (width, height).
        #[must_use]
        pub fn cell(&self) -> (u16, u16) {
            let font = self.picker.font_size();
            (font.width, font.height)
        }

        /// The cells the terminal is sent for `raster` (a [`Picture`]'s
        /// pixels, marked or not).
        ///
        /// # Errors
        ///
        /// [`TreemapError::PixelsEncode`] when its encoder fails.
        pub fn encode(&self, raster: Raster) -> Result<PixelMap, TreemapError> {
            let encode = TreemapError::PixelsEncode;
            let Raster {
                columns,
                rows,
                width,
                height,
                pixels,
            } = raster;
            let image = RgbImage::from_raw(width, height, pixels)
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

        /// What draws in `protocol` on a terminal whose cell is `cell`
        /// pixels (width, height), without asking the terminal: for tests
        /// and benchmarks, which have none to ask. Always `Some` in a build
        /// with the feature.
        #[doc(hidden)]
        #[must_use]
        pub fn assume(protocol: Protocol, cell: (u16, u16)) -> Option<Pixels> {
            // The only constructor that takes a cell size; the picker it
            // makes is otherwise the one a probe makes.
            #[allow(deprecated)]
            let mut picker = Picker::from_fontsize(ratatui_image::FontSize::new(cell.0, cell.1));
            picker.set_protocol_type(match protocol {
                Protocol::Kitty => ProtocolType::Kitty,
                Protocol::Sixel => ProtocolType::Sixel,
                Protocol::Iterm2 => ProtocolType::Iterm2,
            });
            Some(Pixels { picker })
        }
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

    use super::{PixelMap, Protocol, Raster};
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

        /// The terminal's cell, in pixels.
        #[must_use]
        pub fn cell(&self) -> (u16, u16) {
            match self.never {}
        }

        /// The cells the terminal is sent for `raster`.
        ///
        /// # Errors
        ///
        /// None: no [`Pixels`] exists without the feature.
        pub fn encode(&self, _raster: Raster) -> Result<PixelMap, TreemapError> {
            match self.never {}
        }

        /// What draws in a protocol without asking the terminal: without
        /// the feature, nothing.
        #[doc(hidden)]
        #[must_use]
        pub fn assume(_protocol: Protocol, _cell: (u16, u16)) -> Option<Pixels> {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{COMPILED, Picture, PixelCell, PixelMap, Raster};
    use crate::Rect;

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
    fn p4_tui_07_tmux_is_known_by_its_environment() {
        let inside = |vars: &[(&str, &str)]| {
            super::inside_tmux(|name| {
                vars.iter()
                    .find(|(n, _)| *n == name)
                    .map(|(_, v)| (*v).into())
            })
        };
        // Each of the picker's two signs, and tmux's own, alone.
        assert!(inside(&[("TERM", "tmux-256color")]));
        assert!(inside(&[("TERM", "tmux")]));
        assert!(inside(&[("TERM_PROGRAM", "tmux")]));
        assert!(inside(&[("TMUX", "/tmp/tmux-501/default,4242,0")]));
        assert!(inside(&[
            ("TERM", "screen-256color"),
            ("TMUX", "/tmp/tmux-501/default,4242,0"),
        ]));
        // Outside tmux, and an empty `TMUX`.
        assert!(!inside(&[]));
        assert!(!inside(&[("TERM", "xterm-256color")]));
        assert!(!inside(&[
            ("TERM", "xterm-kitty"),
            ("TERM_PROGRAM", "iTerm.app")
        ]));
        assert!(!inside(&[("TERM", "screen-256color")]));
        assert!(!inside(&[("TERM_PROGRAM", "tmux-like")]));
        assert!(!inside(&[("TMUX", "")]));
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

    #[test]
    fn p4_tui_03_the_cursor_is_outlined_in_the_picture_and_a_small_tile_filled() {
        let (accent, link, crust, ground) =
            ((250, 220, 170), (110, 200, 230), (17, 17, 27), (1, 2, 3));
        let picture = Picture {
            cell: (10, 20),
            raster: Raster {
                columns: 20,
                rows: 10,
                width: 200,
                height: 200,
                pixels: [ground.0, ground.1, ground.2].repeat(200 * 200),
            },
            tiles: Vec::new(),
            accent,
            link,
            crust,
        };
        // A tile of 60×40: an accent band 3 pixels thick (an eighth of a
        // 20-pixel cell), a line of crust inside it; its ancestor's edge in
        // the link colour, a pixel thick.
        let marked = picture.marked(Rect::new(20, 40, 60, 40), &[Rect::new(10, 30, 120, 100)]);
        for d in 0..3 {
            assert_eq!(marked.pixel(50, 40 + d), Some(accent));
            assert_eq!(marked.pixel(50, 79 - d), Some(accent));
            assert_eq!(marked.pixel(20 + d, 60), Some(accent));
            assert_eq!(marked.pixel(79 - d, 60), Some(accent));
        }
        assert_eq!(marked.pixel(50, 43), Some(crust));
        assert_eq!(marked.pixel(50, 60), Some(ground));
        assert_eq!(marked.pixel(100, 30), Some(link));
        assert_eq!(marked.pixel(10, 100), Some(link));
        assert_eq!(marked.pixel(100, 31), Some(ground));
        // The picture itself is left as drawn.
        assert_eq!(picture.raster().pixel(50, 40), Some(ground));
        // A tile of 5×5 pixels has no room for the outline: the one cell it
        // lies in is filled; one across a cell's edge fills both cells.
        let small = picture.marked(Rect::new(23, 44, 5, 5), &[]);
        assert_eq!(small.pixel(20, 40), Some(accent));
        assert_eq!(small.pixel(29, 59), Some(accent));
        assert_eq!(small.pixel(30, 40), Some(ground));
        assert_eq!(small.pixel(20, 60), Some(ground));
        let across = picture.marked(Rect::new(27, 44, 5, 5), &[]);
        assert_eq!(across.pixel(20, 40), Some(accent));
        assert_eq!(across.pixel(39, 59), Some(accent));
        assert_eq!(across.pixel(40, 40), Some(ground));
    }
}
