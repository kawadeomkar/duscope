### Added

- `explain --clones` sizes the extents the files under a path share with other files or
  snapshots on btrfs and XFS (reflink copies, deduplicated ranges), where the scan recorded no
  clone ids: each file is opened read-only, never read, for its extent map, and only once its
  path, followed through no symbolic link, is found to lead to a regular file on a local
  filesystem that keeps one. A file whose extents are shared is `is_clone`, the bytes it shares
  come off `freeable_now_bytes` as a lower bound, and the evidence says how many files share how
  many bytes.
- `summary` lists the btrfs subvolumes and ZFS datasets under the path in `subvolumes`, each
  with the bytes the scan found on it, less the subvolumes below it, and a ZFS dataset's
  referenced bytes.

### Changed

- On Linux, a btrfs subvolume, a ZFS dataset and a btrfs subvolume mounted on its own are
  flagged `subvolume` in the listings: on kernels before 6.10 too, where btrfs tells its
  subvolumes apart only by their device.
- A ZFS dataset mounted with `casesensitivity=insensitive`, and an HFS volume, are
  case-insensitive, so paths on them are looked up as such.
