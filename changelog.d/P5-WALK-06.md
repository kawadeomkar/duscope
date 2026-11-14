### Fixed

- On Linux, a scan no longer mounts a filesystem by entering an automount point the kernel has not mounted yet (an NFS submount or referral, debugfs's `tracing`). Such a point is listed and left alone like a network mount: skipped as `one_file_system` under `-x`, otherwise as `network` unless `--include-network` is given. Before, one on the scanned root's own filesystem was entered whatever the flags, `-x` included.
- On Linux 4.11 to 5.12, an automount point the kernel has not mounted yet is now left alone too. Those kernels mark it without vouching for the mark in `statx`'s attribute mask (the 5.10 series vouches from 5.10.36), and a scan read it as a plain directory and entered it, mounting it, whatever `-x` and `--include-network` said.
