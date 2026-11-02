//! The design system's components, drawn into a [`CellBuffer`] in the
//! pack's roles: the badge, the view tabs, the status, the breadcrumbs, a
//! pane (a rounded frame with its title on the top border), key–value rows,
//! list rows with bars, the key line, a message and the prompt.
//!
//! Text is measured and cut in terminal columns as the map measures names,
//! control characters shown as escapes. Without UTF-8 every glyph has an
//! ASCII stand-in: `+-|` frames, `#` and `-` bars, `>` for `›` and `▸`,
//! `~` for `…`.

use duscope_core::rules::Class;
use duscope_treemap::cells::{CellBuffer, Color};
use duscope_treemap::columns;
use duscope_treemap::packs::Pack;
use duscope_treemap::paint::{Glyphs, truncate};
use ratatui_core::layout::Rect;

/// A run of text in one colour.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    /// The text.
    pub text: String,
    /// Its colour.
    pub fg: Color,
    /// Bold.
    pub bold: bool,
}

impl Span {
    /// `text` in `fg`.
    pub fn new(text: impl Into<String>, fg: Color) -> Span {
        Span {
            text: text.into(),
            fg,
            bold: false,
        }
    }

    /// `text` in `fg`, bold.
    pub fn bold(text: impl Into<String>, fg: Color) -> Span {
        Span {
            text: text.into(),
            fg,
            bold: true,
        }
    }
}

/// The screen being drawn: its cells, the pack and the glyphs.
#[derive(Debug)]
pub struct Canvas<'p> {
    /// The cells.
    pub cells: CellBuffer,
    /// The colours.
    pub pack: &'p Pack,
    /// The characters.
    pub glyphs: Glyphs,
}

