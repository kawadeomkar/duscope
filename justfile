# duscope task runner. `just` lists the recipes; CONTRIBUTING.md has the workflow.

set shell := ["bash", "-euo", "pipefail", "-c"]

# Stripped release binary budget in bytes: the larger of the two release targets
# CI holds to it, measured plus 10% (P1-REL-01), again with the bundled SQLite
# at -O1 (P2-IDX-02, DEV-66), and again with thin LTO as phase 2 grew. Run
# 41263296119 (p2-qry-04-explain, 223cc43) measured x86_64 musl at 6,700,928
# bytes and arm64 macOS at 5,635,232; plus 10% is 7,371,021, so 7.38 MB. Abort
# on panic (P2-QRY-04) then took 13% off (4,889,664 bytes on arm64 macOS) and
# the budget stayed, as room for the rest of phase 2. Phase 2 ended at 7,352,256
# bytes on x86_64 musl (run 41977141494, 5beb5a9). Phase 3's first tools take it
# to 7,446,464 (P3-MCP-04 and P3-MCP-07), about half of P3-MCP-04's share being
# P3-MCP-03's registry, which a release build drops while no tool is registered.
# Plus 10% would pass the spec's 8 MB ceiling, so the budget is the ceiling
# (DEV-115). The 8 MB ceiling came from the proposal's size probe, which
# underestimated duscope's own code; with x86_64 musl at 7,823,296 bytes on
# main at 6b740b8c (run 42611223243) and phases 3 to 5 still to come, the
# ceiling is re-based once to 10 MB and the measured-plus-10% ratchet ends
# (P0-CI-03): the budget is the ceiling, packages report their size (the size
# job's comment posts it on every pull request to main) and do not re-derive it.
size_budget := "10000000"

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
# The nightly toolchain cargo fuzz runs on: a dated one, so a fuzz run reproduces (P2-TEST-01).
# `just setup` installs it; ci.yml's fuzz-smoke and nightly-fuzz.yml read it here.
fuzz_toolchain := "nightly-2026-10-23"
# The toolchain the `pixels` feature builds on (P4-TUI-07): ratatui-image's
# Sixel encoder, icy_sixel, quantises through quantette, which needs Rust 1.90,
# past the pinned 1.89 the default build keeps. `just setup` installs it; ci.yml's
# pixels job reads it here.
pixels_toolchain := "1.90"

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
    # stable builds the tools below and runs the msrv job's second leg; the dated nightly runs
    # `just fuzz`, and plain nightly `cargo +nightly fuzz`; pixels_toolchain runs `just pixels`.
    rustup toolchain install stable nightly {{fuzz_toolchain}} --profile minimal
    rustup toolchain install {{pixels_toolchain}} --profile minimal --component clippy
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
      p2_idx_07_linux_a_full_temporary_directory_does_not_fail_the_save
      p2_qry_01_linux_an_exfat_image_folds_case
      p2_qry_01_macos_an_exfat_image_folds_case
      p2_qry_01_macos_an_apfs_image_folds_case_and_nfc
      p2_qry_03_linux_a_mounted_image_has_unaccounted_bytes
      p2_qry_03_macos_a_mounted_image_has_unaccounted_bytes
      p4_core_01_macos_a_trashed_file_round_trips_on_an_image
      marks::p4_tui_06_macos_mark_two_items_confirm_and_both_are_in_the_images_trash
      nav_eval::p4_test_01_macos_the_trash_tasks_move_a_file_on_an_image
      p5_mcp_01_macos_a_real_run_on_an_image_moves_only_the_named_items_to_the_trash
      p0_test_01_macos_a_detach_that_leaves_the_image_attached_fails
      p0_test_03_macos_a_detach_busy_for_six_seconds_succeeds
      p0_test_03_macos_a_detach_that_stays_busy_names_what_holds_it
      platform::macos::fsevents::tests::p5_walk_01_macos_no_log_on_an_image_gives_fsevents_disabled
      platform::macos::fsevents::tests::p5_walk_01_macos_a_remount_since_the_mark_voids_a_replay_below_the_root
      macos::p5_core_01_macos_an_incremental_rescan_equals_a_full_scan_of_the_fixture_tree
      macos::p5_core_01_macos_a_volume_without_history_is_rescanned_whole
      p5_walk_02_linux_btrfs_subvolumes_are_boundaries_and_reflinks_share_extents
      p5_walk_02_linux_xfs_reflinks_share_extents
      p5_walk_02_linux_an_overlay_is_entered_and_confined_by_its_own_mount
      p5_walk_02_linux_foreign_filesystems_report_links_btime_and_case_as_they_are
      p5_walk_02_linux_a_squashfs_image_is_read_only
      p5_walk_02_linux_explain_clones_reports_shared_extents_on_reflinks
      p5_walk_02_linux_summary_lists_btrfs_subvolumes
      p5_walk_02_linux_clean_refuses_an_item_on_a_read_only_image
      platform::posix::unlink::tests::p5_core_03_linux_a_mount_under_the_directory_is_never_crossed
      platform::posix::unlink::tests::p5_core_03_macos_a_mounted_image_under_the_directory_is_never_crossed
      p5_core_04_linux_a_folder_holding_a_mount_stays_out_of_the_trash
      clean::p5_core_04_macos_a_folder_holding_a_mounted_image_stays_out_of_the_trash
      clean::tests::p5_core_04_macos_a_change_under_a_future_change_time_is_seen
      platform::macos::trash::tests::p5_core_06_macos_an_image_answers_by_code
      p5_cli_02_macos_a_scan_of_slash_names_the_data_volume_below_private
      p5_walk_06_linux_an_untriggered_automount_is_not_entered
      p5_walk_06_linux_a_systemd_automount_is_not_triggered
      p5_core_10_linux_an_overlays_files_go_to_its_trash_and_its_mounts_stay
      platform::posix::unlink::tests::p5_core_10_linux_a_bound_file_is_a_mount_point_and_an_overlays_file_is_not
    )
    cargo_test() { DUSCOPE_TEST_PLATFORM=1 cargo test --release --workspace --locked "$@"; }
    # Twenty are built on Linux and twenty-one on macOS; a renamed test must not drop out unnoticed.
    expected=$([[ $(uname -s) == Darwin ]] && echo 21 || echo 20)
    found=$(cargo_test --quiet -- --ignored --exact --list "${tests[@]}" | grep -c ': test$' || true)
    [[ $found -eq $expected ]] || { echo "platform-tests: $found of this platform's $expected tests found; update the list" >&2; exit 1; }
    cargo_test -- --ignored --exact "${tests[@]}"

