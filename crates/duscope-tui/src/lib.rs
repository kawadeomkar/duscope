//! The interactive viewer behind `duscope view` and `duscope scan -i`.
//!
//! This crate is an implementation detail of the `duscope` binary and promises
//! no API stability before 1.0.
//!
//! [`Tui`] holds the terminal (raw mode, the alternate screen, mouse
//! capture) and gives it back when dropped; [`restore`] gives it back from a
//! panic hook. [`App`] is what the viewer shows and how it answers events,
//! and [`run`] drives it until `q` or `Ctrl-C`.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod app;
mod term;

pub use app::{App, Flow, run, snapshot};
pub use crossterm::event::Event;
pub use term::{Background, MIN_SIZE, Tui, next_event, restore, size};

#[cfg(test)]
mod tests;