impl<'p> Canvas<'p> {
    /// A `w`×`h` screen in `pack`'s base colour.
    #[must_use]
    pub fn new(w: u16, h: u16, pack: &'p Pack, glyphs: Glyphs) -> Canvas<'p> {
        let mut canvas = Canvas {
            cells: CellBuffer::new(w, h),
            pack,
            glyphs,
        };
        canvas.fill(Rect::new(0, 0, w, h), pack.roles.base);
        canvas
    }

    fn ascii(&self) -> bool {
        self.glyphs == Glyphs::Ascii
    }

    /// `unicode`, or `ascii` without UTF-8.
    fn glyph<'a>(&self, unicode: &'a str, ascii: &'a str) -> &'a str {
        if self.ascii() { ascii } else { unicode }
    }

    /// The columns `text` takes.
    #[must_use]
    pub fn width(&self, text: &str) -> u16 {
        u16::try_from(columns(text, self.ascii())).unwrap_or(u16::MAX)
    }

    /// `text` cut to `room` columns, ending in `…` when cut.
    #[must_use]
    pub fn cut(&self, text: &str, room: u16) -> String {
        truncate(text, usize::from(room), self.glyphs)
    }

    /// Paints the cells of `area` blank on `bg`.
    pub fn fill(&mut self, area: Rect, bg: Color) {
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                if let Some(cell) = self.cells.get_mut(x, y) {
                    *cell = duscope_treemap::cells::Cell {
                        bg,
                        ..duscope_treemap::cells::Cell::default()
                    };
                }
            }
        }
    }

    /// Writes `spans` from column `x` on row `y`, up to column `end`, on
    /// the cells' background; returns the column after the last written.
    /// Without UTF-8 the design's glyphs take their ASCII stand-ins, and any
    /// other character outside ASCII shows as `?`, as in the map.
    pub fn spans(&mut self, x: u16, y: u16, end: u16, spans: &[Span]) -> u16 {
        let ascii = self.ascii();
        let mut at = x;
        for span in spans {
            if at >= end {
                break;
            }
            let room = end - at;
            let text = if ascii {
                plain(&span.text)
            } else {
                span.text.clone()
            };
            let text = if self.width(&text) > room {
                self.cut(&text, room)
            } else {
                text
            };
            self.cells
                .text(at, y, &text, room, span.fg, span.bold, ascii);
            at = at.saturating_add(self.width(&text)).min(end);
        }
        at
    }

    /// Writes `spans` on `bg` from column `x` on row `y`, up to `end`.
    fn spans_on(&mut self, x: u16, y: u16, end: u16, bg: Color, spans: &[Span]) -> u16 {
        let width: u16 = spans
            .iter()
            .map(|s| self.width(&s.text))
            .fold(0, u16::saturating_add);
        let w = width.min(end.saturating_sub(x));
        self.fill(Rect::new(x, y, w, 1), bg);
        self.spans(x, y, end, spans)
    }

    /// The badge: ` duscope ` on the accent colour, bold in crust.
    pub fn badge(&mut self, x: u16, y: u16, end: u16) -> u16 {
        let roles = self.pack.roles;
        self.spans_on(
            x,
            y,
            end,
            roles.accent,
            &[Span::bold(" duscope ", roles.crust)],
        )
    }

    /// The view tabs, `1 treemap` to `4 types`: the `active` one (0–3) on
    /// surface1, bold in the text colour, the others in overlay1.
    pub fn tabs(&mut self, x: u16, y: u16, end: u16, active: usize) -> u16 {
        const TABS: [&str; 4] = ["1 treemap", "2 list", "3 largest", "4 types"];
        let roles = self.pack.roles;
        let mut at = x;
        for (i, tab) in TABS.iter().enumerate() {
            let text = format!(" {tab} ");
            at = if i == active {
                self.spans_on(at, y, end, roles.surface1, &[Span::bold(text, roles.text)])
            } else {
                self.spans(at, y, end, &[Span::new(text, roles.overlay1)])
            };
        }
        at
    }

    /// The status, ` text `, in subtext, ending at column `end`, from no
    /// further left than `start`; cut to fit.
    pub fn status(&mut self, start: u16, y: u16, end: u16, text: &str) {
        let room = end.saturating_sub(start);
        if room < 6 {
            return;
        }
        let text = self.cut(text, room - 2);
        let w = self.width(&text) + 2;
        let subtext = self.pack.roles.subtext;
        self.spans(end - w, y, end, &[Span::new(format!(" {text} "), subtext)]);
    }

    /// The breadcrumbs: `segments` from column `x` on row `y`, each after
    /// ` › ` (the first after a space) in overlay1, in subtext but the last,
    /// which is bold in the accent colour. When they do not fit, the first
    /// segment, `…`, and as many of the last as fit, the very last cut.
    pub fn crumbs(&mut self, x: u16, y: u16, end: u16, segments: &[String]) {
        let roles = self.pack.roles;
        let sep = self.glyph(" › ", " > ").to_owned();
        let more = self.glyph("…", "~").to_owned();
        let width = |canvas: &Self, segs: &[&str]| -> u16 {
            segs.iter()
                .map(|s| canvas.width(s) + 3)
                .fold(0u16, u16::saturating_add)
                .saturating_sub(2)
        };
        let all: Vec<&str> = segments.iter().map(String::as_str).collect();
        let room = end.saturating_sub(x);
        let shown: Vec<&str> = if all.len() < 3 || width(self, &all) <= room {
            all
        } else {
            // The first, `…`, then the last, and the ones before it while
            // they fit.
            let head = [all[0], more.as_str()];
            let mut start = all.len() - 1;
            while start > 2 && width(self, &[&head[..], &all[start - 1..]].concat()) <= room {
                start -= 1;
            }
            [&head[..], &all[start..]].concat()
        };
        let mut spans = Vec::new();
        let last = shown.len().saturating_sub(1);
        for (i, segment) in shown.iter().enumerate() {
            spans.push(Span::new(
                if i == 0 { " ".to_owned() } else { sep.clone() },
                roles.overlay1,
            ));
            spans.push(if i == last {
                Span::bold(*segment, roles.accent)
            } else {
                Span::new(*segment, roles.subtext)
            });
        }
        self.spans(x, y, end, &spans);
    }

    /// A pane: a rounded frame around `area` in `border`, its `title` on
    /// the top border two columns in, with a cell of padding on the base
    /// colour, bold in the title colour.
    pub fn pane(&mut self, area: Rect, title: &str, border: Color) {
        if area.width < 2 || area.height < 2 {
            return;
        }
        let ascii = self.ascii();
        let (h, v, corners) = if ascii {
            ('-', '|', ['+', '+', '+', '+'])
        } else {
            ('─', '│', ['╭', '╮', '╰', '╯'])
        };
        let base = self.pack.roles.base;
        let put = |canvas: &mut Self, x: u16, y: u16, ch: char| {
            if let Some(cell) = canvas.cells.get_mut(x, y) {
                *cell = duscope_treemap::cells::Cell {
                    ch,
                    fg: border,
                    bg: base,
                    ..duscope_treemap::cells::Cell::default()
                };
            }
        };
        let (right, bottom) = (area.right() - 1, area.bottom() - 1);
        for x in area.left() + 1..right {
            put(self, x, area.top(), h);
            put(self, x, bottom, h);
        }
        for y in area.top() + 1..bottom {
            put(self, area.left(), y, v);
            put(self, right, y, v);
        }
        put(self, area.left(), area.top(), corners[0]);
        put(self, right, area.top(), corners[1]);
        put(self, area.left(), bottom, corners[2]);
        put(self, right, bottom, corners[3]);
        let room = area.width.saturating_sub(4);
        if room > 2 && !title.is_empty() {
            let text = self.cut(title, room - 2);
            let title_color = self.pack.roles.title;
            self.spans_on(
                area.left() + 2,
                area.top(),
                right,
                base,
                &[Span::bold(format!(" {text} "), title_color)],
            );
        }
    }

    /// A key–value row: `label` in a 10-column field in overlay1, then
    /// `value`.
    pub fn key_value(&mut self, x: u16, y: u16, end: u16, label: &str, value: &[Span]) -> u16 {
        let overlay1 = self.pack.roles.overlay1;
        let field = format!("{label:<10}");
        self.spans(x, y, end, &[Span::new(field, overlay1)]);
        self.spans(x.saturating_add(10), y, end, value)
    }

    /// A bar of `cells` cells: `█` in `color` for `share` (0–1) of them,
    /// rounded to whole cells, on a `░` track in surface2.
    #[must_use]
    pub fn bar(&self, share: f64, cells: u16, color: Color) -> Vec<Span> {
        let filled = (share.clamp(0.0, 1.0) * f64::from(cells)).round();
        // In 0..=cells, so the cast is exact.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let filled = filled as u16;
        let (block, track) = if self.ascii() {
            ("#", "-")
        } else {
            ("█", "░")
        };
        vec![
            Span::new(block.repeat(usize::from(filled)), color),
            Span::new(
                track.repeat(usize::from(cells - filled)),
                self.pack.roles.surface2,
            ),
        ]
    }

    /// A list row: the marker column (`▸` in the accent colour on the
    /// current row) where the list has one, a square in `color`, `name`
    /// padded to `name_width`, `size` padded to 6, a bar of `bar` cells for
    /// `share`, and the share as a whole percentage, right-aligned.
    pub fn list_row(
        &mut self,
        x: u16,
        y: u16,
        end: u16,
        row: &ListRow<'_>,
        name_width: u16,
        bar: u16,
    ) {
        let roles = self.pack.roles;
        let marker = match row.current {
            Some(true) => self.glyph("▸ ", "> "),
            Some(false) => "  ",
            None => "",
        };
        let square = self.glyph("■ ", "# ");
        let name = self.cut(row.name, name_width);
        let pad = usize::from(name_width.saturating_sub(self.width(&name)));
        let name = format!("{name}{}", " ".repeat(pad));
        let mut spans = vec![
            Span::new(marker, roles.accent),
            Span::new(square, row.color),
            if row.current == Some(true) {
                Span::bold(name, roles.text)
            } else {
                Span::new(name, row.name_color.unwrap_or(roles.subtext))
            },
            Span::new(format!(" {:>6} ", row.size), roles.text),
        ];
        spans.extend(self.bar(row.share, bar, row.color));
        let pct = format!("{}%", percent(row.share));
        spans.push(Span::new(format!(" {pct:>4}"), roles.overlay1));
        self.spans(x, y, end, &spans);
    }

    /// The key line from column `x` on row `y`: each key bold in the accent
    /// colour and its description in overlay1, three cells apart; then
    /// `message`, right-aligned, where there is room.
    pub fn key_line(
        &mut self,
        x: u16,
        y: u16,
        end: u16,
        keys: &[(&str, &str)],
        message: Option<&Message>,
    ) {
        let roles = self.pack.roles;
        let mut at = x;
        for (i, (key, what)) in keys.iter().enumerate() {
            if i > 0 {
                at = at.saturating_add(3);
            }
            at = self.spans(
                at,
                y,
                end,
                &[
                    Span::bold(*key, roles.accent),
                    Span::new(format!(" {what}"), roles.overlay1),
                ],
            );
        }
        if let Some(message) = message {
            let start = at.saturating_add(3);
            let room = end.saturating_sub(start);
            if room > 4 {
                let text = self.cut(&message.text, room);
                let w = self.width(&text);
                self.message(
                    end - w,
                    y,
                    end,
                    &Message {
                        text,
                        ..message.clone()
                    },
                );
            }
        }
    }

    /// A message: a warning in the pack's red, a success in its green.
    pub fn message(&mut self, x: u16, y: u16, end: u16, message: &Message) {
        let color = match message.kind {
            Notice::Warning => self.pack.roles.red,
            Notice::Success => self.pack.roles.green,
        };
        self.spans(x, y, end, &[Span::new(message.text.clone(), color)]);
    }

    /// The prompt: ` >  ` in overlay1, `input` in the text colour, then
    /// the `▌` cursor in the accent colour.
    pub fn prompt(&mut self, x: u16, y: u16, end: u16, input: &str) {
        let roles = self.pack.roles;
        let cursor = self.glyph("▌", "_");
        self.spans(
            x,
            y,
            end,
            &[
                Span::new(" >  ", roles.overlay1),
                Span::new(input, roles.text),
                Span::new(cursor, roles.accent),
            ],
        );
    }
}

