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
//! system's components, and the panes' contents come from `panes`.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod app;
pub mod layout;
mod panes;
mod term;
pub mod widgets;

pub use app::{App, Flow, run, snapshot};
pub use crossterm::event::Event;
pub use term::{Background, MIN_SIZE, Tui, next_event, restore, size};

#[cfg(test)]
mod tests;
