### Changed
- `by-type` on a scan saved with `--attrs full` answers from the index, as on a basic one, rather than reading each entry: 63 ms instead of 500 ms over a whole saved scan of a million entries, and 49 ms instead of 150 ms for a directory of 262,000 files. A file without an extension is still `binaries` when an execute bit is set.
