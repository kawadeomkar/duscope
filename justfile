# duscope task runner. `just` lists the recipes; CONTRIBUTING.md has the workflow.

set shell := ["bash", "-euo", "pipefail", "-c"]

# Stripped release binary budget in bytes: the larger of the two release targets
# CI holds to it, measured plus 10% (P1-REL-01), again with the bundled SQLite
# at -O1 (P2-IDX-02, DEV-66), and again with thin LTO as phase 2 grew. Run
# 41263296119 (p2-qry-04-explain, 223cc43) measured x86_64 musl at 6,700,928
# bytes and arm64 macOS at 5,635,232; plus 10% is 7,371,021, so 7.38 MB. Abort
# on panic (P2-QRY-04) then took 13% off (4,889,664 bytes on arm64 macOS) and
# the budget stayed, as room for the rest of phase 2. The spec's ceiling stays
# 8 MB.
size_budget := "7380000"

# Developer tools installed by `just setup`, mirrored in CONTRIBUTING.md.
cargo_deny_version := "0.20.2"
cargo_fuzz_version := "0.13.2"
cargo_llvm_cov_version := "0.9.1"
cargo_deb_version := "3.8.0"
# Embeds the dependency list in the binary `just deb` builds, as release.yml's
# cargo-dist builds do.
cargo_auditable_version := "0.7.7"
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
        # GNU du (installed as gdu) for the du parity checks, and bash 4 or later for the test
        # that runs ci.yml's changelog job (macOS ships 3.2); mandoc, xmllint and jq ship with macOS.
        brew install coreutils bash ;;
      Linux)
        sudo=""; [[ $(id -u) -eq 0 ]] || sudo=sudo
        $sudo apt-get update -qq
        $sudo env DEBIAN_FRONTEND=noninteractive apt-get install -y -qq build-essential mandoc lintian libxml2-utils reprepro jq musl-tools
        # The musl target the release builds and `just deb` use.
        rustup target add "$(uname -m)-unknown-linux-musl" ;;
      *) echo "setup: unsupported platform $(uname -s)" >&2; exit 1 ;;
    esac
    installed=$(rustup run stable cargo install --list)
    install() {  # crate version [cargo install arguments]
      if grep -q "^$1 v$2[: ]" <<<"$installed"; then echo "setup: $1 $2 already installed"; return; fi
      rustup run stable cargo install --locked "${@:3}"
    }
    install cargo-deny {{cargo_deny_version}} cargo-deny@{{cargo_deny_version}}
    install cargo-fuzz {{cargo_fuzz_version}} cargo-fuzz@{{cargo_fuzz_version}}
    install cargo-llvm-cov {{cargo_llvm_cov_version}} cargo-llvm-cov@{{cargo_llvm_cov_version}}
    install cargo-deb {{cargo_deb_version}} cargo-deb@{{cargo_deb_version}}
    install cargo-auditable {{cargo_auditable_version}} cargo-auditable@{{cargo_auditable_version}}
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
    just platform-tests

# The ignored platform tests, named here: an #[ignore]d test joins them (and CI's platform
# job) only when it is listed. They mount filesystems, attach images and scan /, need root
# and skip without it; CI runs each test binary under `sudo -E`. Built in release for the
# scan of /. The system-tree du parity and the RSS budget run in nightly-bench.yml.
platform-tests:
    #!/usr/bin/env bash
    set -euo pipefail
    tests=(
      p1_walk_06_linux_mounts_are_crossed_confined_and_cycles_stopped
      p1_walk_06_linux_mount_ids_hold_for_same_device_binds_and_reused_ids
      p1_walk_06_macos_an_exfat_image_is_crossed_and_confined
      platform::macos::tests::p1_walk_07_a_scan_of_slash_counts_users_once
      p2_idx_02_a_full_tmpfs_fails_the_write_after_the_output
      p2_qry_01_linux_an_exfat_image_folds_case
      p2_qry_01_macos_an_exfat_image_folds_case
      p2_qry_01_macos_an_apfs_image_folds_case_and_nfc
    )
    cargo_test() { DUSCOPE_TEST_PLATFORM=1 cargo test --release --workspace --locked "$@"; }
    # Four are built on Linux and four on macOS; a renamed test must not drop out unnoticed.
    expected=4
    found=$(cargo_test --quiet -- --ignored --exact --list "${tests[@]}" | grep -c ': test$' || true)
    [[ $found -eq $expected ]] || { echo "platform-tests: $found of this platform's $expected tests found; update the list" >&2; exit 1; }
    cargo_test -- --ignored --exact "${tests[@]}"

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
    filter={{quote(replace(lowercase(id), "-", "_") + "_")}}
    count=$(cargo test --workspace --quiet -- --list "$filter" | grep -c ': test$' || true)
    [[ $count -gt 0 ]] || { echo "test-pkg: no test named ${filter}*" >&2; exit 1; }
    cargo test --workspace -- "$filter"

