### Changed

- `tree`, `by-type`, `caches` and `diff` with `--format ndjson`, or with `--plain`, write each row
  as it is built instead of holding them all, as `top` and `stale` do: on a million entries,
  `tree --all --depth 1000 --format ndjson` peaks at about 64 MB where it took 720 MB, and
  `diff --all --format ndjson` of two scans that differ in every file at about 220 MB where it took
  770 MB. The output of a run that succeeds is the same, byte for byte. A run whose read of a saved
  scan fails part way (`index_corrupt`, exit 5) now writes the rows read before the failure, where
  it wrote none, then under `--format ndjson` the error envelope in place of the `end` line, and
  under `--plain` nothing more on stdout (the error goes to stderr).
