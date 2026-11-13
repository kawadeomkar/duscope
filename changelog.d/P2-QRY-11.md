### Changed
- `caches` on a saved scan reads the scan's directories and only the files it looks for (each `CACHEDIR.TAG` and the cache rules' marker files) instead of every entry, and reads nothing inside a cache once it is confirmed: about five times faster on a scan of a million entries (422 ms to 81 ms). Its rows, with `--loose` and `--by-project` too, are the same as before.
