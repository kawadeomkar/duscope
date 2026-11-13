### Changed
- `scan --save` holds much less memory: the index is built with SQLite's default page cache, and its sort spills to a temporary file (in `SQLITE_TMPDIR`, `TMPDIR` or the system's temporary directories, mode 0600, unlinked as it is opened, holding directory names and file sizes). If no temporary directory can hold it, the save is built again with the sort in memory.
