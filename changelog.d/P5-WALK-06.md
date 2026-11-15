### Added

- A row's `flags` can name `automount`: on Linux, a directory the scan listed and did not enter because it is an automount point the kernel had not mounted. A saved scan records it as bit 14 of `dirs.flags` (docs/index-format.md).

### Fixed

- On Linux, a scan no longer mounts a filesystem by entering an automount point the kernel has not mounted yet (an NFS submount or referral, debugfs's `tracing`). Such a point is listed and left alone like a network mount: skipped as `one_file_system` under `-x`, otherwise as `network` unless `--include-network` is given. Before, one on the scanned root's own filesystem was entered whatever the flags, `-x` included.
- On Linux 4.11 to 5.12, an automount point the kernel has not mounted yet is now left alone too. Those kernels mark it without vouching for the mark in `statx`'s attribute mask (the 5.10 series vouches from 5.10.36), and a scan read it as a plain directory and entered it, mounting it, whatever `-x` and `--include-network` said.
- On Linux, an automount point a scan leaves alone now counts in the `mounts_excluded` warning, of a fresh scan and of a saved one read at any path above it, and the warning's message counts such points apart from the mounted filesystems `scan.volumes` names. A verb asked about the point itself scans it, as it scans a directory a saved scan excluded. Before, the point read as an empty directory, with no warning.
- On Linux, `clean` plans a directory that holds an automount point the scan left alone as it plans one that holds an excluded directory: what a walk finds under the point is added to the scan's figures as an upper bound, and is not read as a change since the scan.
- On Linux, an ncdu export (`export --format ncdu`) marks an automount point the scan left alone `"excluded":"otherfs"`, as ncdu marks a filesystem it did not enter; `import` reads it back as one.
