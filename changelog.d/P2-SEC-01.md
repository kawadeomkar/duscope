### Fixed

- An envelope longer than `--max-chars`, when it was the first thing written to
  a new index directory, left that directory with the default permissions
  (usually 0755). duscope now makes the index directory, and each directory on
  the way to its `out/`, 0700, as `SECURITY.md` says.
