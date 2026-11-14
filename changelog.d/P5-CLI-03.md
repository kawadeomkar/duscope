### Fixed

- `query --sql` and `export` whose scan cannot be saved print the error envelope (`index_write_failed`, exit 1), as for any other failure: `query` under `--json` and `--format ndjson`, `export` under `--format ndjson`. They save the scan they run before printing anything, and the failure went to stderr alone, leaving stdout empty. The hint now names `--no-save` and `--index-dir`, where it named `duscope scan --save`.
- `import --json` with no index directory (neither `HOME` nor `XDG_CACHE_HOME` set) prints the error envelope, where stdout was empty.
