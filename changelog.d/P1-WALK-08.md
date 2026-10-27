### Added

- `--follow-symlinks` follows links to files and directories. A link that
  loops, or leads to a directory on its own path, is recorded as an `ELOOP`
  error of the directory holding it, and the scan goes on. A link that leads
  nowhere stays a link.
- Ctrl-C stops a scan within 200 ms with exit 130 and one line on stderr,
  `duscope: interrupted`, and writes nothing else.
- A root given as a symbolic link is scanned as the directory it leads to:
  `scan.root` is the resolved path, `scan.root_as_given` the path as typed.
  One that leads nowhere is an error (exit 3,
  `scan_not_found`), and one that loops is an `io_error`, instead of a
  scan of the link itself.
- Files and directories that disappear while a scan reads them count in
  `scan.vanished_count`, with a `vanished` warning, instead of as errors.
  A directory replaced by a file after it was listed counts as vanished.
- A directory that cannot be read because of `ELOOP`, `ENAMETOOLONG` or
  `EIO` is listed empty and flagged unreadable, as one that cannot be read
  for permission is, and counts in the `partial_read` warning.
