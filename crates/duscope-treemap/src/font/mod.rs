//! The bundled font: Inter 4.1 Regular (github.com/rsms/inter, SIL Open
//! Font License 1.1, `OFL.txt` beside it), cut to ASCII, Latin-1 and a
//! little punctuation (`…`, dashes, quotes, `•`, `€`), its composite glyphs
//! flattened and its hinting dropped: what the PNG's labels are drawn in.
//!
//! It is read as the TrueType it is: `head` (units per em), `hhea` and
//! `hmtx` (advances), `cmap` (format 4), `loca` (long offsets) and `glyf`
//! (simple glyphs: on- and off-curve points of quadratic contours). Every
//! read is bounds-checked, so a damaged file gives no glyphs, never a
//! panic.

/// The font file.
const DATA: &[u8] = include_bytes!("Inter-Regular-subset.ttf");

/// A point of a glyph's outline, in font units, y up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Point {
    pub x: i16,
    pub y: i16,
    /// On the curve, or the control point of a quadratic segment.
    pub on: bool,
}

/// The font, its tables found.
#[derive(Debug, Clone)]
pub(crate) struct Font {
    data: &'static [u8],
    units_per_em: u16,
    num_glyphs: u16,
    num_h_metrics: u16,
    hmtx: usize,
    loca: usize,
    glyf: usize,
    /// The format 4 subtable.
    cmap: usize,
}

impl Font {
    /// The bundled Inter; `None` only if the file were damaged.
    pub(crate) fn inter() -> Option<Font> {
        Font::parse(DATA)
    }

    fn parse(data: &'static [u8]) -> Option<Font> {
        const TAGS: [&[u8; 4]; 6] = [b"head", b"hhea", b"hmtx", b"loca", b"glyf", b"cmap"];
        let num_tables = be16(data, 4)?;
        // Each table's offset and length.
        let mut tables = [None; 6];
        for i in 0..usize::from(num_tables) {
            let record = 12 + 16 * i;
            let tag = data.get(record..record + 4)?;
            if let Some(t) = TAGS.iter().position(|&want| want == tag) {
                let offset = usize::try_from(be32(data, record + 8)?).ok()?;
                let length = usize::try_from(be32(data, record + 12)?).ok()?;
                tables[t] = Some((offset, length));
            }
        }
        let [head, hhea, hmtx, loca, glyf, cmap] = tables;
        let (head, hhea, (loca, loca_len)) = (head?.0, hhea?.0, loca?);
        // loca must be long: the subset writes it so.
        if be16(data, head + 50)? != 1 {
            return None;
        }
        let font = Font {
            data,
            units_per_em: be16(data, head + 18)?,
            // A long loca has an offset per glyph and one past the last.
            num_glyphs: u16::try_from((loca_len / 4).saturating_sub(1)).ok()?,
            num_h_metrics: be16(data, hhea + 34)?,
            hmtx: hmtx?.0,
            loca,
            glyf: glyf?.0,
            cmap: format4(data, cmap?.0)?,
        };
        Some(font).filter(|f| f.units_per_em > 0)
    }

    /// The units an em has.
    pub(crate) fn units_per_em(&self) -> u16 {
        self.units_per_em
    }

    /// The glyph of `c`; 0 (the `.notdef` box) where the font has none.
    pub(crate) fn glyph(&self, c: char) -> u16 {
        let Ok(code) = u16::try_from(u32::from(c)) else {
            return 0;
        };
        self.lookup(code).unwrap_or(0)
    }

    fn lookup(&self, code: u16) -> Option<u16> {
        let (data, at) = (self.data, self.cmap);
        let segments = usize::from(be16(data, at + 6)? / 2);
        let ends = at + 14;
        let starts = ends + 2 * segments + 2;
        let deltas = starts + 2 * segments;
        let ranges = deltas + 2 * segments;
        for i in 0..segments {
            if code > be16(data, ends + 2 * i)? {
                continue;
            }
            let start = be16(data, starts + 2 * i)?;
            if code < start {
                return None;
            }
            let delta = be16(data, deltas + 2 * i)?;
            let range = be16(data, ranges + 2 * i)?;
            if range == 0 {
                return Some(code.wrapping_add(delta));
            }
            let glyph = be16(
                data,
                ranges + 2 * i + usize::from(range) + 2 * usize::from(code - start),
            )?;
            return Some(if glyph == 0 {
                0
            } else {
                glyph.wrapping_add(delta)
            });
        }
        None
    }

    /// How far `glyph` moves the pen, in font units.
    pub(crate) fn advance(&self, glyph: u16) -> u16 {
        let metric = glyph.min(self.num_h_metrics.saturating_sub(1));
        be16(self.data, self.hmtx + 4 * usize::from(metric)).unwrap_or(0)
    }

