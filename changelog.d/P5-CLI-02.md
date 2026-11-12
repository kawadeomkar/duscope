### Added

- The `excluded_truncated` warning: a list's `scan.excluded` names at most 20 directories,
  and the warning's `count` says how many more the excludes kept the scan out of under the
  path; its hint names the `duscope summary` that lists every one.

### Changed

- `scan.excluded` names only the directories the excludes kept the scan out of within the
  path a result was read at, and a list (`scan`, `top`, `tree`, `stale`, `by-type`, `caches`,
  `diff`, `query`, `explain --errors`, `clean`'s plan) names the first 20 of them in the
  scan's order. Every envelope carried the whole scan's list: 300 `CACHEDIR.TAG` folders made
  `top` of a folder holding none spill to a file. `summary` and `explain` still name every
  one under their path; `summary` of the scan's root is unchanged. `scan.excluded_count`
  stays the whole scan's.
- `scan.volumes` names the volume the path lies on and those mounted under it, not every
  volume the scan met; at the scan's root it is unchanged.
- Over MCP, a scan of a directory above a root, read at the root, names no excluded
  directory outside it, and of the volumes only the one the root lies on and those mounted
  under it.
