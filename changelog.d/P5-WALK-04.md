### Added
- `-vv` prints a line for each worker of the walk: the entries it read and its time in the directory reads.

### Changed
- On Linux, `-v` prints the walk as two lines, `listing:` and `stat:`, that share the walk's time in the proportion the workers spent listing directories and reading entries' attributes.
