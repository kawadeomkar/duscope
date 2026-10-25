# duscope — agent instructions

duscope is a Rust disk-usage analyzer: a parallel scanner, a SQLite index with precomputed
subtree totals, twelve query verbs with one JSON envelope, a terminal treemap, and an MCP server.
The design lives in two documents; read the parts a task cites before writing code:
- plan/SPEC.md — the work packages, acceptance checks, conventions, kept in step with what was
  built (plan/ is gitignored: the author's machine holds it, with a local git history)
- plan/PROPOSAL.md — the why; its "Implementation specification" section wins any conflict

## Invariants (never trade these away in a package)
- Platform-neutral core: nothing above duscope-walk knows the OS; a platform-only fact is a
  nullable field marked `platforms`, never a verb or flag that exists on one OS.
- Linux and macOS are both first-class: every change builds, tests and benchmarks on both.
- The scanner never follows symlinks by default, never reads file contents (the 43-byte
  CACHEDIR.TAG check and explain's FIEMAP ioctl are the only exceptions), never writes.
- Indexes are immutable snapshots: a rescan writes a new file; nothing edits an index in place.
- Names are bytes (BLOB in the index); the envelope adds name_raw only for invalid UTF-8.
- The envelope changes additively only; every list verb's rows are `entries` plus the shared
  fields; summary, schema and explain return one object.
- In --json, --plain and mcp modes nothing but the payload reaches stdout; humans get stderr.
- clean is a dry run unless --yes; trash by default; --permanent re-checks (dev, ino) first.
- The binary opens no socket. No telemetry, no update checks, no HTTP crates.
- One binary, one package: no component feature flags. The only Cargo features are
  `system-sqlite` and `pixels`.
- A new dependency needs a row in DEPENDENCIES.md, a cargo deny pass, and the size budget
  (<= 8 MB stripped) and build budget (<= 90 s) to hold. Never add: rmcp or tokio to the
  binary, resvg, jaq-core, objc2-core-services, catppuccin, supports-color, terminal_size.

## Commands
- `just setup`      install the pinned tools for this platform (see CONTRIBUTING.md)
- `just check`      fmt + clippy -D clippy::all + tests + doc + deny (licenses, bans, sources)
- `just check-full` check + advisory database + ignored platform tests (what CI runs)
- `just test-pkg P2-QRY-06`   the tests of one package (name prefix p2_qry_06)
- `just test-crate duscope-core`
- `just fixtures`   build the reference tree under target/fixtures.noindex (`--force` rebuilds)
- `just golden`     regenerate golden files after an intended output change (review the diff)
- `just bench`      hyperfine harness on the three seeded trees (results in bench/results)
- `just size`       release build, strip, print the binary size against the budget
- `just repro`      the one reproducible release-build recipe CI and cargo-dist use
- `just deb`        build the .deb packages locally
- `cargo +nightly fuzz run <target>`   fuzzing needs the nightly toolchain

## Code conventions
- Rust 1.88 (rust-toolchain.toml). Edition 2024. `cargo fmt` defaults; clippy config in
  Cargo.toml [workspace.lints]; CI runs clippy::all as errors, pedantic as warnings.
- `#![forbid(unsafe_code)]` in every crate except duscope-walk, whose `src/platform/` modules
  (macos/, linux/, posix/) hold the only `unsafe` blocks, each with a `// SAFETY:` comment.
- Errors: one `thiserror` enum per crate; `anyhow` only in the duscope binary; every error
  that reaches a user maps to an exit code and an error `code` from the conventions appendix.
- No `unwrap`/`expect` outside tests (`#![cfg_attr(test, allow(clippy::unwrap_used,
  clippy::expect_used))]` at each crate root); no `println!` outside duscope/src/out/
  (the only module with `#![allow(clippy::print_stdout)]`); logging via the `log` facade.
- Flag, field, warning and error names follow plan/SPEC.md "Conventions appendix"; a new name
  is a conventions change, not a local choice. Test-only knobs are `DUSCOPE_TEST_*` env vars.
- Platform code is `cfg(target_os = ...)` inside duscope-walk/src/platform/; a `cfg` anywhere
  else is a review question.

## Tests
- Write the acceptance checks of the package as tests first; name them `<id>_<behaviour>`.
- Golden tests compare `--json`, `--plain` and treemap buffers against duscope/tests/golden/,
  one snapshot per OS where output differs (`@macos` / `@linux` suffix), regenerated on both
  platforms in the same PR; property tests beside the code (proptest); PTY tests in
  duscope/tests/pty/; fuzz targets in fuzz/. The duscope-testkit crate sets up the environment
  of every command a test runs (temp HOME, XDG dirs, DUSCOPE_INDEX_DIR, TZ=UTC, LC_ALL=C.UTF-8,
  NO_COLOR unset, 80x24, DUSCOPE_TEST_NOW). It cannot change the test process's own
  environment, so a test calling library code passes those values from the `TestEnv`
  accessors. Never set them by hand.
- Tests never touch the network, the real home directory or the user's index directory.
- Run `just test-pkg <ID>` while working and `just check` before opening the PR; it must be green.
- Never modify, skip or ignore a test outside your package; report it in the PR instead.

## Working beside other agents
- You have one package; the author claimed it in the Tracker (Owner, In progress) before this
  session. Other packages are in flight at the same time.
- Touch only the files in your row (a new file inside them is fine). If you need a file that
  belongs to another in-flight package, stop and ask in your PR; never edit it.
- Branch `<id>-<slug>` from main; rebase onto main, never merge, when main moves. There is no
  merge queue: branch protection requires the branch to be up to date with main, and CI must
  pass on the up-to-date branch before it merges.
- Every user-visible change gets a fragment `changelog.d/<ID>.md` (keep-a-changelog lines under
  `### Added` / `### Changed` / `### Fixed` …). No fragment means the `no-changelog` label, set
  by the author. Never edit CHANGELOG.md directly; release.yml assembles it.

## Pull requests
- One package per PR. Title `P2-IDX-04: <package title>`. Body from the template: what was
  done, each acceptance check with its result here and the CI link for the other platform,
  anything the spec did not cover and how it was decided, the spec changes the work needs, the
  changelog fragment, and last a `Tracker: <ID> In review — <one-line status>` line (`Blocked`
  on a deviation draft).
- Commits: `<ID>: imperative summary`, then a body that says what and why. A
  `Co-Authored-By: Claude <model> <noreply@anthropic.com>` trailer is fine; never put a
  `Claude-Session:` trailer or a link to a Claude Code session in any file, commit or PR.
- Do not widen a package. If the spec is wrong, incomplete or impossible, push your tests, open
  a draft PR titled `<ID>: BLOCKED — proposed deviation` with the proposed entry (finding,
  evidence, proposal row revised, packages affected, change), and stop. The author decides.
- A package has a Size (S: one session, M: a few, L: the ceiling). If yours is clearly larger
  than its size, stop and propose a split in the PR.

## Updating the spec
- plan/ is the only copy of SPEC.md and PROPOSAL.md: no export replaces it. Change them only
  for work that has merged, or for a decision the author has taken, never for your own
  unapproved deviation. Commit each change in plan/'s own repository (`git -C plan commit`,
  no remote), titled `DEV-<n>: <summary>`.
- A PR whose work departs from the spec drafts the edits under "Spec changes" in its body, so
  they are reviewed with the code. Once the author says it merged, apply them with the merge
  commit: rewrite the SPEC.md text to say what was built, put `(DEV-<n>)` after it, and add the
  row to SPEC.md's Deviation log (finding and reason, rows revised, packages, change, who
  decided, Status `implemented in #<PR>, <commit>`). A fix that changes no design (a count, a
  path, a name) gets a row too, its finding starting `Correction:`.
- Never rewrite PROPOSAL.md's body: append a dated entry to the "Changelog" section at its end
  (what changed against the proposal, why, the DEV row, the PR and the commit).

## Repository map
crates/duscope-walk (scanner, platform providers, FFI) · crates/duscope-core (arena, accounting,
index, queries, envelope types) · crates/duscope-treemap (layout, static render, SVG/PNG) ·
crates/duscope-tui (interactive viewer) · crates/duscope-mcp (stdio JSON-RPC layer) ·
crates/duscope-testkit (test environment, dev only) · duscope (the binary: clap, output layer,
verbs) · schemas/ · tests/ · bench/ · fuzz/ · docs/ (public documentation) · plan/ (gitignored
spec, own history) · debian/ · changelog.d/ · .github/workflows/
