//! The key map: every key the viewer answers, what it asks for and the
//! words `?` and the key line show for it, in one table ([`ALL`]). The
//! viewer looks a key up here ([`lookup`]) and `?` lists this table row by
//! row ([`crate::views::help`]), so the two cannot disagree.
//!
//! A [`Binding`] is one row of the help: the keys that do one thing (the
//! arrows and `hjkl` share a row). The key line shows six bindings, the
//! most useful for the pane in focus and the view ([`line`]).
//!
//! A key is matched by its code and whether Ctrl is held; Alt is never one
//! of the viewer's. Shift is part of the code a terminal sends: `Z`, `G`
//! and `BackTab` arrive as themselves.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::cursor::{Dir, Nav};
use crate::views::{Focus, Tab};
use crate::widgets::KeyItem;

/// What a key asks the viewer for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// `q`, `Ctrl-C`: give the terminal back.
    Quit,
    /// `Esc`: close what is open, clear the filter, then up the path.
    Back,
    /// A move of the cursor or the zoom.
    Nav(Nav),
    /// `1`–`4`: switch the view.
    View(Tab),
    /// `s`: the next sort of the siblings and list.
    Sort,
    /// `t`: labels on tiles at least 8×2 cells.
    Labels,
    /// `c`: the next colour mode.
    Color,
    /// `p`: the next theme pack.
    Pack,
    /// `/`: the filter prompt.
    Filter,
    /// `⇥`: focus the next pane.
    FocusNext,
    /// `Shift-⇥`: focus the previous pane.
    FocusPrev,
    /// `i`: the full fact list.
    Facts,
    /// `?`: this table.
    Help,
    /// `y`: copy the selection's path.
    Copy,
    /// `o`: show the selection in the file manager, selected in its folder.
    Open,
    /// `d`: mark or unmark the selection.
    Mark,
    /// `D`: the mark pane.
    MarkPane,
    /// `r`: rescan the view's root and reload.
    Rescan,
}

/// A key and what it asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stroke {
    /// The key.
    pub code: KeyCode,
    /// Whether Ctrl is held.
    pub ctrl: bool,
    /// What it asks for.
    pub action: Action,
}

const fn key(code: KeyCode, action: Action) -> Stroke {
    Stroke {
        code,
        ctrl: false,
        action,
    }
}

const fn ctrl(code: KeyCode, action: Action) -> Stroke {
    Stroke {
        code,
        ctrl: true,
        action,
    }
}

/// One row of the help: keys that do one thing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Binding {
    /// The keys, and what each asks for.
    pub strokes: &'static [Stroke],
    /// The keys as the help writes them.
    pub label: &'static str,
    /// Their stand-in without UTF-8.
    pub ascii: &'static str,
    /// What the help says they do.
    pub what: &'static str,
}

impl Binding {
    /// The keys as they are written where `ascii` is whether the terminal
    /// lacks UTF-8.
    #[must_use]
    pub fn keys(&self, ascii: bool) -> &'static str {
        if ascii { self.ascii } else { self.label }
    }
}

