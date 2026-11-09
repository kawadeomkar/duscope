//! Tests that run duscope in a pseudo-terminal, through `termlens`.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod clean;
mod facts;
mod marks;
mod nav_eval;
mod output;
mod pixels;
mod view;
mod views;
