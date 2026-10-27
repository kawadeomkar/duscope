### Added

- `top [PATH]` lists the largest directories at `--depth` (default 1), with
  the shallower directories that have none below them, so the rows never
  nest. A row whose directory holds a child of at least 80% of its bytes is
  cut through to that child, up to three levels down, and `via` names the
  directories cut through; `--no-cut-through` keeps strict depth.
  `--files` lists the largest files instead and marks a later hard link
  `uncharged`.
- `tree [PATH]` lists the entries down to `--depth` (default 2), largest
  first under each directory, with `pct_of_root` and `pct_of_parent`.
  `--min-pct` leaves out what is smaller than that share of its parent.
- `by-type [PATH]` adds up bytes and files by class (code, media, archives,
  data, binaries, docs, other), each with its largest files
  (`--examples N`). A directory rule such as `node_modules` wins over the
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
  and `--strict` makes unreadable directories exit 4.