/// The spatial moves.
pub static MOVE: Binding = Binding {
    strokes: &[
        key(KeyCode::Left, Action::Nav(Nav::Move(Dir::Left))),
        key(KeyCode::Down, Action::Nav(Nav::Move(Dir::Down))),
        key(KeyCode::Up, Action::Nav(Nav::Move(Dir::Up))),
        key(KeyCode::Right, Action::Nav(Nav::Move(Dir::Right))),
        key(KeyCode::Char('h'), Action::Nav(Nav::Move(Dir::Left))),
        key(KeyCode::Char('j'), Action::Nav(Nav::Move(Dir::Down))),
        key(KeyCode::Char('k'), Action::Nav(Nav::Move(Dir::Up))),
        key(KeyCode::Char('l'), Action::Nav(Nav::Move(Dir::Right))),
    ],
    label: "←↓↑→ hjkl",
    ascii: "arrows hjkl",
    what: "move on the map or a list",
};
/// `⏎`.
pub static OPEN: Binding = Binding {
    strokes: &[key(KeyCode::Enter, Action::Nav(Nav::Open))],
    label: "⏎",
    ascii: "enter",
    what: "open, or filter by a row",
};
/// `⌫`.
pub static UP: Binding = Binding {
    strokes: &[key(KeyCode::Backspace, Action::Nav(Nav::Up))],
    label: "⌫",
    ascii: "bksp",
    what: "up the path",
};
/// `Esc`.
pub static BACK: Binding = Binding {
    strokes: &[key(KeyCode::Esc, Action::Back)],
    label: "Esc",
    ascii: "Esc",
    what: "close, clear filter, or up",
};
/// `z`.
pub static ZOOM: Binding = Binding {
    strokes: &[key(KeyCode::Char('z'), Action::Nav(Nav::Zoom))],
    label: "z",
    ascii: "z",
    what: "zoom to the cursor",
};
/// `Z`.
pub static OUT: Binding = Binding {
    strokes: &[key(KeyCode::Char('Z'), Action::Nav(Nav::Out))],
    label: "Z",
    ascii: "Z",
    what: "zoom out one step",
};
/// `0`.
pub static RESET: Binding = Binding {
    strokes: &[key(KeyCode::Char('0'), Action::Nav(Nav::Reset))],
    label: "0",
    ascii: "0",
    what: "zoom all the way out",
};
/// `g`, `Home`.
pub static FIRST: Binding = Binding {
    strokes: &[
        key(KeyCode::Char('g'), Action::Nav(Nav::First)),
        key(KeyCode::Home, Action::Nav(Nav::First)),
    ],
    label: "g Home",
    ascii: "g Home",
    what: "first sibling or row",
};
/// `G`, `End`.
pub static LAST: Binding = Binding {
    strokes: &[
        key(KeyCode::Char('G'), Action::Nav(Nav::Last)),
        key(KeyCode::End, Action::Nav(Nav::Last)),
    ],
    label: "G End",
    ascii: "G End",
    what: "last sibling or row",
};
/// `PgUp`, `PgDn`.
pub static PAGE: Binding = Binding {
    strokes: &[
        key(KeyCode::PageUp, Action::Nav(Nav::PageUp)),
        key(KeyCode::PageDown, Action::Nav(Nav::PageDown)),
    ],
    label: "PgUp PgDn",
    ascii: "PgUp PgDn",
    what: "page of siblings or rows",
};
/// `1`–`4`.
pub static VIEWS: Binding = Binding {
    strokes: &[
        key(KeyCode::Char('1'), Action::View(Tab::Treemap)),
        key(KeyCode::Char('2'), Action::View(Tab::List)),
        key(KeyCode::Char('3'), Action::View(Tab::Largest)),
        key(KeyCode::Char('4'), Action::View(Tab::Types)),
    ],
    label: "1 2 3 4",
    ascii: "1 2 3 4",
    what: "treemap list largest types",
};
/// `s`.
pub static SORT: Binding = Binding {
    strokes: &[key(KeyCode::Char('s'), Action::Sort)],
    label: "s",
    ascii: "s",
    what: "sort: size name date count",
};
/// `t`.
pub static LABELS: Binding = Binding {
    strokes: &[key(KeyCode::Char('t'), Action::Labels)],
    label: "t",
    ascii: "t",
    what: "labels on tiles from 8×2",
};
/// `c`.
pub static COLOR: Binding = Binding {
    strokes: &[key(KeyCode::Char('c'), Action::Color)],
    label: "c",
    ascii: "c",
    what: "cycle the color mode",
};
/// `p`.
pub static PACK: Binding = Binding {
    strokes: &[key(KeyCode::Char('p'), Action::Pack)],
    label: "p",
    ascii: "p",
    what: "cycle the theme pack",
};
/// `/`.
pub static FILTER: Binding = Binding {
    strokes: &[key(KeyCode::Char('/'), Action::Filter)],
    label: "/",
    ascii: "/",
    what: "filter by a glob",
};
/// `⇥`.
pub static NEXT_PANE: Binding = Binding {
    strokes: &[key(KeyCode::Tab, Action::FocusNext)],
    label: "⇥",
    ascii: "tab",
    what: "focus the next pane",
};
/// `Shift-⇥`.
pub static PREV_PANE: Binding = Binding {
    strokes: &[key(KeyCode::BackTab, Action::FocusPrev)],
    label: "Shift-⇥",
    ascii: "Shift-tab",
    what: "focus the previous pane",
};
/// `i`.
pub static FACTS: Binding = Binding {
    strokes: &[key(KeyCode::Char('i'), Action::Facts)],
    label: "i",
    ascii: "i",
    what: "the selection's full facts",
};
/// `y`.
pub static COPY: Binding = Binding {
    strokes: &[key(KeyCode::Char('y'), Action::Copy)],
    label: "y",
    ascii: "y",
    what: "copy the path",
};
/// `o`.
pub static OPEN_OUT: Binding = Binding {
    strokes: &[key(KeyCode::Char('o'), Action::Open)],
    label: "o",
    ascii: "o",
    what: "show in the file manager",
};
/// `d`.
pub static MARK: Binding = Binding {
    strokes: &[key(KeyCode::Char('d'), Action::Mark)],
    label: "d",
    ascii: "d",
    what: "mark or unmark for Trash",
};
/// `D`.
pub static MARKS: Binding = Binding {
    strokes: &[key(KeyCode::Char('D'), Action::MarkPane)],
    label: "D",
    ascii: "D",
    what: "mark pane: review, confirm",
};
/// `r`.
pub static RESCAN: Binding = Binding {
    strokes: &[key(KeyCode::Char('r'), Action::Rescan)],
    label: "r",
    ascii: "r",
    what: "rescan the root, reload",
};
/// `?`.
pub static HELP: Binding = Binding {
    strokes: &[key(KeyCode::Char('?'), Action::Help)],
    label: "?",
    ascii: "?",
    what: "this help",
};
/// `q`, `Ctrl-C`.
pub static QUIT: Binding = Binding {
    strokes: &[
        key(KeyCode::Char('q'), Action::Quit),
        ctrl(KeyCode::Char('c'), Action::Quit),
    ],
    label: "q Ctrl-C",
    ascii: "q Ctrl-C",
    what: "quit",
};

