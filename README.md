# duscope

> [!NOTE]
> **v0.2.0, saved scans.** duscope scans, saves each scan as a SQLite index and
> answers `top`, `tree`, `by-type`, `stale`, `caches`, `summary` and `explain`
> from it in milliseconds, compares two scans (`diff`) and takes read-only SQL
> (`query --sql`); it prints one JSON envelope, exports to ncdu, GrandPerspective
> and QDirStat, imports ncdu's, gdu's and GrandPerspective's files, and renders
> the treemap in the terminal, as SVG or as PNG. The MCP server and the interactive treemap arrive in later
> releases, as the list below marks.

duscope is a disk-usage analyzer for macOS and Linux. It scans a directory tree
once, in parallel, and:

- draws the tree as a coloured treemap in the terminal (static now; interactive
  later), or renders it as SVG or PNG;
- answers the questions that free space: the largest directories and files,
  large files untouched for months, bytes by file type, regenerable caches, and
  what deleting a path would free;
- keeps each scan in a SQLite index with precomputed subtree totals, answers
  from it in milliseconds, compares two scans and takes read-only SQL;
- prints one versioned JSON envelope, sized to fit the output limits of scripts
  and AI agents;
- will serve the same read-only operations to agents as a stdio MCP server
  (v0.3).

Its sizes are what deleting would free: hard links count once on every
filesystem, and it accounts for APFS clones and firmlinks on macOS and for btrfs
and XFS reflinks, subvolumes and bind mounts on Linux. Linux and macOS are both
first-class, with one binary, one set of commands and one JSON format.

## A tour

The first command scans the tree and saves the scan in the index directory
(`$XDG_CACHE_HOME/duscope`, else `~/Library/Caches/duscope` on macOS and
`~/.cache/duscope` elsewhere; `--index-dir` names another); the summary line
names it. The commands after it answer from that saved scan, as every
command does when a saved scan holds its path: an answer then takes
milliseconds, however large the tree. `--no-save` scans without saving, `--scan`
names another saved scan, and `scan` (or `duscope` alone) always scans afresh.

The output below is from the fixture tree duscope's tests build (`just
fixtures`), a made-up home directory, on macOS (APFS); a terminal shows it in
colour. On Linux some allocated sizes differ, and the thread count at the end of
the first summary line is the machine's default. Every scan gets an id of its
own, so yours is not `1fd3`.
Paths in the home directory are shortened to `~` here; duscope prints them in
full.

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
220.6 MiB allocated · 43,137 files · 8,129 dirs · 0.1 s, 16 threads, saved as 1fd3
```

`tree` shows each entry's share of its parent, here only those of at least 10%:

```console
$ duscope tree ~/Projects --depth 1 --min-pct 10
   Size       %  Path
5.9 MiB  100.0%  ~/Projects
3.5 MiB   59.0%    rust-cli/
1.1 MiB   17.9%    npm-app/
5.9 MiB allocated · 94 files · 137 dirs · saved scan 1fd3 from 2037-01-01 00:00
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
220.6 MiB allocated · 43,137 files · 8,129 dirs · saved scan 1fd3 from 2037-01-01 00:00
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
220.6 MiB allocated · 43,137 files · 8,129 dirs · saved scan 1fd3 from 2037-01-01 00:00
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

`scans` lists the saved scans; here the one the first command saved:

```console
$ duscope scans --json | jq -c '.entries[] | {root, entry_count, source}'
{"root":"~","entry_count":51266,"source":"scan"}
```

`explain` says what one path is and what deleting it would free, here a
`node_modules` directory, with the command that builds it again:

```console
$ duscope explain ~/dev/node_modules --json | jq -c '.result | {kind, allocated_bytes, freeable_now_bytes, regenerable_by, confidence}'
{"kind":"dir","allocated_bytes":18247680,"freeable_now_bytes":18247680,"regenerable_by":"npm install","confidence":"high"}
```

`query --sql` runs one read-only `SELECT` over the saved scan's tables
([docs/index-format.md](docs/index-format.md); `duscope schema` lists them),
here the extensions holding the most data:

```console
$ duscope query ~ --plain --sql "SELECT human_size(sum(apparent)) AS size, count(*) AS files, ext FROM files WHERE ext != '' GROUP BY ext ORDER BY sum(apparent) DESC, ext LIMIT 4"
size	files	ext
17.1 MiB	147	mov
8.0 MiB	2	mkv
7.1 MiB	129	mp4
3.0 MiB	1	xip
```

`diff` compares two saved scans of one tree, by id: what appeared, went, grew
and shrank, the largest change first. After a week's work on the home
directory, for example:

```sh
duscope scan ~ --save          # saved as 7a2c
duscope diff 1fd3 7a2c
```

`duscope` alone, or `duscope scan ~`, draws the treemap: a box per folder, a
tile per file, coloured by class, with the legend and the next commands below.
`duscope render ~ -o home.svg` writes it as SVG (`--format png` for PNG), and
`duscope export ~ --format ncdu -o home.json` writes the scan for ncdu (or
GrandPerspective's `.gpscan`, QDirStat's cache or CSV), and `duscope import`
saves a tree that ncdu, gdu or GrandPerspective wrote. `duscope man` prints the manual page, and `duscope completions bash`
(or zsh, fish, elvish, powershell) the completion script.

## What it never does

- It never follows a symbolic link, unless given `--follow-symlinks`.
- It never reads a file's contents, only names and metadata (and, for `--exclude-caches`, the 43-byte signature of a `CACHEDIR.TAG`). It does read the system's own description of the disk's layout: `/usr/share/firmlinks` on macOS, `/proc/self/mountinfo` on Linux.
- It never writes inside the tree it scans.
- It never deletes without being told: no release yet has a command that deletes, and the `clean` of a later release is a dry run unless given `--yes`, and moves files to the Trash by default.
- It never reads an index file from outside its index directory unless `--trust` names it: an index file may come from anyone ([SECURITY.md](SECURITY.md)).
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
