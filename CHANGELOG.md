# Changelog

All notable changes to duscope are recorded in this file. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Changes are not written here directly. Each pull request with a user-visible
change adds a fragment, `changelog.d/<ID>.md`, in the format given in
[changelog.d/README.md](changelog.d/README.md). At each release
`scripts/changelog.py` assembles the fragments into the version's dated section
and deletes them.

## [0.2.0] - 2026-11-04

### Added

- Every verb's `--json` envelope is described by a JSON Schema under `schemas/envelope/` (`<verb>.schema.json`, with `error.schema.json` for a failure), listing the warning codes, the error codes and the values of every enumerated field. `schemas/envelope/history/` keeps each published version.
- `next` lists up to three commands to run on the same scan, most useful first: the next page when rows remain, then the largest entry to `explain` and the related verbs.
- `export [path] --format F` writes a saved scan (or one run for it) for other tools: `ncdu` (JSON that `ncdu -f` opens), `gpscan` (GrandPerspective's gzip XML; with `-o FILE.gpscan` or into a pipe), `qdirstat` (QDirStat's cache file, with owners and modes: a scan run for it collects them), or `ndjson` and `csv`, a row an entry with its path, kind, sizes, link count, time and a directory's totals. `--size apparent` exports apparent sizes where the format has one measure.
- `import FILE` saves a tree another tool wrote as a scan: ncdu's JSON export, gdu's (whose directory totals are added up again from the entries), or GrandPerspective's `.gpscan`, gzipped or not, from a file or stdin (`-`). `--format ncdu|gdu|gpscan` names the format, or the file shows it. The saved import's `source` is `import:<format>`; `--scan` reads it, and `diff` sets it against a scan of this machine, but `latest` never names it, so no verb reads it in place of a scan. Gzip is read up to 1 GiB of decompressed data.
- `explain --json` returns a `handle`: the path as `explain` found it (device, inode, bytes on disk, the scan) with a fifteen-minute expiry, signed with HMAC-SHA256 by a key that duscope makes on first use in its cache directory (`handle.key`, 32 random bytes, mode 0600; deleting it invalidates every outstanding handle). `clean --from` acts only on what a handle names. A key that others may read issues no handle.
- A configuration file, `~/.config/duscope/config.toml` (or `--config FILE`, `DUSCOPE_CONFIG`; `--config none` reads none), with flag defaults under `[defaults]`, `[excludes] patterns` for every scan, and `[classes]`/`[caches]` extra tables; flags and their environment variables win over it.
- `-v` prints the walk's and the roll-up's timings to stderr, `-vv` each volume and the worker count too, and `--debug` each directory's errors as they happen; `DUSCOPE_LOG` (or `RUST_LOG`) filters the log in `RUST_LOG`'s grammar.
- `-q` keeps warnings off stderr.
- `scan --save` saves the scan as an index, one SQLite file under `$XDG_CACHE_HOME/duscope/scans/` (`~/Library/Caches/duscope` on macOS when unset), and prints its id (`scan.id` in the envelope); `--index-dir` or `DUSCOPE_INDEX_DIR` names another directory. The index is written after the output, all at once and renamed into place; if it cannot be written, duscope says so on stderr and exits 1, and the output stands.
- `-v` reports the index write.
- `top`, `tree`, `by-type` and `stale` read the newest saved scan that holds their path, taken with the same options, instead of scanning again; with none, they scan, save the scan and show its id; if it cannot be saved, the output stands and duscope exits 1. `--no-save` keeps that scan unsaved, and `--no-scan` fails instead of scanning. `--scan` names a saved scan: `latest`, an id or its first four or more digits, or an index file. A saved scan older than `--max-age` (7 days, or `[defaults] max_age`) is still read, with a `stale_index` warning.
- `scans` lists the saved scans; `scans --prune` removes all but the five newest of each root and those under seven days old, and the temporary files of writes that never finished; `scans --check` checks a saved scan's file for damage.
- A saved scan that cannot be read exits 5 with `index_corrupt`, and one written by a version this duscope cannot read with `index_version_mismatch`.
- `rescan [path]` scans a saved scan's root again with the options it was scanned with (excludes, `-x`, clones, links, attributes, threads, size), and saves the result as a new index whose `parent_scan_id` names the old one, which stays until `scans --prune`; queries then read the new one. `--scan` names the saved scan; `--full` is what every rescan does until the incremental one arrives, and `--incremental` answers `not_available` until then.
- `render [path]` draws the treemap as an SVG picture at any size (`--width`, `--height`; 1200 × 800 by default), to stdout or `-o FILE.svg`: tiles shaded with GrandPerspective's gradient, labelled with names and sizes where they fit, with the legend and the totals under the map. `--layout squarify` (the default) or `gp` (GrandPerspective's bisection, files cut from subfolders), `--color-by`, `--theme`, `--depth` and `--pool-files` work as for the terminal map.
- `render --format png` (or `-o FILE.png`) draws the treemap as a PNG, at most 64 Mi pixels: each tile shaded pixel for pixel by GrandPerspective's gradient, labels in a bundled subset of the Inter font (SIL Open Font License 1.1) that holds ASCII and Latin-1, so other characters show as boxes in a PNG (the SVG uses the browser's fonts). duscope rasterises and encodes it itself, with no graphics library.
- On a case-insensitive volume (APFS, HFS+, exFAT), a path typed in another case finds its directory in a saved scan, as does a name typed composed (`é`) that the filesystem stored decomposed.
- `top`, `stale` and `caches` page: a list `--limit` cuts carries `next_cursor`, and `--cursor` returns the rows after it from the same saved scan. A cursor of another scan or query is refused with `invalid_cursor`.
- `--fields a,b,c` writes those fields of each row, in that order, under `--json`, `--format ndjson` and `--plain`; `--fields help` lists a verb's fields, and an unknown name is refused with `unknown_field`. `--detail` writes every field.
- `-o FILE` writes the output to a file, with no 100-row cap on `--limit`.
- A JSON envelope longer than `--max-chars` (24,000 characters by default; 0 never) goes to a file in the index directory's `out/`, and stdout gets the file's path with the first five rows. `scans --prune` removes such files after a week.
- `--all` streams every row of a list, as ndjson unless `--plain` or `--format` says otherwise.
- On a saved scan, `top`, `top --files`, `stale` and `by-type` answer in milliseconds: on a tree of a million entries, a few milliseconds each.
- `summary [path]` gives a scan's totals under a path: bytes on disk, apparent bytes and bytes as `du` counts them (clones each time), each volume's share with its size and free space at the scan's start, the logical size of dataless placeholders, the counts of files, directories, errors and vanished entries, and when and how the scan ran. Where the path is a mount point, `unaccounted_bytes` says what the volume uses that the scan did not find.
- `explain <path>` gives the facts about one path from the newest saved scan that holds it: its size and times, its links and how many lie outside it, its clone group, the bytes deleting it would free now (exact, or a lower or upper bound), the command that regenerates a cache directory and whether it holds a valid `CACHEDIR.TAG`, what git says of it (ignored, precious, tracked files inside), how sure each of these is, and where it came from. A path no saved scan holds is answered from a fresh `stat`, with low confidence.
- `explain --errors <scan>` lists the directories and entries a saved scan could not read.
- `caches [path]` lists the regenerable cache directories under a path, largest first: `node_modules`, `target`, `.venv`, `build`, `Pods`, `vendor` and the others of the cache rules, each confirmed by its marker file in an ancestor (`package.json`, `Cargo.toml`, …), Xcode's `DerivedData`, and any directory holding a valid `CACHEDIR.TAG`. Each row names the command that regenerates the cache and its project, the nearest directory holding the marker. A cache inside a cache is part of the outer one, and a marker beyond a directory of the rule's own name confirms that directory, not the ones inside it.
- `caches --by-project` gives one row per project, with its caches' bytes and the newest modification time anywhere in the project; `--min-size` (10 MiB by default) then applies to a project's total.
- `caches --loose` also lists a cache's name without the marker that confirms it, marked `"confirmed": false`.
- `diff <old> <new>` lists what changed between two saved scans of one tree, largest change first: entries `added` and `removed` (a whole directory as one entry, with how many it held), files `grown` or `shrunk` by at least `--min-delta` (1 MiB by default), and names whose kind changed. `--depth` compares the directories at that depth whole; `--under` compares a path both scans hold. The envelope adds up the bytes added and removed, the net change and the entries added, removed and changed. Scans of other roots are refused with `incompatible_scans`.
- `query --sql "<SELECT>" [path]` runs one read-only statement over a saved scan's tables and prints its columns and rows (a BLOB as `{"$bytes": …}`; `--plain` as tab-separated values with a heading line). A statement without a `LIMIT` returns 25 rows at most, `--max-rows` (10,000 at most) caps one with a `LIMIT`, and `--timeout` (5 s by default, 60 at most) stops it. The statement runs in a sandbox: the index opens read-only, and writes, `PRAGMA`, `ATTACH`, other tables and functions outside an allow-list are refused with `sql_denied`. `path(id)`, `human_size(bytes)` and `under(id, root_id)` are duscope's own functions, and `:root_id` and `:root_last` name the path's subtree.
- `schema [path]` prints the index's layout for `query --sql`: each table with its columns, their types, whether they may be null and the platforms that fill them, and its rows in the newest saved scan that holds the path (or the one `--scan` names). People get the DDL with each table's rows; `--json` gets `tables[] {name, columns[] {name, type, nullable, platforms}, rows}`.
- `schema --envelope <verb>` prints that verb's envelope as JSON Schema 2020-12; `schema --envelope` alone lists the envelopes.
- `by-type` on a directory inside a saved scan answers from the index rather than reading each entry under it: 44 ms for a directory of 262,000 files, and 18 ms over a whole saved scan of a million entries.
- `--trust FILE`, beside every `--scan` (and for `diff`'s scans, `explain --errors` and `scans --check`), reads an index file outside the index directory. Without it, `--scan FILE` naming one is refused with `untrusted_index` (exit 1), since an index file may come from anyone; the next commands an envelope suggests for such a file carry the `--trust` it needs. A link in the index directory's `scans` is not read as a saved scan.
- `SECURITY.md` gives the threat model: the four untrusted inputs (SQL, agent paths and handles, index files, the scanned tree) and the rule for each, the supported versions and the security-release procedure.
- Every index file opens with its views, triggers and attached databases switched off, statements of at most 64 KiB and values of at most 1 MB. A file whose schema has a virtual table or more than 256 objects, or whose `tool_version` is not a duscope version, is refused as `index_corrupt`.
- duscope warns on stderr when the index directory, its `scans` directory or an index file is open to other users or another user's, with the `chmod` that fixes it.
- `duscope agent` prints a briefing for AI agents, in Markdown: when to reach for duscope, the rules that keep it safe (sizes are bytes, `clean` is a dry run unless `--yes`, never pass `--yes` without a person's word), the flow from `explain` to a handle to `clean`, which command answers which question, every verb and exit code of the binary that prints it and every error code the envelope defines, and SQL examples over a saved scan.
- `duscope agent --skill` writes the briefing as a skill that Claude Code and other agents' clients load on their own: `SKILL.md`, whose description carries the words that should bring duscope to mind (disk space, what is eating my disk, large files, node_modules, stale files, clean up, compare scans), and `references/sql.md` with the SQL examples, in `.agents/skills/duscope/` and `.claude/skills/duscope/` under the working directory or `--dir`. A skill that is already there is replaced only with `--force`.
- `duscope mcp --roots DIR` runs an MCP server on stdin and stdout for AI agents, speaking the 2026-07-28 protocol and the 2025-11-25 `initialize` handshake. A client reaches nothing outside `--roots`: a path holding `..`, a link that leads out, and an index file named for a scan are refused with `refused_path`, and a saved scan whose root lies outside the roots is not found. A tool's result is the envelope its verb prints with `--json`, byte for byte, with a one-line summary; `--max-chars` caps every result as it caps the verbs' output, and `--log-level` sets what reaches stderr. The server lists no tools yet: the verbs arrive as tools in a later release.
- `clean PATH… [--from HANDLE…]` moves files and directories to the Trash: on macOS into the volume's Trash through `NSFileManager`, on Linux into the XDG Trash (the home Trash, or the volume's own `.Trash/$uid` or `.Trash-$uid`), by renaming, never by copying. It plans from a saved scan, the newest that holds the first item or `--scan`, and shows the plan first (each item's bytes, what deleting it would free now with links and clone members outside the plan kept, the command that regenerates it) and moves nothing without `--yes`; on a terminal it asks once and acts only on `yes` typed in full; elsewhere, or with `--no-input`, it answers `confirmation_required`. Moving to the Trash frees nothing until the Trash is emptied: the envelope reports `trashed_bytes` and `freed_bytes: 0`.
- `clean` refuses a symbolic link, a mount point, a macOS firmlink, the scan's root (or `/`), a path outside the saved scan and an item on a read-only volume; one refused item makes the whole run a dry run (`refused_path`) unless `--skip-refused`. An item whose volume has no Trash is left where it is (`trash_unavailable`): `clean` never falls back to deleting.
- `--from HANDLE` acts only on what an `explain --json` handle names: the handle must be unexpired and signed by this duscope, and the path must still hold the same device and inode (`path_changed` otherwise).
- `duscope view [path|scan]` opens the treemap in the interactive viewer, from the newest saved scan that holds the path (or the scan named), scanning it otherwise as the list verbs do; `duscope scan -i` scans and opens the result. The viewer takes the terminal (alternate screen, raw mode, mouse) and gives it back as it found it on `q`, `Ctrl-C`, an error or a crash, redraws when the window is resized, and refuses a terminal below 80×24 or a stdout that is not a terminal. Without `--theme` or `COLORFGBG` it asks the terminal for its background and picks the light pack or Mocha.
- The viewer draws the full screen: a header with the `duscope` badge, the scan's path, the view tabs and the scan's totals; a breadcrumb row to the selection; the map in a titled pane, without labels below the top-level folders; beside it the selected pane (size, counts, shares of the parent and of the scan, type, modification time, mode), the siblings pane (the largest nine, then `… n more`) and the legend for the colour mode; and the key line. From 140 columns the right column is 44 wide, from 110 it is 34, and below that the selected pane folds under the map. The selection opens on the largest file.
- `-vv` prints a line for each worker of the walk: the entries it read and its time in the directory reads.
- On Linux, `-v` prints the walk as two lines, `listing:` and `stat:`, that share the walk's time in the proportion the workers spent listing directories and reading entries' attributes.

### Changed

- `schema_version` is `0.2`, and the envelope changes additively only from here.

## [0.1.0] - 2026-10-30

### Added

- `duscope scan [PATH]`, or `duscope [PATH]` alone, scans a tree in
  parallel and reports how much space it takes; `top`, `tree`, `by-type`
  and `stale` run the same scan before they list. Without a path, the
  current directory is scanned.
- Every verb takes the scan flags: `--threads N` (or `DUSCOPE_THREADS`,
  up to 1,024; empty or 0 leaves the choice to the platform),
  `-x`/`--one-file-system`, `--include-network`, `--follow-symlinks`,
  `--size allocated|apparent` (or `--apparent-size`), `--attrs basic|full`
  and `--clones`/`--no-clones`. An APFS clone's shared blocks count once by
  default; `--clones` on a filesystem without clone ids (anything but APFS)
  warns `clones_unavailable`. Sizes given to flags take `K`, `M`, `G` and
  `T` as powers of 1,024 and `KB`, `MB`, `GB` and `TB` as powers of 1,000.
- Exit codes: 0 for success, unreadable directories below the root
  included; 1 for a runtime error; 2 for a usage error; 3 when the path
  does not exist; 4 when the root cannot be read. Closing the pipe duscope
  writes to (`duscope … | head`) ends it with 0 and no error message.
- `--json`, `--plain` and `--format table|plain|json|ndjson` choose how every
  verb writes its result, before or after the verb. Without a flag,
  `DUSCOPE_FORMAT` decides; then an agent's shell (`CLAUDECODE`,
  `CODEX_THREAD_ID`, `CODEX_VERSION`, `GEMINI_CLI`) gets JSON, `CI` gets
  `--plain`, a terminal gets a table, and a pipe gets `--plain`.
- `--json` prints one compact envelope: `scan`, `query`, `entries`,
  `returned_count`, `total_count`, `truncated`, `next_cursor`, `warnings` and
  `next`. Failures print the error object (`code`, `message`, `hint`,
  `retryable`, `fixable_by`). A name that is not UTF-8 adds `name_raw`
  (base64).
- `--format ndjson` writes a `scan` line, one `entry` line per row (every
  row, with no `--limit`), and an `end` line with the counts and warnings.
- `--plain` writes tab-separated records with sizes in bytes and paths as
  their raw bytes; `--header` names the fields and `-0` ends records with NUL.
- `--color auto|always|never` and `--no-color` honour `NO_COLOR`,
  `CLICOLOR_FORCE`, `FORCE_COLOR` and `TERM=dumb`. `--si` shows sizes in
  powers of 1,000, and `-b` shows them in bytes.
- `top [PATH]` lists the largest directories at `--depth` (default 1), with
  the shallower directories that have none below them, so the rows never
  nest. A row whose directory holds a child of at least 80% of its bytes is
  cut through to that child, up to three levels down, and `via` names the
  directories cut through; `--no-cut-through` keeps strict depth. The
  filters judge the rows as listed, after cut-through.
  `--files` lists the largest files instead and marks a later hard link
  `uncharged`.
- `tree [PATH]` lists the entries down to `--depth` (default 2), largest
  first under each directory, with `pct_of_root` and `pct_of_parent`.
  `--min-pct` leaves out what is smaller than that share of its parent.
  `--limit` keeps the shallowest rows, every row of one depth before any
  row below it.
- `by-type [PATH]` adds up bytes and files by class (code, media, archives,
  data, binaries, docs, other), each with its largest files
  (`--examples N`) by path and `name` (and `name_raw` for a name that is
  not UTF-8). A directory rule such as `node_modules` wins over the
  extension of every file beneath it, and `--classes FILE` merges your own
  table over the built-in one.
- `stale [PATH]` lists files whose modification and change times are both
  older than `--older-than` (default 180d) and at least `--min-size`
  (default 10 MiB). `--atime` also requires the access time to be older
  (it needs `--attrs full`) and warns `atime_unreliable` on a `noatime` or
  `relatime` mount.
- Every list verb takes `--glob`, `--regex`, `--invert`, `--exclude`,
  `--type`, `--min-size`/`--max-size`, `--min-depth`/`--max-depth`, and
  `--time` with `--older-than`/`--newer-than` and `--since`/`--until`
  (RFC 3339 or `YYYY-MM-DD`). The filters applied are listed in
  `query.filters`. `--sort size|count|mtime|path` and `-r` set the order,
  and `--strict` makes unreadable directories exit 4. `--limit` returns up
  to 100 rows (25 by default) and refuses more; `--format ndjson` streams
  every row.
- `duscope man` prints the manual page in roff (`duscope man | mandoc -a`
  shows it). It covers every verb's options, the environment variables
  duscope reads, the exit codes, the file formats, examples, and where
  duscope differs from `du` (`-x` inside a macOS volume group,
  `--exclude-caches` markers, case-sensitive patterns), and it is generated
  from the command line itself, so it lists every flag.
- `duscope completions bash|zsh|fish|elvish|powershell` prints the
  completion script for a shell.
- `scan` on a terminal draws a coloured treemap of the tree, nested to file
  level. Depth-1 folders are boxes titled `name · size`, deeper folders are
  boxes or gutter groups, and files are tiles labelled with their name and
  size where they fit. What is too small to draw is pooled into a
  `+N small` tile. A legend of the file classes, the totals, the first
  warning and the next commands follow the map; further warnings follow
  on stderr.
- `--treemap auto|never|always|pixels`: `always` draws at 80×24 when stdout
  is not a terminal, and `pixels` reports `not_compiled` in this build.
  `--depth` caps how deep the map is drawn (a folder at the cap keeps its
  box and title, so `--depth 1` draws one titled box per top-level
  entry), `--min-tile 1|2|4` raises the smallest tile drawn, and
  `--pool-files` gathers each directory's files into one tile.
- `--theme mocha|tokyo-night|dracula|gruvbox|light|terminal-native|no-color`,
  or the path of a TOML theme file (`docs/rules.md`), also through
  `DUSCOPE_THEME`. Without one, a light terminal (`COLORFGBG`) gets the
  light pack and anything else gets Catppuccin Mocha. Colours are 24-bit
  when `COLORTERM` says so and quantised to 256 colours otherwise.
- `--color-by type|folder|level|age|lscolors` chooses what colours the
  tiles. `lscolors` reads the `*suffix=` entries of `LS_COLORS` (`*.png`,
  `*.tar.gz`, `*README`) and `ex=` for executables (under `--attrs full`,
  which collects modes); folders and files no entry matches keep their
  file-class colour, and the other keys (`di=`, `ln=`, `fi=`, …) are not
  read. Under `NO_COLOR` or `--color never`, tiles are `░▒▓█` shades with
  the class letter in each label, and terminals without UTF-8 get `+-|`
  borders and `#=-+` shades.
- `.deb` packages for Linux on amd64 and arm64, attached to each GitHub
  Release: the statically linked binary, the man page, and bash, zsh and fish
  completions. They install on Debian 12 and Ubuntu 22.04 and later with
  `apt install ./duscope_<version>_<arch>.deb`.
- `brew install kawadeomkar/tap/duscope` installs duscope from the Homebrew
  tap, as a bottle on Apple-silicon macOS and on Linux, with the man page
  and shell completions.
- `--follow-symlinks` follows links to files and directories. A link that
  loops, or leads to a directory on its own path, is recorded as an `ELOOP`
  error of the directory holding it, and the scan goes on. A link that leads
  nowhere stays a link.
- Ctrl-C stops a scan within 200 ms with exit 130 and one line on stderr,
  `duscope: interrupted`, and writes nothing else.
- A root given as a symbolic link is scanned as the directory it leads to:
  `scan.root` is the resolved path, `scan.root_as_given` the path as typed.
  One that leads nowhere is an error (exit 3,
  `scan_not_found`), and one that loops is an `io_error`, instead of a
  scan of the link itself.
- Files and directories that disappear while a scan reads them count in
  `scan.vanished_count`, with a `vanished` warning, instead of as errors.
  A directory replaced by a file after it was listed counts as vanished.
- A directory that cannot be read because of `ELOOP`, `ENAMETOOLONG` or
  `EIO` is listed empty and flagged unreadable, as one that cannot be read
  for permission is, and counts in the `partial_read` warning.
- `scan --exclude GLOB` and `--exclude-from FILE` leave out what matches,
  matched against paths below the root. A pattern starting with `/` is
  anchored at the root, any other matches at every depth, `*` stays within
  a component and `**` crosses them, and a trailing `/` matches
  directories only. Patterns match names case-sensitively, also on a
  case-insensitive volume.
- `--ignore-from FILE` reads gitignore-syntax files, negations
  (`!keep.log`), anchors and directory-only patterns included.
- `--exclude-caches` keeps the scan out of cache directories: a name from
  the cache rules whose marker sits beside it (`node_modules` next to
  `package.json`), or a directory holding a valid `CACHEDIR.TAG`.
- An excluded directory is listed but not entered, and appears in
  `scan.excluded` with `reason` `excluded` or `excluded_cache`. Excluded
  files are left out. `scan.excluded_count` counts both.

### Changed

- `SECURITY.md`: from 0.1.0 until 1.0, the latest minor release receives
  security fixes.
- A scan on macOS runs 16 worker threads by default, up from 8: the
  count the thread sweep on the benchmark trees chose. `--threads` and
  `DUSCOPE_THREADS` still set it.
- The Linux release builds (statically linked with musl, the archives and
  `.deb` packages) allocate with mimalloc: a warm scan of a million entries
  takes 0.39 s on the x86_64 benchmark machine, where musl's own allocator
  took 1.02 s. macOS keeps the system allocator, which mimalloc did not
  beat.
- README.md describes what v0.1.0 does, with a tour of `top`, `tree`,
  `by-type`, `stale`, `--plain` and the treemap, checked against the
  fixture tree by a test.

### Fixed

- A directory holding a million entries is read in chunks instead of
  whole: a scan of one on macOS no longer peaks at gigabytes of memory.
  With `--attrs full`, a scan's peak memory stays within 130 bytes per
  entry on the benchmark trees.

## [0.0.1] - 2026-10-25

The phase-0 placeholder release. The binary only prints its version. The release
reserves the name on crates.io and starts the clocks that the Homebrew and
registry paths wait on.

Clock dates. homebrew-core accepts a formula only when its repository is at
least 30 days old.

- 2026-10-23, 04:05:06 UTC: the repository github.com/kawadeomkar/duscope was
  created. It is 30 days old on 2026-11-22. The repository is also the
  homepage, so the homepage clock is the same one.
- 2026-10-23, 07:28:27 UTC: the tap github.com/kawadeomkar/homebrew-tap was
  created with `brew tap-new --github-packages`, keeping its `tests.yml` and
  `publish.yml` and without `autobump.yml`, because release.yml opens the
  version-bump pull requests.
- 2026-10-23: the `gh-pages` branch was created and GitHub Pages serves it, as
  the host of the apt repository (P5-PKG-01).

<!-- Later channels add the date they count from as another line of the list
     above. At release, scripts/changelog.py dates this heading and adds the
     assembled sections below these notes. -->

### Added

- Release archives on GitHub for macOS (arm64 and x86_64) and Linux (arm64 and
  x86_64, statically linked with musl), each with a build attestation that
  `gh attestation verify` checks, and a shell installer.
- The duscope crates on crates.io, so `cargo install duscope --locked` installs
  the release.
- `README.md`: what duscope is, its pre-release status, and each install
  channel with the release that opens it.
- `SECURITY.md`: how to report a vulnerability privately, through GitHub's
  private vulnerability reporting, with the 90-day disclosure window.
- The `duscope` binary as a pre-release placeholder: `duscope --version` prints the version.
- Each crate on crates.io ships a README and the MIT and Apache-2.0 licence texts.
