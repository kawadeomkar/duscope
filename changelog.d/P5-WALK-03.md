### Added
- `--skip-containers` lists `~/Library/Containers` and `~/Library/Group Containers` without entering them, for every verb that scans. They appear in `scan.excluded` with `reason: skipped_containers` and count in `excluded_count`; a saved scan and its `rescan` keep them out. The flag is accepted on Linux too, where a home seldom has those folders.
- A scan whose root holds the home's `Library` (the home, `/`, or `~/Library` itself, also by the Data volume's own path such as `/System/Volumes/Data`) checks whether macOS refuses `~/Library/Mail` and `~/Library/Safari`, and then warns `full_disk_access_missing` with the app to grant Full Disk Access to: the terminal, or under `duscope mcp` the app running the server.
- Cloud placeholders (iCloud, Dropbox, OneDrive files whose content is not on the disk) under the path warn `dataless_present` with their count and logical size, for a fresh scan and a saved one.

### Changed
- `explain` leaves dataless placeholders out of `freeable_now_bytes`: a placeholder reports 0, and a directory the bytes on disk of its other files, with an evidence line naming the placeholders.
