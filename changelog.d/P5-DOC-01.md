### Added

- `README.md` has a section on macOS and Linux: what a user meets on each (Full Disk Access,
  APFS clones, the volume group, `~/Library/Containers`, FSEvents rescans, cloud placeholders and
  snapshots; reflinks, full rescans, automount points, subvolumes, names that are not UTF-8,
  `relatime`, the Trash), each with the document that covers it. Its install section says what
  each channel installs and what building from crates.io or the source needs.
- The manual page has a section on `query --sql`, SQL QUERIES: which saved scan it reads, what a
  statement may do and what is refused (`sql_denied`), duscope's functions and parameters, the
  row, time (`sql_timeout`) and memory limits, and the output. CAVEATS explains the Full Disk
  Access probe and `full_disk_access_missing` on macOS; ENVIRONMENT adds `HOME`, `XDG_DATA_HOME`,
  `TMUX`, `STY` and `TERM_PROGRAM`, with what duscope reads each for; FILES adds the `out/`
  folder of spilled envelopes and a user `caches.toml`.

### Fixed

- `README.md` said that duscope reads no file's contents but a `CACHEDIR.TAG`'s signature, and
  that only for `--exclude-caches`. `caches` reads the signature too, and `explain` reads the
  files git reads (the ignore files, the configuration and the repository's index) to tell
  whether git tracks or ignores a path; the README now says so. It also said that `scan` always
  scans afresh, which `--reuse` does not.
- The manual page's EXIT STATUS gave exit 2 for "a flag of a later release", which no flag is
  any more, and read as if exit 5 meant the version that wrote an index could not read it. Exit 2
  is a usage error, a cursor or field a verb cannot take, two scans `diff` cannot compare, or what
  the build or platform cannot do; exit 5 is a damaged index or one of an index version this
  duscope cannot read.
- `docs/index-format.md` said that no reader names a secondary index: `top --files`, `stale` and
  `by-type` read `files_allocated`, and fail `index_corrupt` on a file without it. It also gave the
  index directory for macOS alone, and the `sparse` flag for Linux, which never sets it.