    /// `glyph`'s outline: its contours, each a closed loop of points.
    pub(crate) fn contours(&self, glyph: u16) -> Vec<Vec<Point>> {
        self.outline(glyph).unwrap_or_default()
    }

    fn outline(&self, glyph: u16) -> Option<Vec<Vec<Point>>> {
        if glyph >= self.num_glyphs {
            return None;
        }
        let data = self.data;
        let entry = self.loca + 4 * usize::from(glyph);
        let start = self.glyf + usize::try_from(be32(data, entry)?).ok()?;
        let end = self.glyf + usize::try_from(be32(data, entry + 4)?).ok()?;
        if end <= start {
            return Some(Vec::new());
        }
        let glyph = data.get(start..end)?;
        let contours =
            usize::try_from(i16::from_be_bytes(glyph.get(0..2)?.try_into().ok()?)).ok()?;
        let mut ends = Vec::with_capacity(contours);
        for i in 0..contours {
            ends.push(usize::from(be16(glyph, 10 + 2 * i)?));
        }
        let points = ends.last().map_or(0, |&e| e + 1);
        let mut at = 10 + 2 * contours;
        at += 2 + usize::from(be16(glyph, at)?);
        let mut flags = Vec::with_capacity(points);
        while flags.len() < points {
            let flag = *glyph.get(at)?;
            at += 1;
            flags.push(flag);
            if flag & 8 != 0 {
                let repeat = *glyph.get(at)?;
                at += 1;
                flags.extend(std::iter::repeat_n(flag, usize::from(repeat)));
            }
        }
        flags.truncate(points);
        let xs = coordinates(glyph, &mut at, &flags, 2, 16)?;
        let ys = coordinates(glyph, &mut at, &flags, 4, 32)?;
        let mut out = Vec::with_capacity(contours);
        let mut first = 0;
        for end in ends {
            let contour = (first..=end.min(points.saturating_sub(1)))
                .map(|i| Point {
                    x: xs[i],
                    y: ys[i],
                    on: flags[i] & 1 != 0,
                })
                .collect();
            out.push(contour);
            first = end + 1;
        }
        Some(out)
    }
}

/// One axis's coordinates of a simple glyph, read from `at`: a short
/// delta where `short` is set (`same` its sign), else a long one unless
/// `same` says it repeats the last.
fn coordinates(
    glyph: &[u8],
    at: &mut usize,
    flags: &[u8],
    short: u8,
    same: u8,
) -> Option<Vec<i16>> {
    let mut out = Vec::with_capacity(flags.len());
    let mut value: i16 = 0;
    for &flag in flags {
        if flag & short != 0 {
            let delta = i16::from(*glyph.get(*at)?);
            *at += 1;
            value = if flag & same != 0 {
                value.wrapping_add(delta)
            } else {
                value.wrapping_sub(delta)
            };
        } else if flag & same == 0 {
            value = value.wrapping_add(i16::from_be_bytes(
                glyph.get(*at..*at + 2)?.try_into().ok()?,
            ));
            *at += 2;
        }
        out.push(value);
    }
    Some(out)
}

/// The format 4 subtable of a `cmap` at `at`: Windows Unicode BMP, else
/// Unicode BMP.
fn format4(data: &[u8], at: usize) -> Option<usize> {
    let count = usize::from(be16(data, at + 2)?);
    let mut found = None;
    for i in 0..count {
        let record = at + 4 + 8 * i;
        let (platform, encoding) = (be16(data, record)?, be16(data, record + 2)?);
        let sub = at + usize::try_from(be32(data, record + 4)?).ok()?;
        if be16(data, sub)? != 4 {
            continue;
        }
        match (platform, encoding) {
            (3, 1) => return Some(sub),
            (0, _) => found = found.or(Some(sub)),
            _ => {}
        }
    }
    found
}

fn be16(data: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(data.get(at..at + 2)?.try_into().ok()?))
}

fn be32(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(data.get(at..at + 4)?.try_into().ok()?))
}

#[cfg(test)]
mod tests {
    use super::Font;

    #[test]
    fn p2_map_02_the_bundled_font_has_its_glyphs() {
        let font = Font::inter().unwrap();
        assert_eq!(font.units_per_em(), 2048);
        for c in ['A', 'z', '0', '…', 'é', '€', '·'] {
            let glyph = font.glyph(c);
            assert!(glyph > 0, "{c:?}");
            assert!(font.advance(glyph) > 0, "{c:?}");
            assert!(!font.contours(glyph).is_empty(), "{c:?}");
        }
        // é was a composite: flattened, the accent is a contour of its own.
        assert!(font.contours(font.glyph('é')).len() > font.contours(font.glyph('e')).len());
        // A space has an advance and no outline; what the font lacks is the
        // .notdef box.
        assert!(font.contours(font.glyph(' ')).is_empty());
        assert_eq!(font.glyph('写'), 0);
        assert!(!font.contours(0).is_empty());
    }
}
