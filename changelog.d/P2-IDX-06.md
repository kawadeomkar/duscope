### Changed
- Saved scans are written without the `dirs_cum_allocated` and `files_mtime` indexes, which no query read: a save is faster and each index file smaller. The index format stays at 2.0, older files open and answer as before, and only a hand-written `query --sql` that sorts all directories by size or all files by age runs slower.
