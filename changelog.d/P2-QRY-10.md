### Changed

- `top --files` and `stale` on a saved scan answer from the index with `--type`, `--min-size`,
  `--max-size`, `--size apparent`, `--depth`, `--min-depth`, `--max-depth`, the time filters and
  `stale --atime`, instead of reading every entry: on a scan of a million entries, tens of
  milliseconds instead of up to a second and more. The rows are the same. `--glob`, `--regex`,
  `--exclude`, `--sort` other than size, `-r` and `--all` still read every entry.
