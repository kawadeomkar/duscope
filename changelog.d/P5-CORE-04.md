### Added
- The `changed_since_scan` warning: `clean` measured items from the tree because they changed since the saved scan; its `count` says how many, and its hint names the rescan that measures the tree again. The viewer's mark pane says the same under its plan.

### Fixed
- `clean` no longer prices an item that changed since its saved scan at the scan's size. A file whose kind, modification or change time, size, bytes on disk or link count differs from the scan's, and a directory whose files, folders, bytes, bytes on disk or newest modification or change time differ (what lies under a folder the scan did not enter left out), is measured from the tree: a directory by a walk, whose bytes deleting it frees at most (`upper_bound`). `--expect-bytes` is held to those figures, so a directory that grew since its scan no longer passes the figure the scan gave it, and nothing is touched. A write whose modification time was put back, and a hard link whose other half has gone, count as changes.
- A directory made since the scan, or one the scan listed without entering, counts in the plan's `total_frees_bytes` by its walk, where it counted 0, so `--expect-bytes 0` no longer passes for it.
- A directory that holds one the scan listed without entering (`--exclude`, `--exclude-caches`) counts what a walk finds there, as an `upper_bound`, where the plan called the scan's smaller figure `exact` and `--expect-bytes` passed against it.
- A changed file with other links counts its bytes, at most and once however many of its links the plan holds, where it counted nothing and `--expect-bytes 0` passed.
- A directory that cannot be read whole has no `frees_bytes` (a figure from what could be read would look like a measurement): the plan carries a `partial_read` warning, and neither `--expect-bytes` nor `duscope_delete`'s `expect_bytes` is met by it.
- The viewer's mark pane no longer freezes the viewer while the plan walks the marked folders: `D` opens it measuring, the viewer goes on answering, `Esc` gives the plan up and `Ctrl-C` quits, and nothing typed counts until the plan is on the screen.

### Changed
- `clean --from` fails with `path_changed` when the entry a handle names has changed since the saved scan, as it does when another entry has taken its place: the handle signs what the scan saw. `duscope_delete`, which runs `clean`, refuses such an item the same way, and still refuses a directory whose bytes no saved scan measured (made since it, not entered by it, or holding what it did not enter), whatever its `expect_bytes`.