/// Every binding, in the order the help lists them.
pub static ALL: [&Binding; 26] = [
    &MOVE, &OPEN, &UP, &BACK, &ZOOM, &OUT, &RESET, &FIRST, &LAST, &PAGE, &VIEWS, &SORT, &LABELS,
    &COLOR, &PACK, &FILTER, &NEXT_PANE, &PREV_PANE, &FACTS, &COPY, &OPEN_OUT, &MARK, &MARKS,
    &RESCAN, &HELP, &QUIT,
];

/// What `event` asks for, by the key map: `None` for a key that is not
/// the viewer's.
#[must_use]
pub fn lookup(event: &KeyEvent) -> Option<Action> {
    if event.modifiers.contains(KeyModifiers::ALT) {
        return None;
    }
    let ctrl = event.modifiers.contains(KeyModifiers::CONTROL);
    ALL.iter()
        .flat_map(|binding| binding.strokes)
        .find(|stroke| stroke.code == event.code && stroke.ctrl == ctrl)
        .map(|stroke| stroke.action)
}

/// The six keys the key line shows for `tab` with `focus`, written for a
/// terminal with UTF-8 or (`ascii`) without; `sort` and `color` are the
/// modes the `s` and `c` items are on. The map shows how to move on it;
/// the lists how to move along them and what `⏎` does there.
#[must_use]
pub fn line<'a>(
    tab: Tab,
    focus: Focus,
    ascii: bool,
    sort: &'a str,
    color: &'a str,
) -> [KeyItem<'a>; 6] {
    let moves = |what| item(if ascii { "jk" } else { "↑↓" }, what);
    let (mv, open, third, fourth) = match (tab, focus) {
        (Tab::Treemap, Focus::Map) => (
            item(if ascii { "hjkl" } else { "←↓↑→" }, "move"),
            item(OPEN.keys(ascii), "open"),
            item(UP.keys(ascii), "up"),
            item(ZOOM.keys(ascii), "zoom"),
        ),
        (_, Focus::Siblings) => (
            moves("move"),
            item(OPEN.keys(ascii), "open"),
            item(NEXT_PANE.keys(ascii), "pane"),
            KeyItem {
                key: SORT.keys(ascii),
                what: "sort",
                active: Some(sort),
            },
        ),
        (_, Focus::Legend) => (
            moves("move"),
            item(OPEN.keys(ascii), "filter"),
            item(NEXT_PANE.keys(ascii), "pane"),
            KeyItem {
                key: COLOR.keys(ascii),
                what: "color",
                active: Some(color),
            },
        ),
        (Tab::List, Focus::Map) => (
            moves("move"),
            item(OPEN.keys(ascii), "open"),
            item(UP.keys(ascii), "up"),
            KeyItem {
                key: SORT.keys(ascii),
                what: "sort",
                active: Some(sort),
            },
        ),
        (Tab::Largest, Focus::Map) => (
            moves("move"),
            item(OPEN.keys(ascii), "show"),
            item(FILTER.keys(ascii), "filter"),
            item(VIEWS.keys(ascii), "views"),
        ),
        (Tab::Types, Focus::Map) => (
            moves("move"),
            item(OPEN.keys(ascii), "filter"),
            item(VIEWS.keys(ascii), "views"),
            KeyItem {
                key: COLOR.keys(ascii),
                what: "color",
                active: Some(color),
            },
        ),
    };
    [
        mv,
        open,
        third,
        fourth,
        item(HELP.keys(ascii), "help"),
        item("q", "quit"),
    ]
}

