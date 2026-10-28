### Fixed

- A directory holding a million entries is read in chunks instead of
  whole: a scan of one on macOS no longer peaks at gigabytes of memory.
  With `--attrs full`, a scan's peak memory stays within 130 bytes per
  entry on the benchmark trees.
