### Fixed
- After `clean` moves or deletes something, its `index_untouched` hint and its `next` command suggest `duscope rescan --scan <id>`. They suggested `duscope rescan <id>`, which `rescan` read as a path and answered `scan_not_found`.
