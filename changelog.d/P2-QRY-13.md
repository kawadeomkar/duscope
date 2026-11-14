### Changed

- `top` and `stale` with `--format ndjson`, or with `--plain`, write each row as it is built
  instead of holding them all: `top --files --all --format ndjson` on a million entries peaks at
  about 200 MB where it took 760 MB (and 3 GB with `--fields`). The output of a run that succeeds
  is the same, byte for byte. A run whose read of the saved scan fails part way (`index_corrupt`,
  exit 5) now writes the rows read before the failure, where it wrote none, then under
  `--format ndjson` the error envelope in place of the `end` line, and under `--plain` nothing
  more on stdout (the error goes to stderr).