/// One row of the siblings or legend list.
#[derive(Debug, Clone)]
pub struct ListRow<'a> {
    /// Whether the row is the cursor's, in a list with a marker column;
    /// `None` without one (the legend).
    pub current: Option<bool>,
    /// The square's and bar's colour.
    pub color: Color,
    /// The name or label.
    pub name: &'a str,
    /// The name's colour, when not subtext.
    pub name_color: Option<Color>,
    /// The size, compact.
    pub size: &'a str,
    /// Its share, 0–1.
    pub share: f64,
}

/// What a message tells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Notice {
    /// Something went wrong or was left out.
    Warning,
    /// Something was done.
    Success,
}

/// A message on the key line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// The text.
    pub text: String,
    /// Its kind.
    pub kind: Notice,
}

/// `text` with the design's glyphs in their ASCII stand-ins.
fn plain(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '·' | '—' | '─' | '░' => '-',
            '…' => '~',
            '›' | '▸' => '>',
            '■' | '█' => '#',
            '●' => '*',
            '×' => 'x',
            '✓' => 'v',
            '▌' => '_',
            other => other,
        })
        .collect()
}

/// `share` (0–1) as a whole percentage.
#[must_use]
pub fn percent(share: f64) -> u64 {
    // Clamped to 0–100 first, so the cast is exact.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let pct = (share.clamp(0.0, 1.0) * 100.0).round() as u64;
    pct
}

