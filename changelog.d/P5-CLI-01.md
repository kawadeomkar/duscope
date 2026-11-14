### Added

- `path_raw`: every row and object with a `path` (`top`, `tree`, `stale`, `by-type`'s
  largest files, `caches`, `diff`, `explain`, `explain --errors`, `summary` and its
  subvolumes, `clean`'s plan items) carries the path's bytes in base64 when they are not
  valid UTF-8, as `name_raw` does for a name. A path under a directory whose name is not
  UTF-8 reached JSON as lossy text that names no file; decoded, `path_raw` is the path to
  pass back. `caches` and `caches --by-project` carry `project_root_raw` the same way. Both
  fields are absent for a path that is UTF-8, and come last in `--fields` and `--detail`.

### Changed

- `--plain --detail` writes one more column at the end of every row: `path_raw` for `top`,
  `tree`, `stale`, `diff` and `explain --errors`; two for `caches`, `path_raw` then
  `project_root_raw`; `project_root_raw` for `caches --by-project`. Each is empty for a path
  that is UTF-8. The columns before them keep their places, so a script that reads them by
  position reads the same values, and one that counts the columns finds one more (two for
  `caches`).
