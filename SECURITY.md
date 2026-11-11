# Security policy

Last reviewed: 2026-11-09, against the MCP server as built (P2-SEC-01). The next review is
P5-SEC-01's, before 1.0, against the MCP server's delete tool and `clean`.

duscope measures directory trees. It reads metadata, writes snapshot indexes of what it found
into its own index directory, and answers queries about them. It opens no network socket.
`duscope mcp` lets agents ask the same questions over stdin and stdout, and `clean` moves what
it is told to the trash. This page says what duscope treats as untrusted, the rule for each
untrusted input, how to report a vulnerability, and how a fix is released.

## Reporting a vulnerability

Report a suspected vulnerability privately, through GitHub's private vulnerability reporting:
[open a report](https://github.com/kawadeomkar/duscope/security/advisories/new) (the
repository's Security tab, then "Report a vulnerability"). Please do not open a public issue,
pull request or discussion about it.

Include what you can:
- the output of `duscope --version`;
- the operating system and filesystem;
- the steps or the input that reproduce the problem;
- the impact as you see it.

The report opens a private advisory that only you and the maintainer can see. A confirmed
vulnerability is fixed in private and released as described under
[Security releases](#security-releases). The disclosure window is 90 days from the report: the
advisory is published with the release that fixes the vulnerability, and no later than 90 days
after the report.

## Supported versions

| Version | Security fixes |
| --- | --- |
| The latest minor release (0.x) | Yes, as its next patch release |
| Earlier minor releases | No |
| 0.0.1, the scaffold that printed its version | No |

Until 1.0, only the latest minor release receives security fixes. If you run an earlier minor,
upgrade.

## Threat model

### What is protected

- **The user's files.** duscope never writes to the trees it scans, apart from the index
  directory when a tree holds it. `clean` is the only command that changes anything, and only
  what it is told to change.
- **What the scans reveal.** Every path a scan measured, and its sizes and times, stays private
  to the user who ran the scan, and to the MCP clients that user starts `duscope mcp` for,
  within its `--roots`.
- **The machine.** No input makes duscope run code it does not ship or write outside its index
  directory and the outputs it is asked for, apart from the temporary sort file of a save
  ([Index files](#3-index-files)). Each input's memory and time are bounded by a fixed
  limit or by the size of the tree or saved scan it asked about, as
  [Limits on memory and time](#limits-on-memory-and-time) sets out.

### What is not protected

- **Anything the same user, or root, can do.** They can read the scans and the handle key, and
  run duscope with any flag.
- **A filesystem or kernel that misreports metadata.** duscope reports what it is told.
- **The build chain.** Release builds are reproducible and attested, which is how a build can
  be checked ([RELEASING.md](docs/RELEASING.md)), but this model does not cover the toolchain.

### Limits on memory and time

A fixed limit bounds what a client or a file can make duscope hold or do:

| Input | Limit |
| --- | --- |
| An MCP message | 1 MiB; a longer line is refused and skipped. Parsing one holds more than its length while it lasts: about 100 MB for 1 MiB of very small JSON objects (`{"":0}`), the most a single message can cost. With a running call holding such a message, a full queue and another such message being parsed, the server peaked at 232 MB (both measured on arm64 macOS) |
| Requests waiting behind a running call | 64, holding 32 MiB between them once parsed, ids included; past either a request is answered "busy" and never runs |
| A path a client names | 4096 bytes, refused before anything resolves it |
| A running tool call | 10 minutes for `duscope_scan`, 30 seconds for the other tools, counted from before its arguments are checked. The answer comes at the deadline; a verb that reads a saved scan has no point at which to stop and runs to its end, for a time that grows with the scan, and the next call waits for it |
| SQL | 5 s by default, 60 s at most, and SQLite's heap capped at 256 MiB |
| git's files, for `explain` | The caps under [The scanned tree](#4-the-scanned-tree) |
| An index file | The limits under [Index files](#3-index-files) |
| A compressed import | 1 GiB once decompressed |

Other costs are bounded only by what the user asks duscope to read, and by decision no fixed limit
applies:

- **A scan** takes memory and time that grow with the tree: no limit on its entries or its depth.
  Its memory grows in proportion to the depth, as each directory on the way down holds its name,
  not its path: a chain of 3,000 directories peaked at 11.7 MB, 1.6 MB more than a scan of an
  empty directory (arm64 macOS). Its time grows faster than the depth, and nothing bounds it but
  the tree: duscope reads a tree deeper than `PATH_MAX` whole, so only a cap on depth could bound
  the time of a deep chain, and by decision there is none. A chain of 20,000 directories took
  36 s on macOS; at 3,000 levels a walker that builds no paths took as long as duscope, so that
  time is the kernel's for each open of a deep directory.
- **A query or a verb on a saved scan** takes time and memory that grow with the scan.

### The four untrusted inputs

#### 1. SQL

This is `query --sql`, and the MCP server's `duscope_query` tool, where an agent writes the
statement.

- SQL runs only through the sandbox (`crates/duscope-core/src/query/sql.rs`).
- **The connection** is separate and read-only, opened on a `mode=ro` URI, with
  `SQLITE_DBCONFIG_DEFENSIVE` set, the schema untrusted, and no attached databases.
- **An authorizer** allows `SELECT`, reads of the index's own tables, and a fixed list of
  functions. It denies everything else: `PRAGMA`, `ATTACH`, writes, `load_extension`, and the
  `sqlite_` tables.
- **One statement** only, which SQLite must call read-only. It is run inside
  `SELECT * FROM (…) LIMIT n+1`, at most 10,000 rows (25 by default).
- **Limits:** SQLite's limits are lowered (statement length, columns, expression depth,
  compound selects, program size, function arguments). A deadline stops the statement (5 s by
  default, `--timeout` up to 60 s), and SQLite's heap is capped at 256 MiB.
- **Whole scans:** a statement reads a whole scan, so over MCP `duscope_query` and
  `duscope_schema` read only a scan whose root is a root or lies under one.
- **Fuzzing:** `fuzz/fuzz_targets/sql.rs` fuzzes the sandbox.

#### 2. Agent paths and handles

An agent names paths through the MCP server's tools. A tool writes only to the index directory,
with one exception a person opts into: a server started with `--allow-delete` offers
`duscope_delete`.

- **Handles.** `explain --json` and the `duscope_explain` tool return a handle: an HMAC-SHA256
  tag over the path, its device and inode, its size on disk, the scan, and an expiry fifteen
  minutes on.
  - **The key** is 32 random bytes in `handle.key` in the index directory, mode 0600.
  - **A key that cannot be trusted** issues no handle: a symbolic link in its place, another
    user's key, or one others may read. A warning says why.
  - **Checking a handle:** the tag is compared in constant time before anything in the
    handle is read.
  - **Rejection:** a handle that was changed or has expired is refused, with `handle_invalid`
    or `handle_expired`.
- **Paths.** The server's `--roots` are canonicalised when it starts, and each path an agent
  supplies goes through one check (`crates/duscope-mcp/src/roots.rs`).
  - A path holding a `..` component is refused. A relative path is read from the root of the
    scan the call names, and refused when it names none.
  - The rest is resolved, links included, and refused unless it lies under a root, with
    `refused_path`. A link inside a root that leads out of it is refused like the path it leads
    to, and a root given as a link is the directory it leads to.
  - A part of a path that does not exist (a file deleted since the scan) is kept as written,
    below the deepest part that does.
- **Saved scans.** A client names a saved scan by its id, a prefix of it or `latest`, never by an
  index file, so it cannot reach `--trust`.
  - A saved scan whose root lies outside every root is invisible to it: `scan_not_found`.
  - A saved scan whose root lies above a root is read at that root, as `--under` reads a
    subtree. Its rows stop at the root; the result's `scan` block still names the scan's own
    root and its totals.
- **Deletes.** `clean` acts on the paths a person names and on handles that verify (`--from`).
  - It is a dry run unless `--yes` is given or a person at a terminal types `yes` when it asks.
  - It moves entries to the trash by default. It refuses a folder that holds a mount point, or
    whose mounts the system cannot tell, since a move would take the mounted volume into the
    trash with it; the mount table is read again just before each move.
  - `--permanent` deletes through `openat`, `O_NOFOLLOW` and `unlinkat`, after checking that the
    entry's device and inode are still what the plan saw (a handle's, for an item named by one).

#### 3. Index files

A saved scan is a SQLite file, and an index file may come from anyone: a copy someone sent, a
download, or a file planted where a scan was expected. SQLite files can carry views, triggers,
virtual tables and crafted pages.

- **Location.** An index file is read from the index directory (`--index-dir`,
  `DUSCOPE_INDEX_DIR`, or by default `$XDG_CACHE_HOME/duscope` or the platform's cache
  directory). If `--scan FILE` names a file outside it, duscope refuses before opening the file:
  `untrusted_index`, exit 1. `--trust FILE` lifts that rule for one file and changes nothing
  else.
  - Both paths are resolved first. A link inside the directory to a file outside it counts as
    outside, and `--trust` may name the file by any path to it.
- **Every open, trusted or not.** The rules are in `crates/duscope-core/src/index/reader.rs`.
  - **Mode:** read-only and immutable (`mode=ro&immutable=1`), with `SQLITE_DBCONFIG_DEFENSIVE`
    set and the schema untrusted.
  - **Disabled:** views, triggers and attached databases. A view cannot be read, even one that
    takes the name of a table duscope reads, and a trigger never fires.
  - **Limits:** statements of at most 64 KiB, the schema's included, and strings and BLOBs of at
    most 1 MB.
  - **Refused schemas:** a schema with a virtual table, or with more than 256 objects, is
    refused. A schema of millions of objects is stopped while it loads.
  - **Versions:** the major `index_version` must be this build's, and `tool_version` must name
    a duscope version.
  - **Errors:** a refused file is `index_corrupt` or `index_version_mismatch`, exit 5.
  - **Rows:** a row whose ids break the index's layout never makes a verb read for ever, and a
    walk down the tree reads each entry once however the rows lie. A verb that reads such a row
    fails `index_corrupt`, exit 5, and so does one that meets an id no row holds, at the first;
    a verb that does not may answer from the rows it read, with no warning. An `n_entries` past
    the highest id + 1 is refused when the file opens. `scans --check` reads every row: the ids
    run from 0 with none left out, and each entry lies where its parent's ids place it.
- **Indexes are snapshots.** Each index is written to a temporary file and renamed into place.
  Nothing edits an index after that.
- **Permissions.** The index directory and its `scans` and `out` directories are 0700. Each
  index file, each spill file in `out` and the handle key are 0600. duscope still reads a looser
  directory, a looser index file, or one another user owns, but it prints a warning on stderr
  with the `chmod` that fixes it.
- **The sort's temporary files.** While a save builds an index file's indexes, SQLite sorts their
  keys in about 2 MB of memory and writes the rest to a temporary file, in sorted runs. Past 16
  runs, a tree of more than about a million files, it opens a second file to merge them. Both go
  in `SQLITE_TMPDIR` if that is set and writable, else `TMPDIR`, else the first of `/var/tmp`,
  `/usr/tmp` and `/tmp` that is writable, and as a last resort the current directory.
  - **Privacy.** Each file has mode 0600 and is in no directory while it is in use, so nothing of
    them remains however the process ends: on Linux SQLite opens it with `O_TMPFILE` where the
    filesystem allows, and otherwise gives it a random name and unlinks it as it opens it. They hold the keys being sorted: a directory's name with its parent's id and its
    own (about a dozen bytes beside the name), and a file's size on disk with its id (about a dozen
    bytes in all). They never hold a file's name or a path.
  - **Where they go.** Set `SQLITE_TMPDIR` to choose the directory. The first file is about as
    large as the largest index being built, about 12 MB for a tree of a million files, and the
    second about 1 MB for every 16 runs.
  - **If it fails.** Where the temporary directory is full, or no directory will take the file,
    duscope builds the index file again with the sort in memory, and warns on stderr.

#### 4. The scanned tree

A tree is anyone's: its names, links and sizes are input, as are the files of other tools that
`import` reads.

- **Names are bytes.** The index stores them as BLOBs. JSON carries invalid UTF-8 as a lossy
  `name` and the bytes in base64 (`name_raw`). Output for people (tables, the treemap, and
  messages and logs on stderr) shows control characters as escapes (`\x1b`), so a file name in
  it cannot drive the terminal.
  SVG output escapes names as XML. Names reach an agent as they are, in JSON: a client should
  read them as data, whatever they say.
- **Links.** The scanner never follows symbolic links unless `--follow-symlinks` is given. It
  does not cross onto another filesystem with `-x`, and it does not enter network filesystems
  without `--include-network`.
- **No reads of contents.** The scanner reads file metadata only, never file contents. There are
  two exceptions:
  - the 43-byte `CACHEDIR.TAG` check;
  - `explain`'s FIEMAP ioctl.

  Besides these, it reads the two system tables that describe the layout: macOS's
  `/usr/share/firmlinks` and Linux's `/proc/self/mountinfo`. It writes nothing.

  `explain`, and so the `duscope_explain` tool, is not the scanner: for its git facts it reads
  what git reads (`.git` and what it names, `.gitignore`, `info/exclude`, the configuration and
  the file its `core.excludesFile` names, and the repository's index). It parses them and runs
  nothing. It reads each one only when it is a regular file, and never past a cap: 64 KiB for
  `.git` and `commondir`, 1 MiB per configuration file, 1 MiB for all the ignore files of one
  lookup together, 1 GiB for the index. So a FIFO or a device in a repository cannot stall
  `explain` or make it read without end. What a repository's files make it hold is bounded:
  the ignore patterns, parsed, take at most about 45 MB, and the index is held as read, 1 GiB
  at most, or 2 GiB for a split index and the shared index it names. A `.gitignore` that is a
  symbolic link is not followed, as git has not followed one since 2.32.

  A repository's own `core.excludesFile` is honoured even when it names a file outside the
  server's roots, as git honours it. A tree that sets it learns at most whether that file holds a
  pattern matching the item, and whether the pattern marks it precious, or, when the file cannot
  be read, why: its text never comes back.
- **Imports.** `import` reads ncdu's and gdu's JSON and GrandPerspective's XML with in-tree
  streaming parsers.
  - Gzip is decompressed through a 1 GiB cap.
  - Names must be names: no `/` or NUL, not `.` or `..`, at most 64 KiB, and unique in their
    directory.
  - Each format has a fuzz target in `fuzz/`.

### The MCP server

`duscope mcp` speaks JSON-RPC, one message per line, on its stdin and stdout, and has no other
transport. Stdout carries protocol messages alone. The log goes to stderr, with control
characters shown as escapes.

- **Tools.** A tool is its verb's command line, built from arguments checked against its input
  schema and through the roots: an argument the schema does not name is refused, and a value
  is joined to its flag with `=`, so it is never read as a flag.
- **Writes.** `duscope_scan` saves an index, a new file each call unless `max_age` finds a recent
  one; `scans --prune`, which no tool runs, removes old ones. A result past `--max-chars` goes
  to the index directory's `out`, and `duscope_explain` makes `handle.key` where there is none.
  With `--allow-delete`, `duscope_delete` moves entries under the roots to the Trash and never
  deletes one permanently. A root itself is refused, each item needs a handle that verifies and
  the bytes it should free, and a call is a dry run unless it says otherwise. No tool writes
  anywhere else.
- **Limits.** A message is at most 1 MiB; a longer line is refused and skipped. Calls run one
  at a time, in the order they came. At most 64 requests, holding 32 MiB between them once parsed,
  ids included, wait behind the running call: a request past either is answered at once with the
  JSON-RPC error `-31000` and never runs, and the server keeps reading, so a client can still
  cancel. A path is
  at most 4096 bytes. `duscope_scan` has 10 minutes and the other tools 30 seconds before the call
  is answered `deadline_exceeded`, counted from before its arguments are checked;
  `duscope_query`'s statement keeps the sandbox's 5 seconds. A cancelled or expired scan stops
  within one directory per walk worker. The verbs that read saved scans have no point at which
  to stop: one runs to its end, and the next call waits for it.

### No network

The binary opens no network socket. There is no telemetry and no update check, and `cargo deny`
bans the HTTP and TLS crates `reqwest`, `hyper` and `openssl` from the dependency tree.

The one socket it opens is local: while `view` or `scan -i` runs, the terminal library's event
reader (crossterm's) opens a pair of Unix-domain sockets connected only to each other, through
which its signal handler tells the event loop that the window was resized. The pair has no
address, never leaves the process and closes when it exits (DEV-70).

## Security releases

A confirmed vulnerability is fixed like this. The full procedure is in
[RELEASING.md](docs/RELEASING.md#security-releases).

1. The private report becomes a draft GitHub security advisory (GHSA). The fix is made in the
   advisory's temporary private fork, and the reporter is invited to review it.
2. A CVE is requested through GitHub, from the advisory.
3. The fix ships as a patch release of the latest minor, whose changelog has only a `Security`
   section.
4. The advisory is published with the release.
5. A `RUSTSEC` advisory is filed when a library crate (`duscope-core`, `duscope-walk`,
   `duscope-treemap`, `duscope-tui`, `duscope-mcp`) is affected, so that `cargo audit` reports
   it to the crate's other users.
