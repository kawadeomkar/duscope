# Dependencies

Every external crate a workspace crate depends on directly has a row here. The row gives what the crate is for, the alternative considered, why it isn't written in-tree, its pin, its maintainer count and its last release. `deny.toml` enforces the policy below. `duscope/tests/dependencies.rs` fails when these tables and `cargo metadata` disagree.

## Policy

- **Admission.** A new crate needs all of the following:
  - a release in the last twelve months
  - two maintainers, or the code vendored instead (see [VENDOR.md](VENDOR.md))
  - semver 1.0 or later, or an exact pin
  - pure Rust, unless it is SQLite or libc
  - features trimmed to what is used
  - no second implementation of something the tree already has: one gitignore engine, one schema source, one query language
- **Pins.** Versions are set once, in `[workspace.dependencies]`. A 0.x crate is pinned with `=`. A crate at 1.0 or later takes a caret requirement, and the committed `Cargo.lock` holds the exact version. A new crate starts at its newest release on the day its package starts. Pins change only in a `DEPS: <yyyy-mm>` pull request, which updates this file in the same commit.
- **Licences.** MIT, MIT-0, Apache-2.0, BSD-3-Clause, ISC, OFL-1.1, Unicode-3.0, Unicode-DFS-2016 and Zlib (`[licenses]` in `deny.toml`). MIT-0 and Zlib were added by DEV-2 for jsonschema's dependencies.
- **Never added.** tokio, rmcp, resvg, jaq-core, reqwest, hyper, openssl, objc2-core-services, catppuccin, supports-color, terminal_size, ansi_colours, directories, notify and fsevent-sys (`[bans]` in `deny.toml`, each with its reason).
- **Graph.** One version of each crate (`multiple-versions = "deny"`), from crates.io only (`[sources]`), with no open RustSec advisory (`[advisories]`).
- **Dev-dependencies** are held to the licence list, crates.io as the only source and the advisory database like every other crate, but are exempt from the bans and the one-version rule: `just deny` runs `cargo deny --exclude-dev check bans`. That exemption lets rmcp serve as the MCP conformance oracle in tests without entering the binary.
- **Budgets.** With a new crate, the stripped release binary must stay at 8 MB or less and a cold `cargo build --release` at 90 s or less. Both are CI jobs.
- **Updates.** Dependabot opens one grouped pull request a month for cargo and one for GitHub Actions. Each lands as a `DEPS: <yyyy-mm>` commit once `just check`, `cargo deny`, the budgets and the benchmark regression job pass.

## Checks

- `just check` runs `just deny`: `cargo deny check licenses sources`, then `cargo deny --exclude-dev check bans`. `just check-full` (what CI runs) adds `advisories`.
- `just test-pkg P0-REPO-04` runs `duscope/tests/dependencies.rs`. It checks that each table lists exactly the crates `cargo metadata` reports for it, with the manifests' pins, and that every 0.x pin is exact.
- The same test checks the licence of every crate that only dev-dependencies pull in. `exclude-dev` takes those crates out of every `cargo deny` check, so the test holds them to the `[licenses]` list itself.

## Reading the tables

- **Which table.** The first lists the direct normal and build dependencies of the six published crates: what ships in the binary and what a crates.io build compiles. The second lists everything else a workspace crate depends on directly: dev-dependencies, and the dependencies of unpublished members (`duscope-testkit`, the fixture generator). A crate in both scopes is listed once, in the first table. Transitive crates aren't listed; `cargo tree` shows them.
- **Pin** is the version requirement as `cargo metadata` prints it: `=x.y.z`, or `^x.y.z` with the exact version in `Cargo.lock`. Requirements that differ between crates are listed comma-separated.
- **Maintainers** is the number of crates.io owners (users and teams) who can publish the crate.
- **Last release** is the crate's newest version on crates.io and its date.
- Maintainers and last release are read from the crates.io API (`/api/v1/crates/<name>` and `/api/v1/crates/<name>/owners`) when a row is added or its pin changes.

## Dependencies of the published crates

None yet: the first arrive with phase 1.

| Crate | Purpose | Alternative considered | Why not in-tree | Pin | Maintainers | Last release |
| --- | --- | --- | --- | --- | --- | --- |

## Dev-dependencies

