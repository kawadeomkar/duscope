# duscope task runner. `just` lists the recipes; CONTRIBUTING.md has the workflow.

set shell := ["bash", "-euo", "pipefail", "-c"]

# Stripped release binary budget in bytes (8 MB); P1-REL-01 tightens it to the
# measured size plus 10%.
size_budget := "8000000"

# Developer tools installed by `just setup`, mirrored in CONTRIBUTING.md.
cargo_deny_version := "0.20.2"
cargo_insta_version := "1.49.0"
cargo_fuzz_version := "0.13.2"
cargo_llvm_cov_version := "0.9.1"
cargo_deb_version := "3.8.0"
cargo_dist_version := "0.33.1"
# The commit of dist's v0.33.1 tag: a tag can move, a commit cannot.
cargo_dist_rev := "665cb7d48902a30386857f8f679e1b1942876f06"
hyperfine_version := "2.0.0"

default:
    @just --list

# Install the pinned toolchains and tools for this platform
setup:
    #!/usr/bin/env bash
    set -euo pipefail
    command -v rustup >/dev/null || { echo "setup: install rustup first (https://rustup.rs)" >&2; exit 1; }
    # The pinned toolchain from rust-toolchain.toml, plus llvm-tools for coverage.
    rustup toolchain install
    rustup component add rustfmt clippy llvm-tools-preview
    # stable builds the tools below and runs the msrv job's second leg; nightly runs cargo fuzz.
    rustup toolchain install stable nightly --profile minimal
    case "$(uname -s)" in
      Darwin)
        # GNU du (installed as gdu) for the du parity checks; mandoc and xmllint ship with macOS.
        brew install coreutils ;;
      Linux)
        sudo=""; [[ $(id -u) -eq 0 ]] || sudo=sudo
        $sudo apt-get update -qq
        $sudo env DEBIAN_FRONTEND=noninteractive apt-get install -y -qq build-essential mandoc lintian libxml2-utils reprepro ;;
      *) echo "setup: unsupported platform $(uname -s)" >&2; exit 1 ;;
    esac
    installed=$(rustup run stable cargo install --list)
    install() {  # crate version [cargo install arguments]
      if grep -q "^$1 v$2[: ]" <<<"$installed"; then echo "setup: $1 $2 already installed"; return; fi
      rustup run stable cargo install --locked "${@:3}"
    }
    install cargo-deny {{cargo_deny_version}} cargo-deny@{{cargo_deny_version}}
    install cargo-insta {{cargo_insta_version}} cargo-insta@{{cargo_insta_version}}
    install cargo-fuzz {{cargo_fuzz_version}} cargo-fuzz@{{cargo_fuzz_version}}
    install cargo-llvm-cov {{cargo_llvm_cov_version}} cargo-llvm-cov@{{cargo_llvm_cov_version}}
    install cargo-deb {{cargo_deb_version}} cargo-deb@{{cargo_deb_version}}
    install hyperfine {{hyperfine_version}} hyperfine@{{hyperfine_version}}
    # dist releases after 0.32 are published on GitHub only, not on crates.io.
    install cargo-dist {{cargo_dist_version}} --git https://github.com/axodotdev/cargo-dist --rev {{cargo_dist_rev}} cargo-dist
    cargo_bin="${CARGO_HOME:-$HOME/.cargo}/bin"
    command -v cargo >/dev/null || echo "setup: put rustup's cargo proxy on PATH (Homebrew: $(brew --prefix 2>/dev/null)/opt/rustup/bin)" >&2
    [[ ":$PATH:" == *":$cargo_bin:"* ]] || echo "setup: add $cargo_bin to PATH for hyperfine and dist" >&2

# fmt, clippy, tests, rustdoc and cargo deny (licences, bans, sources)
check: fmt clippy test docs deny

# check, plus the advisory database and the ignored platform tests (what CI runs)
check-full: check
    just deny advisories
    DUSCOPE_TEST_PLATFORM=1 cargo test --workspace -- --ignored

fmt:
    cargo fmt --all --check

clippy:
    cargo clippy --workspace --all-targets -- -D clippy::all

