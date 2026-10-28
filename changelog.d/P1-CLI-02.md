### Added

- `--json`, `--plain` and `--format table|plain|json|ndjson` choose how every
  verb writes its result, before or after the verb. Without a flag,
  `DUSCOPE_FORMAT` decides; then an agent's shell (`CLAUDECODE`,
  `CODEX_THREAD_ID`, `CODEX_VERSION`, `GEMINI_CLI`) gets JSON, `CI` gets
  `--plain`, a terminal gets a table, and a pipe gets `--plain`.
- `--json` prints one compact envelope: `scan`, `query`, `entries`,
  `returned_count`, `total_count`, `truncated`, `next_cursor`, `warnings` and
  `next`. Failures print the error object (`code`, `message`, `hint`,
  `retryable`, `fixable_by`). A name that is not UTF-8 adds `name_raw`
  (base64).
- `--format ndjson` writes a `scan` line, one `entry` line per row (every
  row, with no `--limit`), and an `end` line with the counts and warnings.
- `--plain` writes tab-separated records with sizes in bytes and paths as
  their raw bytes; `--header` names the fields and `-0` ends records with NUL.
- `--color auto|always|never` and `--no-color` honour `NO_COLOR`,
  `CLICOLOR_FORCE`, `FORCE_COLOR` and `TERM=dumb`. `--si` shows sizes in
  powers of 1,000, and `-b` shows them in bytes.