# The tests of one crate: `just test-crate duscope-core`
test-crate crate:
    cargo test -p {{crate}}

# rustdoc for the workspace with warnings denied
docs:
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps

# Release build and the stripped binary size against the budget; a target (x86_64-unknown-linux-musl) builds that one
size target="":
    #!/usr/bin/env bash
    set -euo pipefail
    target={{quote(target)}}
    if [[ -n $target ]]; then
      cargo build --release --locked -p duscope --target "$target"
      binary=target/$target/release/duscope
    else
      cargo build --release --locked -p duscope
      binary=target/release/duscope
    fi
    bytes=$(wc -c < "$binary" | tr -d ' ')
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

# Regenerate this platform's golden files after an intended output change, then list what changed (duscope/tests/golden/README.md); arguments filter the tests
golden *args:
    #!/usr/bin/env bash
    set -euo pipefail
    DUSCOPE_TEST_UPDATE_GOLDEN=1 cargo test --workspace -- {{args}}
    git status --short -- duscope/tests/golden

# Run the hyperfine harness on the seeded trees (bench/README.md); no arguments: T1, du and diskus, warm
bench *args:
    bench/run.sh {{args}}

# Build the .deb package of a musl target (default: this machine's) into target/debian/ (P1-PKG-01; Linux); `binary`: a musl binary to package instead of building one; `assets`: `reuse` packages the man page and completions already in target/deb-assets/
deb target="" binary="" assets="write":
    #!/usr/bin/env bash
    set -euo pipefail
    [[ $(uname -s) == Linux ]] || { echo "deb: builds on Linux (musl-tools and the musl target installed)" >&2; exit 1; }
    target={{quote(target)}}
    [[ -n $target ]] || target="$(uname -m | sed 's/^arm64$/aarch64/')-unknown-linux-musl"
    case $target in x86_64-*) arch=amd64 ;; aarch64-*) arch=arm64 ;; *) echo "deb: no Debian architecture for $target" >&2; exit 1 ;; esac
    mode={{quote(assets)}}
    [[ $mode == write || $mode == reuse ]] || { echo "deb: assets is write or reuse, not $mode" >&2; exit 1; }
    # The man page and completions, from a host build of the same commit. Only
    # release.yml, whose deb-assets job wrote them for both packages, reuses them.
    assets=target/deb-assets
    case $mode in
      write)
        cargo build --release --locked -p duscope
        mkdir -p "$assets"
        target/release/duscope man > "$assets/duscope.1"
        target/release/duscope completions bash > "$assets/duscope.bash"
        target/release/duscope completions zsh > "$assets/_duscope"
        target/release/duscope completions fish > "$assets/duscope.fish" ;;
      reuse)
        for file in duscope.1 duscope.bash _duscope duscope.fish; do
          [[ -s $assets/$file ]] || { echo "deb: $assets/$file is missing; leave out \`reuse\` to write it" >&2; exit 1; }
        done ;;
    esac
    # cargo-deb packages target/<target>/release/duscope. release.yml passes the
    # binary cargo-dist built for the release archive; otherwise build one the
    # same way, with cargo-auditable embedding the dependency list.
    binary={{quote(binary)}}
    packaged="target/$target/release/duscope"
    if [[ -z $binary ]]; then
      cargo auditable build --release --locked -p duscope --target "$target"
    elif [[ $binary != "$packaged" ]]; then
      mkdir -p "target/$target/release"
      cp "$binary" "$packaged"
      chmod 755 "$packaged"
    fi
    debian/changelog.sh
    # The package is version X.Y.Z-1 in a file named duscope_X.Y.Z_<arch>.deb; a
    # pre-release X.Y.Z-rc.1 becomes X.Y.Z~rc.1, which dpkg sorts before X.Y.Z,
    # as debian/changelog.sh writes it.
    version=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1 | sed 's/-/~/')
    deb="target/debian/duscope_${version}_${arch}.deb"
    mkdir -p target/debian
    cargo deb --locked -p duscope --target "$target" --no-build --no-strip --deb-version "$version-1" --output "$deb"
    ls "$deb"
