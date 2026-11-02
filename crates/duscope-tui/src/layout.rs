//! The screen's geometry, from the canvas's 160×45 artboard: the header on
//! row 0, the breadcrumbs on row 1, the panes from row 2 down to the row
//! above the key line, the key line on the last row.
//!
//! At 140 columns and more the right column is 44 columns of text, at 110
//! and more 34: its panes are one column wider than that, border to
//! border, and stand one column right of the map pane, which takes the
//! rest. So at 160×45 the map pane spans columns 0–113 and the right
//! column 115–159, and the map inside the pane is 112×40: `columns − right
//! column − 4` by `rows − 5`. The right column stacks the selected pane (12
//! rows, border to border), the siblings pane (what is left) and the legend
//! pane (10 rows). Below 110 columns the right column folds into one
//! selected pane of 6 rows under the map, which keeps the full width.

use ratatui_core::layout::Rect;

/// The narrowest terminal with the right column at its full width.
pub const WIDE: u16 = 140;
/// The narrowest terminal with a right column at all.
pub const NARROW: u16 = 110;
/// The right column's text width at [`WIDE`] and over.
pub const RIGHT_WIDE: u16 = 44;
/// The right column's text width from [`NARROW`] to [`WIDE`].
pub const RIGHT_NARROW: u16 = 34;
/// The selected pane's rows, border to border.
const SELECTED: u16 = 12;
/// The legend pane's rows, border to border.
const LEGEND: u16 = 10;
/// The fewest rows the panes of the right column shrink to.
const MIN_PANE: u16 = 5;
/// The folded selected pane's rows, border to border.
const FOLDED: u16 = 6;

/// Where everything goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    /// Row 0: the badge, the scan's path, the view tabs, the status.
    pub header: Rect,
    /// Row 1: the path to the selection.
    pub crumbs: Rect,
    /// The map's pane, border to border.
    pub map_pane: Rect,
    /// The map, inside its pane's border.
    pub map: Rect,
    /// The panes beside the map, or the one under it.
    pub panes: Panes,
    /// The last row: the keys, and a message.
    pub keys: Rect,
}

/// The selected, siblings and legend panes, border to border.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panes {
    /// The right column, at [`NARROW`] columns and over.
    Column {
        /// The selection's facts.
        selected: Rect,
        /// The selection's parent's children.
        siblings: Rect,
        /// The colours' shares.
        legend: Rect,
    },
    /// Narrower: the selection's facts under the map.
    Folded {
        /// The selection's facts.
        selected: Rect,
    },
}

/// The layout of a `w`×`h` screen, which the viewer draws from 80×24.
#[must_use]
pub fn layout(w: u16, h: u16) -> Layout {
    let header = Rect::new(0, 0, w, 1.min(h));
    let crumbs = Rect::new(0, 1.min(h), w, 1.min(h.saturating_sub(1)));
    let keys = Rect::new(0, h.saturating_sub(1), w, 1.min(h));
    // The rows between the breadcrumbs and the key line.
    let body = h.saturating_sub(3);
    let right = if w >= WIDE {
        Some(RIGHT_WIDE)
    } else if w >= NARROW {
        Some(RIGHT_NARROW)
    } else {
        None
    };
    let (map_pane, panes) = if let Some(right) = right {
        let map_pane = Rect::new(0, 2, w.saturating_sub(right + 2), body);
        let x = w.saturating_sub(right + 1);
        let width = right + 1;
        let (selected, legend) = column_rows(body);
        let siblings = body.saturating_sub(selected + legend);
        (
            map_pane,
            Panes::Column {
                selected: Rect::new(x, 2, width, selected),
                siblings: Rect::new(x, 2 + selected, width, siblings),
                legend: Rect::new(x, 2 + selected + siblings, width, legend),
            },
        )
    } else {
        let folded = FOLDED.min(body);
        let map_pane = Rect::new(0, 2, w, body - folded);
        (
            map_pane,
            Panes::Folded {
                selected: Rect::new(0, 2 + map_pane.height, w, folded),
            },
        )
    };
    Layout {
        header,
        crumbs,
        map_pane,
        map: inner(map_pane),
        panes,
        keys,
    }
}

