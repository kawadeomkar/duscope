# Changelog

All notable changes to duscope are recorded in this file. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Changes are not written here directly. Each pull request with a user-visible
change adds a fragment, `changelog.d/<ID>.md`, in the format given in
[changelog.d/README.md](changelog.d/README.md). At each release
`scripts/changelog.py` assembles the fragments into the version's dated section
and deletes them.

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
