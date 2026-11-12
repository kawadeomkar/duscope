//! The interactive viewer behind `duscope view` and `duscope scan -i`.
//!
//! This crate is an implementation detail of the `duscope` binary and promises
//! no API stability before 1.0.
//!
//! [`Tui`] holds the terminal (raw mode, the alternate screen, mouse
//! capture) and gives it back when dropped; [`restore`] gives it back from a
//! panic hook. [`App`] is what the viewer shows and how it answers events,
//! and [`run`] drives it until `q` or `Ctrl-C`. [`layout`] places the
//! header, the panes and the key line, [`widgets`] draws the design
//! system's components, and the panes' contents come from `panes`. The
//! cursor (`cursor`) moves over the map on the selected path (`path`),
//! whose visible root the zoom (`zoom`) sets. The key map (`keys`) routes
//! every key and is what `?` lists; `views` holds the list, largest and
//! types tables and the help, `mask` the map's filter.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod app;
mod cursor;
mod keys;
pub mod layout;
mod mask;
mod panes;
mod path;
mod term;
mod views;
pub mod widgets;
mod zoom;

pub use app::{App, Effects, Flow, Opener, TermWrite, run, snapshot};
pub use crossterm::event::Event;
pub use panes::{ScanFacts, Sources};
pub use term::{Background, MIN_SIZE, Tui, next_event, open_detached, restore, size};

#[cfg(test)]
mod tests;
