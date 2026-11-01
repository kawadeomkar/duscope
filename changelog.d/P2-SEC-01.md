### Added
- `--trust FILE` reads an index file outside the index directory. Without it, `--scan FILE` naming one is refused with `untrusted_index` (exit 1), since an index file may come from anyone; the next commands an envelope suggests for such a file carry the `--trust` it needs.
- `SECURITY.md` gives the threat model: the four untrusted inputs (SQL, agent paths and handles, index files, the scanned tree) and the rule for each, the supported versions and the security-release procedure.

### Changed
- Every index file opens with its views, triggers and attached databases switched off, statements of at most 64 KiB and values of at most 1 MB. A file whose schema has a virtual table or more than 256 objects, or whose `tool_version` is not a duscope version, is refused as `index_corrupt`.
- duscope warns on stderr when the index directory, its `scans` directory or an index file is open to other users or another user's, with the `chmod` that fixes it.
