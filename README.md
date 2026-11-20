# duscope

> [!NOTE]
> **v0.3.0, agents.** `duscope mcp` serves eleven read-only tools to MCP clients
> over stdio. They scan a directory, list the saved scans and answer `summary`,
> `top`, `by-type`, `stale`, `caches`, `explain`, `diff`, `schema` and read-only
> SQL from them, each in its verb's JSON envelope, and reach nothing outside the
> directories given to `--roots`; `--allow-delete` adds a twelfth, which moves
> entries to the Trash. `duscope agent --skill` writes the skill an agent's client
> loads: in the project's evaluation, agents used 29.5% fewer tokens with it
> ([bench/README.md](bench/README.md#the-agent-eval)). v0.3.0 also carries the
> interactive viewer (`duscope view`) and `clean`, held to their own gate in v0.4,
> and, on macOS, rescans that read only what changed.

duscope is a disk-usage analyzer for macOS and Linux. It scans a directory tree
once, in parallel, and:

- draws the tree as a coloured treemap in the terminal, as a static map or in
  an interactive viewer, or renders it as SVG or PNG;
- answers the questions that free space: the largest directories and files,
  large files untouched for months, bytes by file type, regenerable caches, and
  what deleting a path would free;
- keeps each scan in a SQLite index with precomputed subtree totals, answers
  from it in milliseconds, compares two scans and takes read-only SQL;
- prints one versioned JSON envelope, sized to fit the output limits of scripts
  and AI agents;
- serves the same read-only operations to agents as a stdio MCP server.

Its sizes are what deleting would free: hard links count once on every
filesystem, and it accounts for APFS clones and firmlinks on macOS and for
subvolumes and bind mounts on Linux. On Linux a scan counts each btrfs or XFS
reflinked copy in full, as `du` does, and `duscope explain --clones` measures
what they share ([limits](docs/limits.md#reflinks-on-linux)). Linux and macOS
are both first-class, with one binary, one set of commands and one JSON format.

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

On macOS and Linux from v0.1.0, from the tap `kawadeomkar/homebrew-tap`, with
the man page and the shell completions. Homebrew pours a bottle where the tap
has one for the machine (Apple-silicon macOS, x86_64 Linux) and otherwise
builds duscope from source, with its own Rust:

```sh
brew install kawadeomkar/tap/duscope
```

Once duscope meets homebrew-core's notability bar, the formula moves to
homebrew-core and `brew install duscope` works without the tap.

### Debian and Ubuntu

`.deb` packages for amd64 and arm64, attached to each GitHub release from
v0.1.0. They install one statically linked binary (musl), which needs no
particular glibc, with the man page and the bash, zsh and fish completions:

```sh
sudo apt install ./duscope_<version>_amd64.deb
```

The signed apt repository, `https://kawadeomkar.github.io/duscope/apt`, serves
the same packages, so `sudo apt install duscope` installs duscope on Debian 12
and later and Ubuntu 22.04 and later, and `apt upgrade` keeps it up to date.
[docs/apt.md](docs/apt.md) has the commands that add it and its key's
fingerprint. It is not live yet: it opens once its signing key exists (phase 5,
before 1.0), with the first release after that.

### crates.io

```sh
cargo install duscope --locked
```

It needs Rust 1.89 or later and a C compiler, which builds the SQLite duscope
bundles. Cargo builds it with its default release settings: duscope's release
profile (link-time optimisation, symbols stripped) applies only to builds
inside this repository, so the binary is larger than the prebuilt ones. Cargo
installs no man page or completions: `duscope man` and `duscope completions`
print them.

The `pixels` feature adds `--treemap pixels`: the map drawn in pixels on a
terminal that shows images through Kitty graphics, Sixel or iTerm2's inline
images, asked for only when the flag is given. No prebuilt package has it,
and it needs Rust 1.90 or later:

```sh
cargo install duscope --locked --features pixels
```

Inside tmux the map stays in cells, with a warning: asking the terminal
through tmux would mean running `tmux set -p allow-passthrough on`, which
changes the pane's settings for every program after duscope, and duscope
runs no other program.

### From source

It needs [rustup](https://rustup.rs), which installs the toolchain pinned in
`rust-toolchain.toml`, and a C compiler:

```sh
git clone https://github.com/kawadeomkar/duscope
cd duscope
rustup toolchain install
cargo build --release
./target/release/duscope --version
```

## A tour

The first command scans the tree and saves the scan in the index directory
(`$XDG_CACHE_HOME/duscope`, else `~/Library/Caches/duscope` on macOS and
`~/.cache/duscope` elsewhere; `--index-dir` names another); the summary line
names it. The commands after it answer from that saved scan, as every
command does when a saved scan holds its path: an answer then takes
milliseconds, however large the tree. `--no-save` scans without saving, `--scan`
names another saved scan, and `scan` (or `duscope` alone) scans afresh, unless
`--reuse AGE` lets it print a saved scan younger than that.

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

## The interactive viewer

`duscope view ~` opens the treemap in an interactive viewer, on the newest
saved scan that holds the path (or on a new scan, which it saves), and
`duscope scan ~ -i` scans first. It needs a terminal of at least 80×24, and
gives the terminal back as it found it when it ends.

The map fills the screen; from 110 columns a column at its right holds the
selection's facts, its siblings and the legend of the colours. The selection
starts on the file reached by taking the largest entry at every level. The line
at the bottom shows six keys for what is in focus, and `?` lists them all, as
this table does (`⏎` is Enter, `⌫` Backspace and `⇥` Tab):

| Keys | What they do |
| --- | --- |
| `←↓↑→` `hjkl` | Move to the neighbouring tile, or along a list or the pane in focus |
| `⏎` | Go down the path; in the largest files, show the file on the map; in the types and the legend, keep that row's tiles on the map |
| `⌫` | Go up the path |
| `Esc` | Close the facts, the help, the prompt or the mark pane; else clear the filter; else go up the path |
| `z` | Zoom the map to the folder under the cursor |
| `Z` | Zoom out one step |
| `0` | Zoom all the way out |
| `g` `Home` | The first sibling, or the first row |
| `G` `End` | The last sibling, or the last row |
| `PgUp` `PgDn` | A page of siblings or rows |
| `1` `2` `3` `4` | The views: the treemap, the folder as a list, the largest files, the file types |
| `s` | The next sort of the siblings and the list: size, name, modified, count |
| `t` | Labels on the tiles of at least 8×2 cells, on or off |
| `c` | The next colour mode: by type, folder, level, age or `LS_COLORS` |
| `p` | The next theme pack |
| `/` | Filter the map, the list and the largest files by a glob, such as `*.zip` |
| `⇥` | Focus the next pane: the map, the siblings, the legend |
| `Shift-⇥` | Focus the previous pane |
| `i` | The selection's full facts, with what deleting it would free |
| `y` | Copy the selection's path to the clipboard |
| `o` | Show the selection in the file manager |
| `d` | Mark the selection for the Trash, or unmark it |
| `D` | The mark pane: the plan for the marked paths, as `duscope clean` makes it |
| `r` | Scan the viewer's root again and reload |
| `?` | List the keys |
| `q` `Ctrl-C` | Quit |

In the mark pane, `yes` typed in full, then `⏎`, moves the marked paths to the
Trash; any other answer moves nothing. The viewer never deletes, and a plan that
refuses a path moves nothing. The manual page (`duscope man`) describes the
viewer and `clean` in full.

## With an AI agent

`duscope agent --skill` writes a skill that Claude Code and other agents' clients load by
themselves when a question is about disk space: the briefing an agent reads before it uses
duscope (which verb answers which question, the handle `clean` checks, the exit and error codes,
the SQL tables), in `.agents/skills/duscope/` and `.claude/skills/duscope/` under the working
directory or `--dir`. The skill is recommended: in the project's evaluation, 120 runs of ten
disk-hygiene tasks through Claude Code, agents completed every task with it and without it, and
used 29.5% fewer tokens with it ([bench/README.md](bench/README.md#the-agent-eval),
[the results](bench/eval/results/2026-11-16.json)). `duscope agent` prints the same briefing on
stdout, and run by an agent's client every verb writes JSON.

`duscope mcp` serves scans to MCP clients over stdio, and they reach nothing
outside the directories given to `--roots` (its tools need v0.3.0 or later:
v0.2.0's server offers none). Each client takes one line:

```sh
claude mcp add --transport stdio duscope -- duscope mcp --roots ~
codex mcp add duscope -- duscope mcp --roots ~
gemini mcp add --scope user duscope duscope mcp -- --roots ~
```

[docs/mcp.md](docs/mcp.md) lists the tools and has a Claude Code hook that asks
before `duscope clean` runs. The server is read-only by default; `--allow-delete`
adds `duscope_delete`, which can only move entries to the Trash.

## What it never does

- It never follows a symbolic link, unless given `--follow-symlinks`.
- It never reads a file's contents, only names and metadata, with three exceptions: the 43-byte signature of a `CACHEDIR.TAG` (for `caches` and `--exclude-caches`); for `explain`, the files git reads to tell whether it tracks or ignores a path (the ignore files, the configuration and the repository's index); and the system's own description of the disk's layout, `/usr/share/firmlinks` on macOS and `/proc/self/mountinfo` on Linux.
- It never writes inside the tree it scans.
- It never deletes without being told: `clean` is a dry run unless given `--yes` (or `yes` typed at its question), moves files to the Trash unless given `--permanent`, and refuses a symbolic link, a mount point and the scanned tree's root.
- It never reads an index file from outside its index directory unless `--trust` names it: an index file may come from anyone ([SECURITY.md](SECURITY.md)).
- It never opens a network connection: no telemetry, no update checks.

## macOS and Linux

The commands, the flags and the JSON are the same on both. What duscope can
see is not, where the systems differ.

On macOS:

- **Full Disk Access.** The protected folders of `~/Library` are read only when
  the app that runs duscope (the terminal, or the client that starts `duscope
  mcp`) has Full Disk Access; otherwise they count as unreadable, with the
  warning `full_disk_access_missing`
  ([troubleshooting](docs/troubleshooting.md#full_disk_access_missing),
  [limits](docs/limits.md#full-disk-access-is-a-heuristic-macos)).
- **APFS clones** count once, unless `--no-clones`; a clone written to since
  makes the totals above it an upper bound
  ([limits](docs/limits.md#apfs-clones-written-to-since-they-were-cloned)).
- **The volume group.** A scan of `/` reads the Data volume once, through the
  firmlinks, and `-x` tells the group's volumes apart by their own devices
  (CAVEATS in `duscope man`;
  [limits](docs/limits.md#scanvolumes-and-a-firmlink-below-the-path-macos)).
- **`~/Library/Containers`** can take minutes to read; `--skip-containers`
  leaves it and `Group Containers` out
  ([limits](docs/limits.md#librarycontainers-can-be-slow-macos)).
- **`rescan`** reads again only what the volume's FSEvents history reports
  changed, and the whole tree when it cannot trust the history
  ([limits](docs/limits.md#fsevents-history-is-advisory-macos-rescans)).
- **Cloud placeholders** (iCloud Drive, Dropbox, OneDrive) take no space on the
  disk ([`dataless_present`](docs/troubleshooting.md#dataless_present)), and
  **Time Machine's local snapshots** hold what a deletion frees until they go
  ([limits](docs/limits.md#snapshots)).
- **The Trash** is the Finder's, on the item's volume (CLEANING UP in `duscope
  man`).

On Linux:

- **Reflinks** (`cp --reflink`, btrfs and XFS) count in full, as `du` counts
  them, and `duscope explain --clones` measures what they share
  ([limits](docs/limits.md#reflinks-on-linux)).
- **`rescan`** reads the whole tree, since Linux keeps no change history
  ([limits](docs/limits.md#a-linux-rescan-reads-the-whole-tree)).
- **Automount points** the kernel has not mounted are listed and not entered
  ([limits](docs/limits.md#automount-points-linux), CAVEATS in `duscope man`).
- **Subvolumes.** No flag keeps a scan inside one btrfs subvolume, and `clean`
  takes a subvolume for a mount point
  ([limits](docs/limits.md#no---one-subvolume-linux),
  [limits](docs/limits.md#a-btrfs-subvolume-is-a-mount-point-to-clean-linux)).
- **Names that are not UTF-8** keep their bytes in `path_raw` and `name_raw`
  ([limits](docs/limits.md#paths-that-are-not-utf-8-linux)).
- **Access times.** Linux mounts with `relatime` by default, under which
  `stale --atime` can take a file read lately for one left untouched
  ([`atime_unreliable`](docs/troubleshooting.md#atime_unreliable)).
- **The Trash** is the freedesktop one, `~/.local/share/Trash` or the volume's
  own; on overlayfs, NFS, 9p and FUSE it has limits of its own
  ([limits](docs/limits.md#overlayfs-what-the-trash-cannot-take-linux),
  [limits](docs/limits.md#the-trash-on-nfs-9p-and-fuse-linux)).

## Documentation

- [CONTRIBUTING.md](CONTRIBUTING.md): setup, the tools, the `just` recipes and
  the pull-request workflow.
- [SECURITY.md](SECURITY.md): the threat model (what duscope treats as untrusted and
  the rule for each), how to report a vulnerability, and the supported versions.
- [CHANGELOG.md](CHANGELOG.md): what changed in each release.
- [docs/troubleshooting.md](docs/troubleshooting.md): every warning and error,
  and what to do about it.
- [docs/limits.md](docs/limits.md): what duscope cannot measure, or measures
  only within bounds, and why.
- [docs/](docs/README.md): the rest of the documentation.

## Licence

duscope is licensed under either of [MIT](LICENSE-MIT) or
[Apache-2.0](LICENSE-APACHE), at your option.
