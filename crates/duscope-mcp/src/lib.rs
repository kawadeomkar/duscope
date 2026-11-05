//! The stdio JSON-RPC layer and tool registry behind `duscope mcp`.
//!
//! This crate is an implementation detail of the `duscope` binary and promises
//! no API stability before 1.0.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod discover;
pub mod jsonrpc;
mod legacy;
pub mod roots;
pub mod server;
pub mod tools;
pub mod transport;
