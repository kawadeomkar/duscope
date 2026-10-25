# duscope

A disk-usage analyzer for macOS and Linux. It scans a directory tree once, in
parallel, into a SQLite index with precomputed subtree totals, and answers
questions about it as tables, JSON, a terminal treemap, or through a stdio MCP
server.

duscope is pre-release: the [repository](https://github.com/kawadeomkar/duscope)
says what works today and lists the other install channels.

## Install

```sh
cargo install duscope --locked
```

Cargo builds the binary with its default release settings, because duscope's
release profile (link-time optimisation, one codegen unit, symbols stripped)
applies only to builds inside its repository. The prebuilt binaries on the
[releases page](https://github.com/kawadeomkar/duscope/releases) are built with
it, so they are smaller.

## Licence

Licensed under either of MIT (`LICENSE-MIT`) or Apache-2.0 (`LICENSE-APACHE`),
at your option.
