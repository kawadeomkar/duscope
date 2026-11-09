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
| `--skip-containers` | — | a scan taken with excludes is read only by a call that repeats them, which the read tools cannot |
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
| `--skip-containers` | — | the tool never scans |
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
| `--skip-containers` | — | the tool never scans |
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
| `--skip-containers` | — | the tool never scans |
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
| `--skip-containers` | — | the tool never scans |
| `--provider` | — | hidden: for tests |
| `--test-now` | — | hidden: for tests |

## Analysis group (P3-MCP-07)

`duscope_caches` reads a saved scan with `--no-scan`, as the scan group does;
`duscope_explain` reads one, or the path as it is now, and never scans.
`duscope_query`'s statement and `duscope_schema`'s row counts read a whole scan,
not the subtree at a path, so these two read only a scan whose root is a root or
lies under one: one whose root lies above a root is not found, as one the roots
hide. With no `scan_id`, or `latest`, they read the newest such scan that holds
the path, and pass its id. `duscope_diff` always compares under a path inside
the roots, so a scan whose root lies above a root is compared at that root.

### `duscope_caches` (`caches`)

| Flag | Parameter | Why |
| --- | --- | --- |
| `PATH` | `path` | absolute, or relative to the scan's root |
| `--scan` | `scan_id` | `latest` or an id, never an index file |
| `--by-project` | `by_project` | |
| `--loose` | `loose` | |
| `--min-size` | `min_size` | |
| `--size` | `size` | |
| `--fields` | `fields` | joined with commas |
| `--limit` | `limit` | 1 to 100 |
| `--cursor` | `cursor` | |
| `--depth` | — | caches are found at any depth under the path |
| `--min-depth` | — | caches are found at any depth under the path |
| `--max-size` | — | the tool lists the largest |
| `--older-than` | — | `duscope_stale` answers by age |
| `--newer-than` | — | `duscope_stale` answers by age |
| `--since` | — | `duscope_stale` answers by age |
| `--until` | — | `duscope_stale` answers by age |
| `--time` | — | `duscope_stale` answers by age |
| `--type` | — | caches are directories |
| `--sort` | — | the tool lists the largest first |
| `--reverse` | — | the tool lists the largest first |
| `--glob` | — | caches are found by their rules' markers |
| `--regex` | — | caches are found by their rules' markers |
| `--invert` | — | caches are found by their rules' markers |
| `--exclude` | — | caches are found by their rules' markers |
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
| `--skip-containers` | — | the tool never scans |
| `--provider` | — | hidden: for tests |
| `--test-now` | — | hidden: for tests |

### `duscope_explain` (`explain`)

| Flag | Parameter | Why |
| --- | --- | --- |
| `PATH` | `path` | absolute, or relative to the scan's root |
| `--scan` | `scan_id` | `latest` or an id, never an index file |
| `--errors` | — | lists the unreadable directories of a whole scan, which for a scan above a root lie outside the roots |
| `--clones` | — | opens every file under the path to read its extent map: a cost on demand that the tool, which answers from the saved scan, does not take |
| `--max-age` | — | the `stale_index` warning's threshold: the configuration's, or 7d |
| `--trust` | — | names an index file outside the index directory; a client names saved scans by id |
| `--test-now` | — | hidden: for tests |

### `duscope_query` (`query`)

| Flag | Parameter | Why |
| --- | --- | --- |
| `--sql` | `sql` | |
| `PATH` | `path` | absolute, or relative to the scan's root: the entry `:root_id` names |
| `--scan` | `scan_id` | always set: the scan `scan_id` names, or the newest of a directory under the roots that holds the path |
| `--max-rows` | — | a `LIMIT` in the statement asks for fewer; the ceiling stays the sandbox's 10000 |
| `--timeout` | — | the sandbox's 5 seconds |
| `--no-scan` | — | always set: a read-only tool never scans |
| `--no-save` | — | with `--no-scan`, nothing is scanned to save |
| `--trust` | — | names an index file outside the index directory; a client names saved scans by id |
| `--max-age` | — | the `stale_index` warning's threshold: the configuration's, or 7d |
| `--size` | — | the scan block's measure; the tables hold both sizes |
| `--apparent-size` | — | the scan block's measure; the tables hold both sizes |
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
| `--skip-containers` | — | the tool never scans |
| `--provider` | — | hidden: for tests |
| `--test-now` | — | hidden: for tests |