/// The colour a pane border takes in `pack`: the map's pane is drawn in
/// the title colour, as the canvas draws it, unless that is one of the
/// pack's class colours (Dracula's title purple is its code colour,
/// Gruvbox's title pink its media colour), where a border would run into
/// the tiles beside it: there, and for every other pane, surface2.
#[must_use]
pub fn border(pack: &Pack, map: bool) -> Color {
    let title = pack.roles.title;
    if map && !Class::ALL.iter().any(|&class| pack.class(class) == title) {
        title
    } else {
        pack.roles.surface2
    }
}

/// `n` with thousands separators: `1,284,310`.
#[must_use]
pub fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A size in full, as the selected pane prints it: `1.84 GiB`, `932 MiB`,
/// `12 KiB`, `512 B`; two decimals from GiB up.
#[must_use]
pub fn full_size(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut unit = 0;
    let mut divisor = 1u64;
    while bytes / divisor >= 1024 && unit + 1 < UNITS.len() {
        divisor *= 1024;
        unit += 1;
    }
    if unit >= 3 {
        let hundredths = (u128::from(bytes) * 100 + u128::from(divisor) / 2) / u128::from(divisor);
        format!(
            "{}.{:02} {}",
            hundredths / 100,
            hundredths % 100,
            UNITS[unit]
        )
    } else {
        let whole = (u128::from(bytes) + u128::from(divisor) / 2) / u128::from(divisor);
        format!("{whole} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use duscope_core::rules::Class;
    use duscope_treemap::cells::Color;
    use duscope_treemap::packs::{NAMES, Pack};
    use duscope_treemap::paint::Glyphs;
    use ratatui_core::layout::Rect;

    use super::{Canvas, ListRow, Message, Notice, Span, border, full_size, percent, thousands};

    /// Row `y` of `canvas` as text, trimmed at the end.
    fn row(canvas: &Canvas<'_>, y: u16) -> String {
        (0..canvas.cells.width())
            .filter_map(|x| canvas.cells.get(x, y).map(|c| c.ch))
            .filter(|&c| c != '\0')
            .collect::<String>()
            .trim_end()
            .to_owned()
    }

    #[test]
    fn p4_tui_02_a_pane_is_a_rounded_frame_titled_on_its_border() {
        let mocha = Pack::named("mocha").unwrap();
        let mut canvas = Canvas::new(20, 4, &mocha, Glyphs::Unicode);
        canvas.pane(Rect::new(0, 0, 20, 4), "selected", mocha.roles.surface2);
        assert_eq!(row(&canvas, 0), "╭─ selected ───────╮");
        assert_eq!(row(&canvas, 1), "│                  │");
        assert_eq!(row(&canvas, 3), "╰──────────────────╯");
        // The title bold in the title colour on the base, the frame in the
        // border colour.
        let title = canvas.cells.get(3, 0).unwrap();
        assert_eq!(
            (title.fg, title.bg, title.bold),
            (mocha.roles.title, mocha.roles.base, true)
        );
        assert_eq!(canvas.cells.get(0, 0).unwrap().fg, mocha.roles.surface2);
        // A title longer than the frame is cut; without UTF-8, ASCII corners.
        let mut canvas = Canvas::new(12, 3, &mocha, Glyphs::Ascii);
        canvas.pane(
            Rect::new(0, 0, 12, 3),
            "in com.apple.Safari",
            mocha.roles.surface2,
        );
        assert_eq!(row(&canvas, 0), "+- in co~ -+");
        assert_eq!(row(&canvas, 2), "+----------+");
    }

    #[test]
    fn p4_tui_02_bars_round_to_whole_cells_on_a_track() {
        let mocha = Pack::named("mocha").unwrap();
        let canvas = Canvas::new(1, 1, &mocha, Glyphs::Unicode);
        let text = |share: f64, cells: u16| -> String {
            canvas
                .bar(share, cells, mocha.roles.accent)
                .iter()
                .map(|s| s.text.as_str())
                .collect()
        };
        assert_eq!(text(0.517, 16), "████████░░░░░░░░");
        assert_eq!(text(0.038, 16), "█░░░░░░░░░░░░░░░");
        assert_eq!(text(0.03, 6), "░░░░░░");
        assert_eq!(text(1.7, 6), "██████");
        assert_eq!(text(-0.2, 8), "░░░░░░░░");
        let spans = canvas.bar(0.5, 4, mocha.roles.accent);
        assert_eq!(
            (spans[0].fg, spans[1].fg),
            (mocha.roles.accent, mocha.roles.surface2)
        );
        assert_eq!((percent(0.517), percent(0.005), percent(2.0)), (52, 1, 100));
    }

    #[test]
    fn p4_tui_02_the_header_and_key_line_components() {
        let mocha = Pack::named("mocha").unwrap();
        let roles = mocha.roles;
        let mut canvas = Canvas::new(80, 3, &mocha, Glyphs::Unicode);
        let x = canvas.badge(0, 0, 80);
        let x = canvas.tabs(x, 0, 80, 1);
        canvas.status(
            x + 1,
            0,
            80,
            "175 MiB · 5 files · 6 dirs · scanned in 6.8s ✓",
        );
        assert_eq!(
            row(&canvas, 0),
            " duscope  1 treemap  2 list  3 largest  4 types   175 MiB · 5 files · 6 dirs ·…"
        );
        // The badge on the accent colour, bold in crust; the active tab on
        // surface1, the others in overlay1; the status in subtext.
        let cell = |x| *canvas.cells.get(x, 0).unwrap();
        assert_eq!(
            (cell(1).fg, cell(1).bg, cell(1).bold),
            (roles.crust, roles.accent, true)
        );
        assert_eq!((cell(10).fg, cell(10).bg), (roles.overlay1, roles.base));
        assert_eq!(
            (cell(21).fg, cell(21).bg, cell(21).bold),
            (roles.text, roles.surface1, true)
        );
        assert_eq!(cell(60).fg, roles.subtext);
        // The key line: keys bold in the accent colour three cells apart, a
        // warning right-aligned in red where there is room.
        let message = Message {
            text: "warning: 2 unreadable".to_owned(),
            kind: Notice::Warning,
        };
        canvas.key_line(1, 1, 80, &[("q", "quit"), ("?", "help")], Some(&message));
        assert_eq!(
            row(&canvas, 1),
            format!(" q quit   ? help{}warning: 2 unreadable", " ".repeat(43))
        );
        assert_eq!(canvas.cells.get(1, 1).unwrap().fg, roles.accent);
        assert_eq!(canvas.cells.get(79, 1).unwrap().fg, roles.red);
        // The prompt and a success message.
        canvas.prompt(0, 2, 40, "*.dmg");
        canvas.message(
            30,
            2,
            80,
            &Message {
                text: "done".to_owned(),
                kind: Notice::Success,
            },
        );
        assert_eq!(row(&canvas, 2), " >  *.dmg▌                    done");
        assert_eq!(canvas.cells.get(9, 2).unwrap().fg, roles.accent);
        assert_eq!(canvas.cells.get(30, 2).unwrap().fg, roles.green);
    }

    #[test]
    fn p4_tui_02_breadcrumbs_keep_the_first_and_the_last_when_long() {
        let mocha = Pack::named("mocha").unwrap();
        let segments: Vec<String> = ["~", "Library", "Caches", "com.apple.Safari", "Cache.db-wal"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        let mut canvas = Canvas::new(60, 2, &mocha, Glyphs::Unicode);
        canvas.crumbs(0, 0, 60, &segments);
        assert_eq!(
            row(&canvas, 0),
            " ~ › Library › Caches › com.apple.Safari › Cache.db-wal"
        );
        // The last in the accent colour, bold; the others in subtext.
        assert_eq!(canvas.cells.get(43, 0).unwrap().fg, mocha.roles.accent);
        assert_eq!(canvas.cells.get(5, 0).unwrap().fg, mocha.roles.subtext);
        canvas.crumbs(0, 1, 40, &segments);
        assert_eq!(row(&canvas, 1), " ~ › … › com.apple.Safari › Cache.db-wal");
    }

    #[test]
    fn p4_tui_02_list_rows_and_key_value_rows() {
        let mocha = Pack::named("mocha").unwrap();
        let code = mocha.class(Class::Code);
        let mut canvas = Canvas::new(43, 3, &mocha, Glyphs::Unicode);
        let row_of = |current, name| ListRow {
            current,
            color: code,
            name,
            name_color: None,
            size: "1.8G",
            share: 0.517,
        };
        canvas.list_row(0, 0, 43, &row_of(Some(true), "Cache.db-wal"), 16, 6);
        canvas.list_row(0, 1, 43, &row_of(None, "a-very-long-name"), 10, 8);
        canvas.key_value(0, 2, 43, "size", &[Span::new("1.84 GiB", mocha.roles.text)]);
        assert_eq!(row(&canvas, 0), "▸ ■ Cache.db-wal       1.8G ███░░░  52%");
        assert_eq!(row(&canvas, 1), "■ a-very-lo…   1.8G ████░░░░  52%");
        assert_eq!(row(&canvas, 2), "size      1.84 GiB");
        // The current row's name bold in the text colour, a square in the
        // tile's colour, the label of a key-value row in overlay1.
        let cell = |x, y| *canvas.cells.get(x, y).unwrap();
        assert_eq!((cell(4, 0).fg, cell(4, 0).bold), (mocha.roles.text, true));
        assert_eq!(cell(2, 0).fg, code);
        assert_eq!(cell(0, 2).fg, mocha.roles.overlay1);
    }

    #[test]
    fn p4_tui_02_without_utf_8_every_glyph_has_an_ascii_stand_in() {
        let mocha = Pack::named("mocha").unwrap();
        let mut canvas = Canvas::new(60, 2, &mocha, Glyphs::Ascii);
        canvas.list_row(
            0,
            0,
            60,
            &ListRow {
                current: Some(true),
                color: mocha.roles.accent,
                name: "café · x",
                name_color: None,
                size: "1.8G",
                share: 0.5,
            },
            16,
            6,
        );
        canvas.crumbs(0, 1, 60, &["~".to_owned(), "a".to_owned(), "b".to_owned()]);
        let text = format!("{}\n{}", row(&canvas, 0), row(&canvas, 1));
        assert!(text.is_ascii(), "{text}");
        assert_eq!(row(&canvas, 0), "> # caf? - x           1.8G ###---  50%");
        assert_eq!(row(&canvas, 1), " ~ > a > b");
    }

    #[test]
    fn p4_tui_02_no_border_takes_a_class_colour() {
        // DEV-43's open question: Dracula's title colour is its code colour
        // and Gruvbox's its media colour, so the map's pane, drawn in the
        // title colour by the canvas, takes surface2 there; no border the
        // viewer draws equals a class colour in any pack.
        for name in NAMES {
            let pack = Pack::named(name).unwrap();
            for map in [true, false] {
                let color = border(&pack, map);
                if color != Color::Reset {
                    assert!(
                        !Class::ALL.iter().any(|&c| pack.class(c) == color),
                        "{name}: {color:?}"
                    );
                }
            }
        }
        let mocha = Pack::named("mocha").unwrap();
        assert_eq!(border(&mocha, true), mocha.roles.title);
        let dracula = Pack::named("dracula").unwrap();
        assert_eq!(border(&dracula, true), dracula.roles.surface2);
        let gruvbox = Pack::named("gruvbox").unwrap();
        assert_eq!(border(&gruvbox, true), gruvbox.roles.surface2);
    }

    #[test]
    fn p4_tui_02_sizes_in_full_and_counts_with_separators() {
        assert_eq!(full_size(1884 << 20), "1.84 GiB");
        assert_eq!(full_size(932 << 20), "932 MiB");
        assert_eq!(full_size(12 << 10), "12 KiB");
        assert_eq!(full_size(512), "512 B");
        assert_eq!(full_size(3 << 40), "3.00 TiB");
        assert_eq!(thousands(1_284_310), "1,284,310");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(0), "0");
    }
}
