# duscope

> [!WARNING]
> **Pre-release.** duscope does nothing useful yet: today the binary only prints
> its version (`duscope --version`). The rest of this page describes what it is
> being built to do, and every install channel below is marked with the release
> that opens it.

duscope is a disk-usage analyzer for macOS and Linux. It scans a directory tree
once, in parallel, into a SQLite index with precomputed subtree totals. From that
index it is built to:

- draw the tree as a coloured treemap in the terminal, static or interactive;
- answer questions in milliseconds: the largest directories and files, large
  files untouched for months, bytes by file type, regenerable caches, what
  changed between two scans, and read-only SQL;
- print one versioned JSON envelope, sized to fit the output limits of scripts
  and AI agents;
- serve the same read-only operations to agents as a stdio MCP server.

Its sizes are what deleting would free: hard links count once on every
filesystem, and it accounts for APFS clones and firmlinks on macOS and for btrfs
and XFS reflinks, subvolumes and bind mounts on Linux. Linux and macOS are both
first-class, with one binary, one set of commands, one JSON format and one index
format. duscope never deletes anything unless a person confirms it.

A short tour of the commands arrives with the first usable release, v0.1.0.

## Install

GitHub Releases and crates.io carry v0.0.1, the pre-release placeholder. The
other channels open with the release each one names.

### GitHub Releases

Prebuilt binaries for macOS (Apple silicon and Intel) and Linux (x86_64 and
arm64, statically linked with musl), built by cargo-dist with build
attestations. The shell installer picks the archive for the machine and
installs `duscope` into `~/.cargo/bin`:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/kawadeomkar/duscope/releases/latest/download/duscope-installer.sh | sh
```

To check a downloaded archive was built by this repository's release workflow:

```sh
gh attestation verify duscope-aarch64-apple-darwin.tar.xz --repo kawadeomkar/duscope
```

### Homebrew (not yet live: from v0.1.0)

On macOS and Linux, with bottles, from the tap `kawadeomkar/homebrew-tap`:

```sh
brew install kawadeomkar/tap/duscope
```

Once duscope meets homebrew-core's notability bar, the formula moves to
homebrew-core and `brew install duscope` works without the tap.

### Debian and Ubuntu

`.deb` packages for amd64 and arm64 (not yet live: from v0.1.0), attached to
each GitHub release. They install one static binary, which runs on any glibc:

```sh
sudo apt install ./duscope_<version>_amd64.deb
```

A signed apt repository (not yet live: phase 5, before 1.0) will then make it
`sudo apt install duscope` on Debian 12 and later and Ubuntu 22.04 and later,
once the repository is added as `docs/apt.md` will describe.

### crates.io

```sh
cargo install duscope --locked
```

Cargo builds it with its default release settings: duscope's release profile
(link-time optimisation, symbols stripped) applies only to builds inside this
repository, so the binary is larger than the prebuilt ones.

### From source

This works today, and builds the pre-release placeholder. It needs
[rustup](https://rustup.rs), which installs the toolchain pinned in
`rust-toolchain.toml`:

```sh
git clone https://github.com/kawadeomkar/duscope
cd duscope
rustup toolchain install
cargo build --release
./target/release/duscope --version
```

## Documentation

- [CONTRIBUTING.md](CONTRIBUTING.md): setup, the tools, the `just` recipes and
  the pull-request workflow.
- [SECURITY.md](SECURITY.md): how to report a vulnerability.
- [CHANGELOG.md](CHANGELOG.md): what changed in each release.
- [docs/](docs/README.md): the rest of the documentation.

## Licence

duscope is licensed under either of [MIT](LICENSE-MIT) or
[Apache-2.0](LICENSE-APACHE), at your option.
