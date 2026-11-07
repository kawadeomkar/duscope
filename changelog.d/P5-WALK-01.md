### Added

- On macOS a saved scan records, for each volume it entered, where the volume's FSEvents history stood when the scan began: the UUID of its FSEvents database, the system's event id read before the scan read anything, and the system build (`fsevents_uuid`, `fsevents_event_id` and `os_build` in `scan_volumes`, which `duscope query --sql` reads). An incremental rescan will read the changes since from there. The three stay NULL on Linux.
