### Fixed

- `scan --reuse` checks the saved scan it prints, as `top` does: a read of it that fails
  (`index_corrupt`, exit 5) ends the rows there under `--format ndjson` and `--plain`, and
  writes none in the other formats. It printed an empty list, or rows naming the scan's root,
  and exited 0.
- `export --format gpscan` records the volume the exported path lies on, found by its
  directory's device as `scan.volumes` finds it. From a macOS scan of `/`, an export of a
  folder under `/Users`, `/private` or `/opt`, which the Data volume holds through a
  firmlink, recorded the size and free space of the read-only System volume mounted at `/`.
- `summary` of a file names the volume the file lies on, with the file's bytes, in
  `result.volumes`. A file in a saved scan had none, and a file scanned for the command had
  its volume with no bytes on it.
