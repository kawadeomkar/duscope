### Fixed

- Ctrl-C stops `duscope import` as it stops `scan --save`: while the file is read, or while the
  import is saved, duscope exits 130 with `duscope: interrupted` and leaves neither the index nor
  its temporary file; once the index is saved, the import finishes. Ctrl-C used to kill it, with
  no message, and a Ctrl-C during the save left a temporary file for `scans --prune`.
