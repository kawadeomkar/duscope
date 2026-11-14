### Fixed

- On Linux, a scan no longer mounts a filesystem by entering an automount point the kernel has not mounted yet (an NFS submount or referral, debugfs's `tracing`). Such a point is listed and left alone like a network mount: skipped as `one_file_system` under `-x`, otherwise as `network` unless `--include-network` is given. Before, one on the scanned root's own filesystem was entered whatever the flags, `-x` included.