# cargo deny over the given checks; the bans leave dev-dependencies out (deny.toml says why)
deny *checks="licenses bans sources":
    #!/usr/bin/env bash
    set -euo pipefail
    full=()
    bans=false
    for check in {{checks}}; do
      if [[ $check == bans ]]; then bans=true; else full+=("$check"); fi
    done
    if (( ${#full[@]} )); then cargo deny check "${full[@]}"; fi
    if $bans; then cargo deny --exclude-dev check bans; fi

test:
    cargo test --workspace

# The tests of one package: `just test-pkg P2-QRY-06` runs the tests named p2_qry_06_*
test-pkg id:
    #!/usr/bin/env bash
    set -euo pipefail
    filter='{{replace(lowercase(id), "-", "_")}}_'
    count=$(cargo test --workspace --quiet -- --list "$filter" | grep -c ': test$' || true)
    [[ $count -gt 0 ]] || { echo "test-pkg: no test named ${filter}*" >&2; exit 1; }
    cargo test --workspace -- "$filter"

# The tests of one crate: `just test-crate duscope-core`
test-crate crate:
    cargo test -p {{crate}}

# rustdoc for the workspace with warnings denied
docs:
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps

# Release build and the stripped binary size against the budget
size:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo build --release --locked -p duscope
    bytes=$(wc -c < target/release/duscope | tr -d ' ')
    awk -v b="$bytes" -v l={{size_budget}} 'BEGIN { printf "duscope: %d bytes stripped, %.2f MB of the %.2f MB budget\n", b, b / 1e6, l / 1e6 }'
    (( bytes <= {{size_budget}} )) || { echo "size: over budget" >&2; exit 1; }

# The reproducible release build CI and releases use; prints the binary's SHA-256
repro target_dir="target/repro":
    #!/usr/bin/env bash
    set -euo pipefail
    env_lines=$('{{just_executable()}}' --justfile '{{justfile()}}' repro-env '{{target_dir}}')
    while IFS= read -r line; do export "$line"; done <<< "$env_lines"
    cargo build --release --locked -p duscope --target-dir '{{target_dir}}'
    if command -v sha256sum >/dev/null; then sha256sum '{{target_dir}}/release/duscope'; else shasum -a 256 '{{target_dir}}/release/duscope'; fi

# The environment of the reproducible build as KEY=value lines: repro builds under it, and release.yml's cargo-dist builds too
repro-env target_dir="target/repro":
    #!/usr/bin/env bash
    set -euo pipefail
    # Cargo splits RUSTFLAGS on whitespace, so the remapped paths cannot hold any.
    for dir in '{{justfile_directory()}}' "${CARGO_HOME:-$HOME/.cargo}" '{{target_dir}}'; do
      if [[ $dir == *[[:space:]]* ]]; then
        echo "repro: '$dir' contains whitespace, which RUSTFLAGS cannot carry; use a path without any" >&2
        exit 1
      fi
    done
    echo "SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-$(git log -1 --format=%ct)}"
    echo "CARGO_INCREMENTAL=0"
    rustflags="--remap-path-prefix={{justfile_directory()}}=/duscope --remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}=/cargo"
    if [[ $(uname -s) == Darwin ]]; then
      # ld64 records each object file's absolute path in the binary (its UUID survives
      # stripping), so drop the target directory from those paths.
      mkdir -p '{{target_dir}}'
      rustflags+=" -C link-arg=-Wl,-oso_prefix,$(cd '{{target_dir}}' && pwd -P)/"
    fi
    echo "RUSTFLAGS=$rustflags"

# Build the reference fixture tree under target/fixtures.noindex (`--force` rebuilds) and print the manifest hash
fixtures *args:
    cargo run --quiet --locked -p mktree -- {{args}}

# Regenerate golden files after an intended output change (P1-TEST-01)
golden *args:
    @echo "golden: not yet implemented (P1-TEST-01)" >&2; exit 1

# Run the hyperfine harness on the seeded trees (bench/README.md); no arguments: T1, du and diskus, warm
bench *args:
    bench/run.sh {{args}}

# Build the .deb packages (P1-PKG-01)
deb *args:
    @echo "deb: not yet implemented (P1-PKG-01)" >&2; exit 1