fmt:
    cargo fmt --all --check

clippy:
    cargo clippy --workspace --all-targets -- -D clippy::all

# Run one fuzz target for `seconds` (60: CI's fuzz-smoke; nightly-fuzz.yml runs 600) on the
# pinned nightly. The corpus grows in fuzz/corpus/<target>/ and a crash lands in
# fuzz/artifacts/<target>/, both ignored by git; an input slower than 30 s fails the run.
fuzz target seconds="60":
    cd fuzz && cargo +{{fuzz_toolchain}} fuzz run {{target}} -- -max_total_time={{seconds}} -timeout=30

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

# The locale leg on macOS (CI's `checks` job runs it under TZ=America/Los_Angeles and
# LC_ALL=fr_FR.UTF-8; DEV-141): every test but the fixture tree's timed rebuild
# (P0-TEST-01), which reads no locale and takes about a minute and a half on a macOS
# runner. The same packages as `just test`: selecting fewer would resolve other features
# and build the tests again.
test-locale:
    cargo test --workspace -- --skip p0_test_01_tree_builds_and_reports_its_time --skip p0_test_01_rebuild_lists_identically

# The `pixels` feature on its toolchain (CI's `pixels` job): clippy over duscope-treemap, the
# one crate with code behind the feature; the binary built with it; and the feature's tests,
# which run the probe against mocked terminals (Kitty graphics, Sixel, iTerm2, none, silent)
pixels:
    cargo +{{pixels_toolchain}} clippy --locked -p duscope-treemap --features pixels --all-targets --no-deps -- -D clippy::all
    cargo +{{pixels_toolchain}} build --locked -p duscope --features pixels
    cargo +{{pixels_toolchain}} test --locked -p duscope-treemap --features pixels -- p4_tui_07_
    cargo +{{pixels_toolchain}} test --locked -p duscope --features pixels --test pixels --test pty -- p4_tui_07_ p4_tui_03_

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