| Crate | Purpose | Alternative considered | Why not in-tree | Pin | Maintainers | Last release |
| --- | --- | --- | --- | --- | --- | --- |
| `cargo_metadata` | Typed `cargo metadata` output for the workspace tests: the crate dependency direction (`duscope/tests/scaffold.rs`), these tables and the dev-only licences (`duscope/tests/dependencies.rs`), and the check that the schema crates stay dev-only (`crates/duscope-core/tests/schema_drift.rs`) | `guppy`, which builds on `cargo_metadata` and adds a graph-query layer these tests don't need; parsing `cargo tree` text, whose format isn't stable | The data is cargo's JSON format. An in-tree reader would still need `serde` and `serde_json`, plus types that track cargo's schema | `=0.23.1` | 1 | 0.23.1, 2025-11-12 |
| `jsonschema` | Checks every fixture under `schemas/` against the JSON Schema 2020-12 meta-schema, and compiles each one to prove its `$ref`s resolve inside the file (`duscope/tests/schemas.rs`); default features off, so it fetches nothing: no HTTP or file resolver, no `reqwest` or `rustls` | `boon`, whose last release (0.6.1, 2025-01-07) is outside the twelve-month window; a Python validator in CI, which would leave the check out of `cargo test` | A 2020-12 validator with the bundled meta-schemas is a crate's worth of work, and `jsonschema` is on the audited dev list (D36) for the meta-validation | `=0.60.0` | 1 | 0.60.0, 2026-10-22 |
| `schemars` | Derives the JSON Schema of the envelope types in duscope-core's unit tests, through `#[cfg_attr(test, derive(JsonSchema))]`, for the drift test against the hand-maintained `schemas/` files (`crates/duscope-core/src/schema_drift.rs`); feature `derive` only | Generating the files from the types at build time, which would make the code the schema source and leave nothing to check the hand-maintained files against | A derive that follows the types' serde attributes is the drift oracle the spec names, and `schemars` is on the audited dev list (D36) | `^1.2.3` | 1 | 1.2.3, 2026-10-12 |
| `serde` | Deserialises the fixture manifest and its build record into typed structs, with `toml` (`tests/fixtures/mktree`); features `std` and `derive` | Walking `toml::Table` values by hand: about a hundred lines of field checks in place of the derive, and weaker error messages | It is the framework `toml` deserialises through, and it is on the audited list (D36) for the envelope types | `^1.0.229` | 2 | 1.0.229, 2026-07-18 |
| `serde_json` | Parses the `schemas/` files and compares them with the derived schemas as values (`duscope/tests/schemas.rs`, duscope-core's drift test); feature `std` | Comparing the files as text, which fails on key order and whitespace in hand-maintained files | It is the JSON value type `schemars` and `jsonschema` both work in, and it is on the audited list (D36) for the envelope | `^1.0.154` | 2 | 1.0.154, 2026-10-11 |
| `sha2` | The manifest hash: SHA-256 of the expanded fixture manifest, printed by mktree, recorded in the manifest and usable as the CI cache key; default features off | std's `DefaultHasher`, whose output may change between Rust releases; an in-tree SHA-256 | The hash is compared across machines and releases, so it must be a standard one. `sha2` is on the audited list for handles and scan ids | `=0.11.1` | 3 | 0.11.1, 2026-10-12 |
| `tempfile` | Private temporary directories for `TestEnv` (`HOME`, the XDG directories, `DUSCOPE_INDEX_DIR`, `TMPDIR`), the tests' scratch trees (`crates/duscope-testkit`, mktree's tests) and the throwaway repositories of the release-workflow test (`duscope/tests/release_workflow.rs`) | Directories under `std::env::temp_dir()` named from the pid and a counter, created and removed by hand | Race-free creation (exclusive create, random names) and removal on drop are what the crate exists for; it is on the audited dev list | `^3.27.0` | 1 | 3.27.0, 2026-03-11 |
| `toml` | Parses the fixture manifest (`tests/fixtures/mktree/manifest.toml`) and the build record (`record.toml`); features `std`, `parse` and `serde`, no writer | A line-based manifest format, which the conventions rule out: data files are TOML | A TOML parser is a crate's worth of work, and `toml` is on the audited list for the config file and the rule tables | `^1.1.8` | 2 | 1.1.8+spec-1.1.0, 2026-10-09 |
