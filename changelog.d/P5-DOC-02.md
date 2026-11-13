### Added

- `docs/troubleshooting.md`: every warning and error code duscope gives, what it means, why it
  happens and what to do about it. `docs/limits.md`: what duscope cannot measure, or measures
  only within bounds, and why: APFS clones written to since they were cloned, reflinks on
  Linux, snapshots, the Full Disk Access probe, the FSEvents history behind a macOS rescan and
  each reason it reads the whole tree, a tree held in memory and its 4 GiB of names, automount
  points, subvolumes, overlayfs, names that are not UTF-8, `--format ndjson` and `--plain`
  output, the Trash on NFS, 9p and FUSE, and the viewer's terminal. The manual page's CAVEATS
  points at both.

### Fixed

- `README.md` said that duscope accounts for btrfs and XFS reflinks. A Linux scan counts each
  reflinked copy in full, as `du` does, and `duscope explain --clones` measures what they share;
  the README now says so.
