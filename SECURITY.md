# Security policy

Last reviewed: 2026-11-01, against the code that adds `--trust` (P2-SEC-01). The next review
is P5-SEC-01's, before 1.0, against the MCP server and `clean`.

duscope measures directory trees. It reads metadata, writes snapshot indexes of what it found
into its own index directory, and answers queries about them. It opens no socket. From phase 3,
an MCP server lets agents ask the same questions, and from phase 4, `clean` moves what it is
told to the trash. This page says what duscope treats as untrusted, the rule for each untrusted
input, how to report a vulnerability, and how a fix is released.

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

- **The user's files.** duscope never writes to the trees it scans. From phase 4, `clean` is the
  only command that changes anything, and only what it is told to change.
- **What the scans reveal.** Every path a scan measured, and its sizes and times, stays private
  to the user who ran the scan.
- **The machine.** No input makes duscope run code it does not ship, write outside its index
  directory and the outputs it is asked for, or use memory and time without bound.

### What is not protected

- **Anything the same user, or root, can do.** They can read the scans and the handle key, and
  run duscope with any flag.
- **A filesystem or kernel that misreports metadata.** duscope reports what it is told.
- **The build chain.** Release builds are reproducible and attested, which is how a build can
  be checked ([RELEASING.md](docs/RELEASING.md)), but this model does not cover the toolchain.

### The four untrusted inputs

#### 1. SQL

This is `query --sql`, and from phase 3 the MCP server's `query` tool, where an agent writes the
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
- **Fuzzing:** `fuzz/fuzz_targets/sql.rs` fuzzes the sandbox.

#### 2. Agent paths and handles

From phase 3, an agent names paths, and from phase 4 it asks `clean` to act on them.

- **Handles.** `explain --json` returns a handle: an HMAC-SHA256 tag over the path, its device
  and inode, its size on disk, the scan, and an expiry fifteen minutes on.
  - **The key** is 32 random bytes in `handle.key` in the index directory, mode 0600.
  - **A key that cannot be trusted** issues no handle: a symbolic link in its place, another
    user's key, or one others may read. A warning says why.
  - **Checking a handle:** the tag is compared in constant time before anything in the
    handle is read.
  - **Rejection:** a handle that was changed or has expired is refused, with `handle_invalid`
    or `handle_expired`.
- **Paths (phase 3).** Each path an agent supplies is made absolute and resolved, and is
  refused unless it lies under one of the server's `--roots`.
- **Deletes (phase 4).** `clean` acts only on a handle that verifies.
  - It is a dry run unless `--yes` is given.
  - It moves entries to the trash by default.
  - `--permanent` deletes through `openat`, `O_NOFOLLOW` and `unlinkat`, after checking that the
    entry's device and inode are still the handle's.

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
- **Indexes are snapshots.** Each index is written to a temporary file and renamed into place.
  Nothing edits an index after that.
- **Permissions.** The index directory and its `scans` directory are 0700. Each index file and
  the handle key are 0600. duscope still reads a looser directory, a looser index file, or one
  another user owns, but it prints a warning on stderr with the `chmod` that fixes it.

#### 4. The scanned tree

A tree is anyone's: its names, links and sizes are input, as are the files of other tools that
`import` reads.

- **Names are bytes.** The index stores them as BLOBs. JSON carries invalid UTF-8 as a lossy
  `name` and the bytes in base64 (`name_raw`). Output for people (tables and the treemap)
  shows control characters as escapes (`\x1b`), so a file name cannot drive the terminal. SVG
  output escapes names as XML.
- **Links.** The scanner never follows symbolic links unless `--follow-symlinks` is given. It
  does not cross onto another filesystem with `-x`, and it does not enter network filesystems
  without `--include-network`.
- **No reads of contents.** The scanner reads file metadata only, never file contents. There are
  two exceptions:
  - the 43-byte `CACHEDIR.TAG` check;
  - `explain`'s FIEMAP ioctl.

  Besides these, it reads the two system tables that describe the layout: macOS's
  `/usr/share/firmlinks` and Linux's `/proc/self/mountinfo`. It writes nothing.
- **Imports.** `import` reads ncdu's and gdu's JSON and GrandPerspective's XML with in-tree
  streaming parsers.
  - Gzip is decompressed through a 1 GiB cap.
  - Names must be names: no `/` or NUL, not `.` or `..`, at most 64 KiB, and unique in their
    directory.
  - Each format has a fuzz target in `fuzz/`.

### No network

The binary opens no socket. There is no telemetry and no update check, and `cargo deny` bans
HTTP and TLS crates from the dependency tree.

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
