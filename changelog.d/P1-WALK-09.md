### Added

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
