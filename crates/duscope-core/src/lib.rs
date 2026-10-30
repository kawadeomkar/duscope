//! The duscope engine: the scan arena and its roll-up, the on-disk index,
//! every query, and the types of the JSON envelope.
//!
//! This crate is an implementation detail of the `duscope` binary and promises
//! no API stability before 1.0.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod accounting;
pub mod arena;
pub mod config;
pub mod envelope;
mod errors;
pub mod index;
pub mod query;
pub mod resolve;
pub mod rollup;
pub mod rules;

#[cfg(test)]
mod schema_drift;

pub use errors::CoreError;
