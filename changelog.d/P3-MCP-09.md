### Fixed
- `duscope mcp` reads the configuration file as every other verb does: its `index_dir` is used when neither `--index-dir` nor `DUSCOPE_INDEX_DIR` names a directory, so the server and the command line read and save the same scans, and a configuration file it cannot use stops the server at start (exit 2).
- `duscope_scan` whose save fails answers `index_write_failed` with its message and hint, where it answered an envelope naming a scan id that no saved scan had.
