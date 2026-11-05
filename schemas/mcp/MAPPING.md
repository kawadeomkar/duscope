# MCP tool mapping

How each MCP tool maps onto its verb: the verb's flags the tool exposes, the ones
it renames, and the ones it excludes, with the reason. `<tool>.input.json` beside
this file is the tool's hand-written input schema and `<tool>.output.json` its
output schema. A test compares each input schema with the clap definition of its
verb through this file, and fails on a flag that is neither exposed nor listed as
excluded.

Each tool has a section headed `` ### `<tool>` (`<verb>`) `` with one table, and
the global flags have one table for every tool. A row names a flag (`--long`, or
a positional argument's value name such as `PATH`), then the tool parameter that
sets it in backticks, or `—` when none does, with the reason. A tool's row for a
global flag takes the place of the global row. The test checks that every flag
of the verb has a row, every row a flag, every schema property a row and every
row's parameter a property, and that the types agree: a switch is a boolean, a
repeatable flag an array, and a parameter's `enum` lists values the flag takes.

Each package owns one section, so the two can be edited side by side.

## Global flags

| Flag | Parameter | Why |
| --- | --- | --- |
| `--config` | — | set by the server: the configuration file it was started with |
| `--verbose` | — | the server's log, at its `--log-level`, takes the verb's |
| `--debug` | — | the server's log, at its `--log-level`, takes the verb's |
| `--quiet` | — | the server's log, at its `--log-level`, takes the verb's |
| `--index-dir` | — | set by the server: its index directory |
| `--json` | — | set by the server: a tool returns the JSON envelope |
| `--plain` | — | a tool returns the JSON envelope |
| `--format` | — | a tool returns the JSON envelope |
| `--color` | — | for a terminal |
| `--no-color` | — | for a terminal |
| `--si` | — | for human-readable sizes; the envelope's are bytes |
| `--bytes` | — | for human-readable sizes; the envelope's are bytes |
| `--null` | — | for a shell pipeline |
| `--header` | — | for the plain format's columns |
| `--fields` | — | a tool whose rows have fields exposes it as `fields` |
| `--detail` | — | for the human formats' columns |
| `--output` | — | writes a file the client would choose |
| `--max-chars` | — | set by the server: its `--max-chars` |

## Scan group (P3-MCP-04)

The read-only tools run their verb with `--no-scan`, so they never scan or save:
a scan is `duscope_scan`'s alone. With no `scan_id`, a verb reads the newest
saved scan that holds the path and was taken with the default options (the
configuration's); a scan taken with others is read by its `scan_id`.

### `duscope_scan` (`scan`)

| Flag | Parameter | Why |
| --- | --- | --- |
| `PATH` | `root` | absolute, under a root; the server's one root by default |
| `--save` | — | always set: a scan the other tools can read |
| `--reuse` | `max_age` | |
| `--attrs` | `attrs` | |
| `--clones` | `clones` | `true` |
| `--no-clones` | `clones` | `false` |
| `--one-file-system` | `one_file_system` | |
| `--threads` | — | the platform's default, from its thread sweep |
| `--include-network` | — | a tool reads the local file system alone (`openWorldHint` false) |
| `--follow-symlinks` | — | a link can lead out of the roots |
| `--size` | — | what the scan's rows are ranked by; the read tools take `size` |
| `--apparent-size` | — | what the scan's rows are ranked by; the read tools take `size` |
| `--exclude` | — | a scan taken with excludes is read only by a call that repeats them, which the read tools cannot |
| `--exclude-from` | — | names a file to read, which a client would choose |
| `--ignore-from` | — | names a file to read, which a client would choose |
| `--exclude-caches` | — | a scan taken with excludes is read only by a call that repeats them, which the read tools cannot |
| `--depth` | — | the treemap's |
| `--treemap` | — | the treemap's: a tool returns the JSON envelope |
| `--color-by` | — | the treemap's |
| `--theme` | — | the treemap's |
| `--pool-files` | — | the treemap's |
| `--min-tile` | — | the treemap's |
| `--interactive` | — | the terminal viewer |
| `--skip-containers` | — | hidden: a later phase's |
| `--provider` | — | hidden: for tests |
| `--test-now` | — | hidden: for tests |

### `duscope_scans` (`scans`)

| Flag | Parameter | Why |
| --- | --- | --- |
| `PATH` | — | set by the server: every root, so the scans of directories under them are listed |
| `--prune` | — | deletes saved scans |
| `--check` | — | checks an index file for damage, for whoever keeps the index directory |
| `--trust` | — | names an index file outside the index directory; a client names saved scans by id |
| `--test-now` | — | hidden: for tests |

### `duscope_summary` (`summary`)

| Flag | Parameter | Why |
| --- | --- | --- |
| `PATH` | `path` | absolute, or relative to the scan's root |
| `--scan` | `scan_id` | `latest` or an id, never an index file |
| `--no-scan` | — | always set: a read-only tool never scans |
| `--no-save` | — | with `--no-scan`, nothing is scanned to save |
| `--trust` | — | names an index file outside the index directory; a client names saved scans by id |
| `--max-age` | — | the `stale_index` warning's threshold: the configuration's, or 7d |
| `--size` | — | the result holds both sizes |
| `--apparent-size` | — | the result holds both sizes |
| `--threads` | — | the tool never scans |
| `--one-file-system` | — | the tool never scans |
| `--include-network` | — | the tool never scans |
| `--follow-symlinks` | — | the tool never scans |
| `--attrs` | — | the tool never scans |
| `--clones` | — | the tool never scans |
| `--no-clones` | — | the tool never scans |
| `--exclude-from` | — | the tool never scans |
| `--ignore-from` | — | the tool never scans |
| `--exclude-caches` | — | the tool never scans |
| `--skip-containers` | — | hidden: a later phase's |
| `--provider` | — | hidden: for tests |
| `--test-now` | — | hidden: for tests |

### `duscope_top` (`top`)

| Flag | Parameter | Why |
| --- | --- | --- |
| `PATH` | `path` | absolute, or relative to the scan's root |
| `--scan` | `scan_id` | `latest` or an id, never an index file |
| `--files` | `files` | |
| `--depth` | `depth` | |
| `--min-size` | `min_size` | |
| `--glob` | `glob` | |
| `--exclude` | `exclude` | |
| `--size` | `size` | |
| `--fields` | `fields` | joined with commas |
| `--limit` | `limit` | 1 to 100 |
| `--cursor` | `cursor` | |
| `--no-cut-through` | — | looking through a directory that holds 80% of its parent is what a question about space wants |
| `--min-depth` | — | the rows start at the path, which the call chooses |
| `--max-size` | — | the tool lists the largest |
| `--older-than` | — | `duscope_stale` answers by age |
| `--newer-than` | — | `duscope_stale` answers by age |
| `--since` | — | `duscope_stale` answers by age |
| `--until` | — | `duscope_stale` answers by age |
| `--time` | — | `duscope_stale` answers by age |
| `--type` | — | `files` chooses files or directories |
| `--sort` | — | the tool lists the largest first |
| `--reverse` | — | the tool lists the largest first |
| `--regex` | — | `glob` and `exclude` match paths |
| `--invert` | — | `exclude` leaves rows out |
| `--all` | — | a tool returns one envelope; `cursor` pages |
| `--strict` | — | an unreadable directory is a warning in the envelope |
| `--no-scan` | — | always set: a read-only tool never scans |
| `--no-save` | — | with `--no-scan`, nothing is scanned to save |
| `--trust` | — | names an index file outside the index directory; a client names saved scans by id |
| `--max-age` | — | the `stale_index` warning's threshold: the configuration's, or 7d |
| `--apparent-size` | — | `size` |
| `--threads` | — | the tool never scans |
| `--one-file-system` | — | the tool never scans |
| `--include-network` | — | the tool never scans |
| `--follow-symlinks` | — | the tool never scans |
| `--attrs` | — | the tool never scans |
| `--clones` | — | the tool never scans |
| `--no-clones` | — | the tool never scans |
| `--exclude-from` | — | the tool never scans |
| `--ignore-from` | — | the tool never scans |
| `--exclude-caches` | — | the tool never scans |
| `--skip-containers` | — | hidden: a later phase's |
| `--provider` | — | hidden: for tests |
| `--test-now` | — | hidden: for tests |

### `duscope_by_type` (`by-type`)

| Flag | Parameter | Why |
| --- | --- | --- |
| `PATH` | `path` | absolute, or relative to the scan's root |
| `--scan` | `scan_id` | `latest` or an id, never an index file |
| `--examples` | `examples` | |
| `--glob` | `glob` | |
| `--exclude` | `exclude` | |
| `--size` | `size` | |
| `--fields` | `fields` | joined with commas |
| `--limit` | `limit` | 1 to 100 |
| `--classes` | — | names a file to read, which a client would choose; the configuration's `[classes]` applies |
| `--cursor` | — | `by-type` does not page |
| `--all` | — | a tool returns one envelope |
| `--depth` | — | the kinds count every file under the path |
| `--min-depth` | — | the kinds count every file under the path |
| `--min-size` | — | the kinds count every file under the path |
| `--max-size` | — | the kinds count every file under the path |
| `--older-than` | — | `duscope_stale` answers by age |
| `--newer-than` | — | `duscope_stale` answers by age |
| `--since` | — | `duscope_stale` answers by age |
| `--until` | — | `duscope_stale` answers by age |
| `--time` | — | `duscope_stale` answers by age |
| `--type` | — | the kinds are kinds of file |
| `--sort` | — | the tool lists the largest first |
| `--reverse` | — | the tool lists the largest first |
| `--regex` | — | `glob` and `exclude` match paths |
| `--invert` | — | `exclude` leaves rows out |
| `--strict` | — | an unreadable directory is a warning in the envelope |
| `--no-scan` | — | always set: a read-only tool never scans |
| `--no-save` | — | with `--no-scan`, nothing is scanned to save |
| `--trust` | — | names an index file outside the index directory; a client names saved scans by id |
| `--max-age` | — | the `stale_index` warning's threshold: the configuration's, or 7d |
| `--apparent-size` | — | `size` |
| `--threads` | — | the tool never scans |
| `--one-file-system` | — | the tool never scans |
| `--include-network` | — | the tool never scans |
| `--follow-symlinks` | — | the tool never scans |
| `--attrs` | — | the tool never scans |
| `--clones` | — | the tool never scans |
| `--no-clones` | — | the tool never scans |
| `--exclude-from` | — | the tool never scans |
| `--ignore-from` | — | the tool never scans |
| `--exclude-caches` | — | the tool never scans |
| `--skip-containers` | — | hidden: a later phase's |
| `--provider` | — | hidden: for tests |
| `--test-now` | — | hidden: for tests |

### `duscope_stale` (`stale`)

| Flag | Parameter | Why |
| --- | --- | --- |
| `PATH` | `path` | absolute, or relative to the scan's root |
| `--scan` | `scan_id` | `latest` or an id, never an index file |
| `--older-than` | `older_than` | |
| `--min-size` | `min_size` | |
| `--atime` | `atime` | |
| `--attrs` | — | set to `full` when `atime` is true, so the scan read holds access times |
| `--glob` | `glob` | |
| `--exclude` | `exclude` | |
| `--size` | `size` | |
| `--fields` | `fields` | joined with commas |
| `--limit` | `limit` | 1 to 100 |
| `--cursor` | `cursor` | |
| `--depth` | — | files at any depth under the path |
| `--min-depth` | — | files at any depth under the path |
| `--max-size` | — | the tool lists the largest |
| `--newer-than` | — | the tool lists what is old |
| `--since` | — | the tool lists what is old |
| `--until` | — | `older_than` |
| `--time` | — | the tool measures the modification time, and the access time with `atime` |
| `--type` | — | the tool lists files |
| `--sort` | — | the tool lists the largest first |
| `--reverse` | — | the tool lists the largest first |
| `--regex` | — | `glob` and `exclude` match paths |
| `--invert` | — | `exclude` leaves rows out |
| `--all` | — | a tool returns one envelope; `cursor` pages |
| `--strict` | — | an unreadable directory is a warning in the envelope |
| `--no-scan` | — | always set: a read-only tool never scans |
| `--no-save` | — | with `--no-scan`, nothing is scanned to save |
| `--trust` | — | names an index file outside the index directory; a client names saved scans by id |
| `--max-age` | — | the `stale_index` warning's threshold: the configuration's, or 7d |
| `--apparent-size` | — | `size` |
| `--threads` | — | the tool never scans |
| `--one-file-system` | — | the tool never scans |
| `--include-network` | — | the tool never scans |
| `--follow-symlinks` | — | the tool never scans |
| `--clones` | — | the tool never scans |
| `--no-clones` | — | the tool never scans |
| `--exclude-from` | — | the tool never scans |
| `--ignore-from` | — | the tool never scans |
| `--exclude-caches` | — | the tool never scans |
| `--skip-containers` | — | hidden: a later phase's |
| `--provider` | — | hidden: for tests |
| `--test-now` | — | hidden: for tests |

## Analysis group (P3-MCP-07)

`duscope_caches`, `duscope_explain`, `duscope_query`, `duscope_schema`,
`duscope_diff`: not yet written.
