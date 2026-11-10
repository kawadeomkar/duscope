### Added
- The `changed_since_scan` warning: `clean` measured items from the tree because they changed since the saved scan; its `count` says how many, and its hint names the rescan that measures the tree again. The viewer's mark pane says the same under its plan.

### Fixed
- `clean` no longer prices an item that changed since its saved scan at the scan's size. A file whose kind, modification time or size differs from the scan's, and a directory whose files, folders, bytes or newest modification time differ (what lies under a folder the scan did not enter left out), is measured from the tree: a directory by a walk, whose bytes deleting it frees at most (`upper_bound`). `--expect-bytes` is held to those figures, so a directory that grew since its scan no longer passes the figure the scan gave it, and nothing is touched.
- A directory made since the scan, or one the scan listed without entering, counts in the plan's `total_frees_bytes` by its walk, where it counted 0, so `--expect-bytes 0` no longer passes for it.

### Changed
- `clean --from` fails with `path_changed` when the entry a handle names has changed since the saved scan, as it does when another entry has taken its place: the handle signs what the scan saw. `duscope_delete`, which runs `clean`, refuses such an item the same way, and still refuses a directory whose bytes no saved scan measured, whatever its `expect_bytes`.