### `duscope_schema` (`schema`)

| Flag | Parameter | Why |
| --- | --- | --- |
| `PATH` | `path` | absolute, or relative to the scan's root |
| `--scan` | `scan_id` | always set, but with `envelope`: the scan `scan_id` names, or the newest of a directory under the roots that holds the path |
| `--envelope` | `envelope` | a verb's name; the result's `envelopes` lists them |
| `--trust` | — | names an index file outside the index directory; a client names saved scans by id |

### `duscope_diff` (`diff`)

| Flag | Parameter | Why |
| --- | --- | --- |
| `OLD` | `old` | `latest` or an id, never an index file |
| `NEW` | `new` | `latest` or an id, never an index file |
| `--under` | `path` | always set: the path, or the older scan's root, or the root under it |
| `--min-delta` | `min_delta` | |
| `--depth` | `depth` | |
| `--size` | `size` | |
| `--limit` | `limit` | 1 to 100 |
| `--all` | — | a tool returns one envelope |
| `--trust` | — | names an index file outside the index directory; a client names saved scans by id |

## Delete (P5-MCP-01)

`duscope_delete` runs `clean` on the handles a call names, and only under the
server's `--allow-delete`: without it the tool is not listed and a call to it
is an unknown tool. It is Trash-only, always: no argument reaches
`--permanent`, and a value it passes on follows a `=`, so it cannot be read as
a flag. Each `handle` is verified and its path bound to the roots (under one,
and not one itself) before the verb runs; each `expect_bytes` goes to
`--expect-item-bytes`, which refuses an item whose `frees_bytes` is not within
1% of it. `dry_run` defaults to true, and answers the plan as a successful
result (`--dry-run`); `false` is `--yes`.

### `duscope_delete` (`clean`)

| Flag | Parameter | Why |
| --- | --- | --- |
| `PATH` | — | a tool removes only what a handle names |
| `--from` | `items` | each item's `handle` |
| `--expect-item-bytes` | `items` | each item's `expect_bytes` (hidden: server-only) |
| `--yes` | `dry_run` | `false` |
| `--dry-run` | `dry_run` | `true`, the default (hidden: server-only) |
| `--permanent` | — | the Trash only, over MCP |
| `--expect-bytes` | — | a whole plan's figure: `expect_bytes` is per item |
| `--no-input` | — | a tool run has no terminal: `--json` never prompts |
| `--skip-refused` | — | a refused item stops the call |
| `--scan` | — | the scan the handle names, else the newest that holds its path |
| `--max-age` | — | the plan warns of an old scan; a client cannot act on the flag |
| `--trust` | — | names an index file outside the index directory; a client names saved scans by id |
| `--test-now` | — | hidden: for tests |

## Conformance oracle (P3-MCP-06)

Oracle: rmcp 3.9.0, the protocol's Rust SDK, whose client the conformance suite
runs against `duscope mcp`.

The workspace's `Cargo.toml` pins it with `=3.9.0`, a dev-dependency of
`duscope-mcp` alone, and `deny.toml` bans it, with the tokio it runs on, from
everything else, so no build the project ships holds it (D36, DEV-104). A
`p3_mcp_06_*` test holds the line above to that pin and to the version
`Cargo.lock` resolves; a `DEPS:` pull request that moves the pin moves the line.

The crate ships no suite to point at a server: its tests drive its own client
against its own server, or against a JavaScript or Python server they start
over stdio and list the tools of (`tests/test_with_js.rs`,
`tests/test_with_python.rs`). `crates/duscope-mcp/tests/conformance.rs` drives
that client the same way against the binary: both protocol revisions'
handshakes, a version refused and retried from the binary's list, a 2024-11-05
client refused, `server/discover`, `tools/list`, `tools/call` of every tool the
binary lists in both revisions, the errors a client meets, a deadline, a
running and a queued call cancelled, and a scan's progress. A tap between the
two holds every line the binary writes to rmcp's model of what it answers. CI's
`mcp-conformance` job runs the crate's tests on ubuntu-24.04 and macos-26, and
prints the oracle's version in its log.
