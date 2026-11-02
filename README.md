# duscope

> [!NOTE]
> **v0.1.0, the first usable release.** duscope scans, answers `top`, `tree`,
> `by-type` and `stale` from the scan, prints one JSON envelope and draws a
> treemap in the terminal. The index and its history, the MCP server and the
> interactive treemap arrive in later releases, as the list below marks.

duscope is a disk-usage analyzer for macOS and Linux. It scans a directory tree
once, in parallel, and:

- draws the tree as a coloured treemap in the terminal (static now; interactive
  later);
- answers the questions that free space: the largest directories and files,
  large files untouched for months, bytes by file type;
- prints one versioned JSON envelope, sized to fit the output limits of scripts
  and AI agents;
- will keep each scan in a SQLite index with precomputed subtree totals, answer
  from it in milliseconds, compare two scans and take read-only SQL (v0.2), and
  serve the same read-only operations to agents as a stdio MCP server (v0.3).

Its sizes are what deleting would free: hard links count once on every
filesystem, and it accounts for APFS clones and firmlinks on macOS and for btrfs
and XFS reflinks, subvolumes and bind mounts on Linux. Linux and macOS are both
first-class, with one binary, one set of commands and one JSON format.

## A tour

Each command scans the tree it is given and answers from that scan. The output
below is from the fixture tree duscope's tests build (`just fixtures`), a
made-up home directory, on macOS (APFS); a terminal shows it in colour. On Linux
some allocated sizes differ, and the thread count at the end of the summary
line is the machine's default. Paths in the home directory are shortened to `~`
here; duscope prints them in full.

`top` lists the largest directories. A directory holding 80% of its parent's
bytes is cut through to the child that holds them, so the row says where the
space is:

```console
$ duscope top ~ --limit 6
    Size      %   Files  Path
79.3 MiB  36.0%  24,859  ~/Library
54.3 MiB  24.6%   8,604  ~/dev
30.0 MiB  13.6%       8  ~/Movies
10.5 MiB   4.8%     250  ~/Music
10.0 MiB   4.5%   2,001  ~/Documents
 7.9 MiB   3.6%   4,000  ~/.cache
220.6 MiB allocated · 43,137 files · 8,129 dirs · 0.1 s, 16 threads
```

`tree` shows each entry's share of its parent, here only those of at least 10%:

```console
$ duscope tree ~/Projects --depth 1 --min-pct 10
   Size       %  Path
5.9 MiB  100.0%  ~/Projects
3.5 MiB   59.0%    rust-cli/
1.1 MiB   17.9%    npm-app/
5.9 MiB allocated · 94 files · 137 dirs · 0.0 s, 16 threads
```

`by-type` adds bytes up by class; a directory rule wins over the extensions
inside it (`node_modules` is code, whatever it holds):

```console
$ duscope by-type ~
    Size      %   Files  Class
54.7 MiB  24.8%  18,988  data
50.9 MiB  23.1%   1,704  media
44.0 MiB  20.0%   4,055  binaries
24.5 MiB  11.1%   6,277  code
15.1 MiB   6.8%   4,932  docs
 7.4 MiB   3.3%     197  archives
23.9 MiB  10.9%   6,983  other
220.6 MiB allocated · 43,137 files · 8,129 dirs · 0.1 s, 16 threads
```

`stale` finds large files neither modified nor changed for a while:

```console
$ duscope stale ~ --older-than 1y --min-size 1M
   Size    Modified  Path
7.0 MiB  2035-08-20  ~/Movies/holiday-2035.mov
6.0 MiB  2034-01-02  ~/Movies/wedding.mp4
4.0 MiB  2035-01-02  ~/Movies/lecture-01.mkv
4.0 MiB  2035-01-02  ~/Movies/lecture-02.mkv
3.0 MiB  2035-01-02  ~/Downloads/Xcode_16.1.xip
1.5 MiB  2034-10-24  ~/Downloads/ubuntu-24.04.1-live-server-arm64.iso
220.6 MiB allocated · 43,137 files · 8,129 dirs · 0.2 s, 16 threads
```

Scripts take `--plain` (tab-separated, sizes in bytes) or `--json`, the
envelope every verb shares; agents get JSON without asking. Nothing but the
payload reaches stdout, and the summary line goes to stderr (the two blocks
below leave it out):

```console
$ duscope top ~ --files --limit 3 --plain
7340032	7340032	1	0	2035-08-20T00:00:00Z	file	~/Movies/holiday-2035.mov
6291456	6291456	1	0	2034-01-02T00:00:00Z	file	~/Movies/wedding.mp4
5242880	5242880	1	0	2036-12-12T00:00:00Z	file	~/Movies/screen-recording.mov
```

The envelope is one JSON object: `"ok": true`, the scan and the query, and the
rows in `entries`, here picked out with jq:

```console
$ duscope top ~ --files --limit 2 --json | jq -c '.entries[] | {path, kind, allocated_bytes}'
{"path":"~/Movies/holiday-2035.mov","kind":"file","allocated_bytes":7340032}
{"path":"~/Movies/wedding.mp4","kind":"file","allocated_bytes":6291456}
```

`duscope` alone, or `duscope scan ~`, draws the treemap: a box per folder, a
tile per file, coloured by class, with the legend and the next commands below.
`duscope man` prints the manual page, and `duscope completions bash` (or zsh,
fish, elvish, powershell) the completion script.

## What it never does

- It never follows a symbolic link, unless given `--follow-symlinks`.
- It never reads a file's contents, only names and metadata (and, for `--exclude-caches`, the 43-byte signature of a `CACHEDIR.TAG`). It does read the system's own description of the disk's layout: `/usr/share/firmlinks` on macOS, `/proc/self/mountinfo` on Linux.
- It never writes inside the tree it scans.
- It never deletes without being told: v0.1.0 has no command that deletes, and the `clean` of a later release is a dry run unless given `--yes`, and moves files to the Trash by default.
- It never opens a network connection: no telemetry, no update checks.

## Install

GitHub Releases and crates.io carry each release from v0.1.0 (v0.0.1 was the
pre-release placeholder). The other channels open with the release each one
names.

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

### Homebrew

On macOS and Linux, with bottles, from the tap `kawadeomkar/homebrew-tap`:

```sh
brew install kawadeomkar/tap/duscope
```

Once duscope meets homebrew-core's notability bar, the formula moves to
homebrew-core and `brew install duscope` works without the tap.

### Debian and Ubuntu

`.deb` packages for amd64 and arm64, attached to each GitHub release from
v0.1.0. They install one static binary, which runs on any glibc:

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

It needs [rustup](https://rustup.rs), which installs the toolchain pinned in
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
- [SECURITY.md](SECURITY.md): the threat model (what duscope treats as untrusted and
  the rule for each), how to report a vulnerability, and the supported versions.
- [CHANGELOG.md](CHANGELOG.md): what changed in each release.
- [docs/](docs/README.md): the rest of the documentation.

## Licence

duscope is licensed under either of [MIT](LICENSE-MIT) or
[Apache-2.0](LICENSE-APACHE), at your option.