/// The keys the mark pane answers.
#[must_use]
pub fn pane_line(ascii: bool) -> [KeyItem<'static>; 2] {
    [
        item(OPEN.keys(ascii), "confirm"),
        item(BACK.keys(ascii), "close"),
    ]
}

/// A key and its words, on no mode.
fn item<'a>(key: &'a str, what: &'a str) -> KeyItem<'a> {
    KeyItem {
        key,
        what,
        active: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;

    fn press(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn p4_tui_05_every_stroke_of_the_map_is_found_by_its_key() {
        for binding in ALL {
            for stroke in binding.strokes {
                let modifiers = if stroke.ctrl {
                    KeyModifiers::CONTROL
                } else {
                    KeyModifiers::NONE
                };
                assert_eq!(
                    lookup(&press(stroke.code, modifiers)),
                    Some(stroke.action),
                    "{}",
                    binding.label
                );
            }
        }
    }

    #[test]
    fn p4_tui_05_no_key_is_bound_twice_and_alt_keys_are_not_the_viewers() {
        let mut seen = Vec::new();
        for binding in ALL {
            for stroke in binding.strokes {
                let key = (stroke.code, stroke.ctrl);
                assert!(!seen.contains(&key), "{key:?} bound twice");
                seen.push(key);
            }
        }
        for code in [KeyCode::Char('q'), KeyCode::Enter, KeyCode::Char('z')] {
            assert_eq!(lookup(&press(code, KeyModifiers::ALT)), None, "{code:?}");
        }
        // Shift is part of what the terminal sends: `Z`, not `z`.
        assert_eq!(
            lookup(&press(KeyCode::Char('Z'), KeyModifiers::SHIFT)),
            Some(Action::Nav(Nav::Out))
        );
        assert_eq!(
            lookup(&press(KeyCode::BackTab, KeyModifiers::SHIFT)),
            Some(Action::FocusPrev)
        );
        assert_eq!(
            lookup(&press(KeyCode::Char('Q'), KeyModifiers::SHIFT)),
            None
        );
    }

    #[test]
    fn p4_tui_05_the_key_line_shows_six_keys_of_the_map_for_each_pane() {
        let ascii_options = [false, true];
        for ascii in ascii_options {
            for tab in [Tab::Treemap, Tab::List, Tab::Largest, Tab::Types] {
                for focus in [Focus::Map, Focus::Siblings, Focus::Legend] {
                    let items = line(tab, focus, ascii, "size", "type");
                    assert_eq!(items.len(), 6);
                    assert_eq!(items[4].what, "help");
                    assert_eq!(items[5].what, "quit");
                    if ascii {
                        assert!(items.iter().all(|i| i.key.is_ascii()), "{items:?}");
                    }
                }
            }
        }
        // Each stroke is written in its binding's label: what `?` lists is
        // what the map holds, key for key.
        for binding in ALL {
            for stroke in binding.strokes {
                let word = match stroke.code {
                    KeyCode::Char(c) if stroke.ctrl => format!("Ctrl-{}", c.to_ascii_uppercase()),
                    KeyCode::Char(c) => c.to_string(),
                    KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down => continue,
                    KeyCode::Enter => "⏎".to_owned(),
                    KeyCode::Backspace => "⌫".to_owned(),
                    KeyCode::Tab => "⇥".to_owned(),
                    KeyCode::BackTab => "Shift-⇥".to_owned(),
                    KeyCode::Home => "Home".to_owned(),
                    KeyCode::End => "End".to_owned(),
                    KeyCode::PageUp => "PgUp".to_owned(),
                    KeyCode::PageDown => "PgDn".to_owned(),
                    KeyCode::Esc => "Esc".to_owned(),
                    other => panic!("a key the map holds that the label test does not: {other:?}"),
                };
                assert!(binding.label.contains(&word), "{word} of {}", binding.label);
            }
        }
        // The map's line keeps to the first frame's keys.
        let map: Vec<(&str, &str)> = line(Tab::Treemap, Focus::Map, false, "size", "type")
            .iter()
            .map(|i| (i.key, i.what))
            .collect();
        assert_eq!(
            map,
            [
                ("←↓↑→", "move"),
                ("⏎", "open"),
                ("⌫", "up"),
                ("z", "zoom"),
                ("?", "help"),
                ("q", "quit")
            ]
        );
    }
}
