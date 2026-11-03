//! Tests that run duscope in a pseudo-terminal, through `termlens`.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod clean;
mod output;
mod view;
