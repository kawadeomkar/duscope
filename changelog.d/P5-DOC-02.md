### Added

- `docs/troubleshooting.md`: every warning and error code duscope gives, what it means, why it
  happens and what to do about it. `docs/limits.md`: what duscope cannot measure, or measures
  only within bounds, and why: APFS clones written to since they were cloned, reflinks on
  Linux, snapshots, the Full Disk Access probe, the FSEvents history behind a macOS rescan, a
  tree held in memory and its 4 GiB of names, subvolumes, overlayfs, and names that are not
  UTF-8. The manual page's CAVEATS points at both.
