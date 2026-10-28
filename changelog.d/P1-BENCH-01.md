### Changed

- A scan on macOS runs 16 worker threads by default, up from 8: the
  count the thread sweep on the benchmark trees chose. `--threads` and
  `DUSCOPE_THREADS` still set it.
