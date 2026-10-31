### Added
- `scan --save` saves the scan as an index, one SQLite file under `$XDG_CACHE_HOME/duscope/scans/` (`~/Library/Caches/duscope` on macOS when unset), and prints its id (`scan.id` in the envelope); `--index-dir` or `DUSCOPE_INDEX_DIR` names another directory. The index is written after the output, all at once and renamed into place; if it cannot be written, duscope says so on stderr and exits 1, and the output stands.
- `-v` reports the index write.