/// The selected and legend panes' rows in a right column `body` rows tall:
/// 12 and 10, giving up rows, the legend first, so the siblings pane keeps
/// [`MIN_PANE`] rows on a short terminal.
fn column_rows(body: u16) -> (u16, u16) {
    let (mut selected, mut legend) = (SELECTED, LEGEND);
    let mut short = (MIN_PANE + selected + legend).saturating_sub(body);
    let from_legend = short.min(legend - MIN_PANE);
    legend -= from_legend;
    short -= from_legend;
    selected -= short.min(selected - MIN_PANE);
    (selected, legend)
}

/// The inside of a pane: one cell in from its border.
#[must_use]
pub fn inner(pane: Rect) -> Rect {
    Rect::new(
        pane.x + 1,
        pane.y + 1,
        pane.width.saturating_sub(2),
        pane.height.saturating_sub(2),
    )
}

#[cfg(test)]
mod tests {
    use ratatui_core::layout::Rect;

    use super::{Panes, layout};

    #[test]
    fn p4_tui_02_the_layout_rules_at_the_four_sizes() {
        // 160×45, the canvas: a 44-column right column, the map 112×40.
        let at = layout(160, 45);
        assert_eq!(at.header, Rect::new(0, 0, 160, 1));
        assert_eq!(at.crumbs, Rect::new(0, 1, 160, 1));
        assert_eq!(at.map_pane, Rect::new(0, 2, 114, 42));
        assert_eq!(at.map, Rect::new(1, 3, 112, 40));
        assert_eq!(
            at.panes,
            Panes::Column {
                selected: Rect::new(115, 2, 45, 12),
                siblings: Rect::new(115, 14, 45, 20),
                legend: Rect::new(115, 34, 45, 10),
            }
        );
        assert_eq!(at.keys, Rect::new(0, 44, 160, 1));
        // 120×40: 34 columns from 110 to 139; the map is the rest.
        let at = layout(120, 40);
        assert_eq!(at.map, Rect::new(1, 3, 82, 35));
        assert_eq!(
            at.panes,
            Panes::Column {
                selected: Rect::new(85, 2, 35, 12),
                siblings: Rect::new(85, 14, 35, 15),
                legend: Rect::new(85, 29, 35, 10),
            }
        );
        // Below 110 the right column folds into a selected pane of six rows
        // under the map, which keeps the full width.
        for (w, h) in [(100, 30), (80, 24)] {
            let at = layout(w, h);
            assert_eq!(at.map_pane, Rect::new(0, 2, w, h - 9), "{w}×{h}");
            assert_eq!(at.map, Rect::new(1, 3, w - 2, h - 11), "{w}×{h}");
            assert_eq!(
                at.panes,
                Panes::Folded {
                    selected: Rect::new(0, h - 7, w, 6)
                },
                "{w}×{h}"
            );
            assert_eq!(at.keys, Rect::new(0, h - 1, w, 1), "{w}×{h}");
        }
        // At the edges: 140 and 110 columns take the wider column.
        assert_eq!(layout(140, 45).map.width, 92);
        assert_eq!(layout(139, 45).map.width, 101);
        assert_eq!(layout(110, 45).map.width, 72);
        assert!(matches!(layout(109, 45).panes, Panes::Folded { .. }));
    }

    #[test]
    fn p4_tui_02_a_short_terminal_shrinks_the_legend_then_the_selected_pane() {
        // 140×24: 21 rows for the column; the legend gives up five, the
        // selected pane one, so the siblings keep five.
        let Panes::Column {
            selected,
            siblings,
            legend,
        } = layout(140, 24).panes
        else {
            panic!("folded at 140 columns");
        };
        assert_eq!(
            (selected.height, siblings.height, legend.height),
            (11, 5, 5)
        );
        assert_eq!(legend.bottom(), 23);
    }
}
