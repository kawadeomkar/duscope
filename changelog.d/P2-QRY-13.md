### Changed

- `top` and `stale` with `--format ndjson`, or with `--plain`, write each row as it is built
  instead of holding them all: `top --files --all --format ndjson` on a million entries peaks at
  about 200 MB where it took 760 MB (and 3 GB with `--fields`). The output is the same, byte for
  byte. `tree`, `by-type`, `caches` and `diff` still hold their rows.
