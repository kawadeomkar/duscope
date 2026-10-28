# Contributing to duscope

This guide covers setup, the tools, the `just` recipes, the package workflow, tests,
benchmarks, packaging and releases. Agents working in this repository read
[CLAUDE.md](CLAUDE.md) first; it states the same rules for a session.

## Prerequisites

- [rustup](https://rustup.rs). The Rust toolchain itself is pinned in `rust-toolchain.toml`
  (1.88), and `just setup` installs it.
- [just](https://just.systems), the task runner.
- [gh](https://cli.github.com), signed in with `gh auth login`, to open pull requests.

Two directories must be on `PATH`:

- rustup's `cargo`, `rustc` and `rustup` proxies. The rustup.rs installer puts them in
  `~/.cargo/bin`. Homebrew's `rustup` is keg-only and keeps them in
  `$(brew --prefix)/opt/rustup/bin` instead; put that directory ahead of any other Rust.
- `~/.cargo/bin` (or `$CARGO_HOME/bin`), where `just setup` installs its tools with
  `cargo install`.

On macOS with Homebrew's rustup, for example:

```sh
export PATH="$(brew --prefix)/opt/rustup/bin:$HOME/.cargo/bin:$PATH"
```

## Setup

```sh
just setup
just check
```

`just setup` installs the pinned toolchain with `rustfmt`, `clippy` and `llvm-tools-preview`,
the stable and nightly toolchains, and the tools in the table below. On macOS it installs GNU
coreutils with Homebrew; on Linux it installs the system packages with `apt-get` (through
`sudo` unless you are root). It skips a tool already installed at its pinned version, so it is
safe to run again after a pin changes.

`just check` is what every pull request must pass: `cargo fmt --check`, clippy with
`-D clippy::all`, the tests, rustdoc with warnings denied, and `cargo deny` (licences, bans,
sources).

## Tool versions

The versions `just setup` installs are pinned at the top of the `justfile`. This table mirrors
them, and the test `p0_repo_02_contributing_tool_versions_match_justfile` fails when the two
disagree, so a pin changes in both places in the same pull request.

| Tool | Version | Installed by | Used for |
| --- | --- | --- | --- |
| `rustup` | 1.88 (`rust-toolchain.toml`), stable and nightly | you; the toolchains by `just setup` | 1.88 builds and tests; stable builds the tools below and runs the MSRV job's second leg; nightly runs `cargo fuzz` |
| `just` | any 1.x release | you | the recipes below |
| `gh` | any 2.x release | you | pull requests |
| `cargo-deny` | 0.20.2 | `just setup` | licences, bans, sources and advisories |
| `cargo-fuzz` | 0.13.2 | `just setup` | fuzz targets, on nightly |
| `cargo-llvm-cov` | 0.9.1 | `just setup` | coverage |
| `cargo-deb` | 3.8.0 | `just setup` | the `.deb` packages |
| `cargo-auditable` | 0.7.7 | `just setup` | the dependency list embedded in the binary `just deb` packages, as in the release archives |
| `cargo-dist` | 0.33.1 | `just setup`, from the GitHub tag (later releases are not on crates.io) | release builds (the `dist` command) |
| `hyperfine` | 2.0.0 | `just setup` | the benchmark harness |
| `mandoc` | system package | `just setup` (apt) on Linux; ships with macOS | linting the man page |
| `lintian` | system package | `just setup` (apt), Linux only | linting the `.deb` packages |
| `xmllint` | system package | `just setup` (apt `libxml2-utils`) on Linux; ships with macOS | validating SVG output |
| `reprepro` | system package | `just setup` (apt), Linux only | the apt repository |
| `gdu` (GNU `du`) | system package | `just setup` (Homebrew `coreutils`), macOS only | the `du` parity tests on macOS |

## Recipes

`just` on its own lists every recipe. A recipe whose package has not landed yet fails with
"not yet implemented".

| Recipe | What it does |
| --- | --- |
| `just setup` | install the pinned toolchains and tools for this platform |
| `just check` | `just fmt`, `just clippy`, `just test`, `just docs` and `just deny`: the pull-request gate |
| `just check-full` | `just check` plus the advisory database and the ignored platform tests; what CI runs (needs the network) |
| `just platform-tests` | the ignored platform tests, by name, in a release build; they need root and skip without it (CI runs them under `sudo`) |
| `just test-pkg P2-QRY-06` | the tests of one package (names starting `p2_qry_06_`) |
| `just test-crate duscope-core` | the tests of one crate |
| `just size [TARGET]` | release build and the stripped binary size against the budget (`size_budget`, 5.25 MB) |
| `just repro` | the reproducible release build, printing the binary's SHA-256 |
| `just fixtures` | build the reference fixture tree under `target/fixtures.noindex` |
| `just golden` | regenerate the golden files after an intended output change |
| `just bench` | the hyperfine harness on the seeded trees |
| `just deb [TARGET]` | build the `.deb` package of a musl target (Linux) |

## Package workflow

duscope is built in work packages. Each has an id `P<phase>-<AREA>-<nn>` (`P2-IDX-04`), a
brief and acceptance checks, and one package is one pull request.

1. Branch `<id>-<slug>` from `main`, for example `p2-idx-04-bulk-writer`.
2. Write the package's acceptance checks as tests first, named after the id and the behaviour
   in snake case (`p2_idx_04_rename_is_atomic`), so that `just test-pkg P2-IDX-04` runs them.
3. Run `just test-pkg <ID>` while working and `just check` before opening the pull request.
4. Commit as `<ID>: imperative summary`, with a body that says what changed and why.
5. A user-visible change adds a changelog fragment, `changelog.d/<ID>.md`, with
   keep-a-changelog lines under `### Added`, `### Changed`, `### Fixed` and so on. A pull
   request with nothing user-visible carries the `no-changelog` label instead, set by the
   maintainer. `CHANGELOG.md` is assembled from the fragments at release; never edit it.
6. When `main` moves, rebase onto it; never merge `main` into the branch. Branch protection
   merges only a branch that is up to date with `main` and green in CI, and every pull request
   is squash-merged.

A change that is not yet a package, such as a bug report or a feature idea, starts as an issue
from one of the templates.

## Tests

- `just test-pkg <ID>` runs a package's tests while you work; `just check` is the gate.
- Golden files live in `duscope/tests/golden/`, one copy per OS where the output differs.
  After an intended output change, `just golden` regenerates this platform's copies and lists
  them; [duscope/tests/golden/README.md](duscope/tests/golden/README.md) says how to get the
  other platform's.
- `just check-full` adds the advisory database and the ignored platform tests, which mount
  filesystems and need root on Linux; CI's `platform` job runs them under `sudo`.
- Tests never touch the network, your home directory or your index directory: the
  `duscope-testkit` crate gives every test its own environment.

## Benchmarks

`just bench` runs the hyperfine harness on the seeded trees: [bench/README.md](bench/README.md)
has the trees, the pinned competitors, the result file and the thread sweep. CI's
`bench-regression` job measures duscope on T1 in every pull request; the nightly job on the
bench runners measures the full matrix.

## Packages and releases

`just deb` builds a `.deb` on Linux with `musl-tools`, the musl target and `cargo-auditable`
installed (`just setup` installs them); it writes the man page and completions afresh each time.
Releases are cut by the maintainer from a signed tag, following
[docs/RELEASING.md](docs/RELEASING.md) step by step; `release.yml` builds, attests and publishes
everything, and nothing is published by hand.

## Pull requests

Open the pull request with `gh pr create --base main`, titled `<ID>: <package title>`. The
template asks for what was done; each acceptance check with its result on macOS and on Linux
(the local run, and the CI run for the other platform); anything the specification did not
cover and how it was decided; whether the change deviates from the specification; the
changelog fragment; and, as the last line, the `Tracker:` line.

If the specification is wrong, incomplete or impossible, push the tests you have, open a draft
pull request titled `<ID>: BLOCKED — proposed deviation` whose body holds the proposed entry
(finding, evidence, proposal rows revised, packages affected, proposed change), and stop.

## Licence

duscope is licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your
option. Unless you explicitly state otherwise, any contribution intentionally submitted for
inclusion in duscope by you, as defined in the Apache-2.0 license, shall be dual licensed as
above, without any additional terms or conditions.
