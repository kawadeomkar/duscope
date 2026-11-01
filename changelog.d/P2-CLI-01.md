### Added
- Every verb's `--json` envelope is described by a JSON Schema under `schemas/envelope/` (`<verb>.schema.json`, with `error.schema.json` for a failure), listing the warning codes, the error codes and the values of every enumerated field. `schemas/envelope/history/` keeps each published version.
- `next` lists up to three commands to run on the same scan, most useful first: the next page when rows remain, then the largest entry to `explain` and the related verbs.

### Changed
- `schema_version` is `0.2`, and the envelope changes additively only from here.
- `summary`'s `threads` is `thread_count`, and `diff`'s `entries_added`, `entries_removed` and `entries_changed` are `added_count`, `removed_count` and `changed_count`, as counts are named everywhere else.
