# Troubleshooting

What duscope's warnings and errors mean, why each happens, and what to do about
it. Every warning code and every error code has an entry under its own heading,
so `troubleshooting.md#partial_read` leads to `partial_read`. The limits behind
several of them are in [limits.md](limits.md).

## Reading a warning or an error

A warning does not stop a command: the result is printed, and the warning
follows it. On a terminal it is two lines on stderr, the message and a command
that helps:

```text
warning: 3 directories could not be read; the totals are a lower bound
hint: Rerun duscope as a user who can read them, or with --strict to fail instead
```

In the JSON envelope (`--json`, and every answer to an MCP client) each warning
is an object in `warnings`: `code`, `message`, `count` (how many things it is
about, or null) and `hint` (or null). `-q` keeps warnings off stderr; the
envelope keeps them. A warning never changes the exit status, except
`partial_read` under `--strict`.

An error ends the command with a non-zero exit status. On a terminal it is the
message on stderr, then a `hint:` line:

```text
duscope: /srv/data does not exist
hint: Check the path, or run duscope scan in the directory to measure
```

In JSON, stdout gets `"ok": false` and an `error` object: `code`, `message`,
`hint`, `retryable` (whether running the same command again may succeed) and
`fixable_by` (`user`, `agent` or `none`). The exit statuses:

| Status | Meaning |
| --- | --- |
| 0 | success, partial reads included |
| 1 | a runtime error |
| 2 | a usage error |
| 3 | the path, or a saved scan asked for, does not exist |
| 4 | permission denied on the root, or unreadable directories under `--strict` |
| 5 | a saved scan is damaged, or written in an index version this duscope cannot read |
| 130 | interrupted (Ctrl-C) |

## Warnings

### `partial_read`

Directories under the path could not be read, so the totals leave out what they
hold: they are a lower bound. `count` says how many. A directory is unreadable
when its permissions keep the user out, when macOS's privacy protections refuse
it (see [`full_disk_access_missing`](#full_disk_access_missing)), when it is
one of the macOS folders that refuse every process
([limits.md](limits.md#folders-no-process-may-read-macos)), or when reading it
fails otherwise (an I/O error, for one). Each such directory is in the rows with
the flag `unreadable` and the sizes of its own entry.

`clean` gives the same code for an item its plan could not read whole while it
measured it: what that item frees is unknown (`frees_bytes` is null), so no
`--expect-bytes` is met.

What to do:

- `duscope explain --errors SCAN` lists the directories and entries a saved
  scan could not read, each with its error; `--debug` prints each one as the
  scan meets it.
- Scan again as a user who can read them, or, on macOS, grant the Full Disk
  Access the warning `full_disk_access_missing` asks for.
- To fail rather than answer with a lower bound, give `--strict` to a list verb
  (`top`, `tree`, `by-type`, `stale`, `caches`): it exits 4
  (`permission_denied`) when a directory under the path could not be read.
- For `clean`, make what is under the item readable, then run `duscope clean`
  again.

### `mounts_excluded`

The scan met mounted filesystems under the path and did not enter them, so what
they hold is not in the totals. `count` says how many, and `scan.volumes` names
each with its `reason`:

| `reason` | The volume | To enter it |
| --- | --- | --- |
| `network` | NFS, SMB, FUSE, 9p, AFP or autofs; on Linux also an automount point the kernel has not mounted yet | `--include-network` (for an automount point, without `-x`) |
| `one_file_system` | any mount other than the root's, under `-x` | leave out `-x` |
| `kernel` | proc, sysfs, cgroup and the other kernel pseudo-filesystems, and macOS's devfs | never entered |
| `bind_cycle` | a bind mount of a directory already on the path, which would loop | never entered |
| `excluded_cache` | a mount point holding a `CACHEDIR.TAG`, under `--exclude-caches` | leave out `--exclude-caches` |

What to do: nothing, when the volume is not part of the question. Otherwise
scan again with the flag the table gives.

### `vanished`

Entries disappeared between the scan listing their directory and reading them:
something (a build, a package manager, a browser's cache) was changing the tree
while it was read. `count` says how many, for the whole scan: a saved scan does
not record where they were, so a verb that reads part of it gives the whole
scan's count. A directory that disappeared after its parent listed it keeps its
row, empty, and counts both among the directories and among the vanished.

What to do: scan again once the tree is quiet.

### `full_disk_access_missing`

macOS only. The privacy protections refused the scan two folders of the home,
`~/Library/Mail` and `~/Library/Safari`, so the folders they protect are left
out of the totals and counted as unreadable (`partial_read`). The grant belongs
to the app that runs duscope, not to duscope: the terminal app, or for
`duscope mcp` the app that runs the server. The message names that app when
duscope can tell it.

Full Disk Access has no API. Before a scan whose walk reaches your `~/Library`
(a scan of the home, of a directory above it such as `/`, or of `~/Library`, by
any of their paths), duscope tries to open both folders, and it warns when the
privacy protections refuse both. The warning comes with the scan that ran the
probe; a saved scan keeps the unreadable folders, not the warning. What the
probe cannot tell is in
[limits.md](limits.md#full-disk-access-is-a-heuristic-macos).

What to do: open System Settings > Privacy & Security > Full Disk Access, turn
it on for the app the message names (add it with `+` if it is not listed), quit
and reopen that app if macOS asks, then scan again. For an MCP client, restart
the client, then call `duscope_scan` again.

### `dataless_present`

macOS only. Files under the path are cloud placeholders (iCloud Drive,
Dropbox, OneDrive) whose content is not on this disk: they count at their
logical size in apparent sizes, they take no space here, and deleting them
frees nothing on this disk. `count` says how many. duscope reads their metadata
only and never downloads them.

What to do: nothing; totals of bytes on disk leave them out already.
`duscope summary PATH` gives their logical size as `dataless_bytes`.

### `upper_bound`

Some files under the path may share blocks with other files, so the totals may
count those blocks more than once: they are an upper bound. On APFS this is a
clone written to since it was cloned, which still shares some of its blocks;
APFS says only that it may share blocks, so duscope counts it in full and marks
every directory above it. Those rows carry the flag `upper_bound`, and
`explain` says `freeable_kind: upper_bound`. The warning has no hint: nothing
needs fixing, but a deletion there frees less than the figure may suggest.
[limits.md](limits.md#apfs-clones-written-to-since-they-were-cloned) has the
details, and why Linux reflinks raise nothing.

### `clones_unavailable`

`--clones` was given, and the scan read a filesystem that reports no clone ids:
every filesystem on Linux, and on macOS any volume but APFS. There clones count
in full, as `du` counts them. APFS clones are counted once without the flag,
which is the default.

What to do: leave out `--clones` where it cannot apply. On Linux,
`duscope explain --clones PATH` sizes the extents the files under a path share
(btrfs and XFS reflinks).

### `stale_index`

The answer comes from a saved scan that finished longer ago than `--max-age`
(7 days unless the flag, or `max_age` under `[defaults]` in `config.toml`, says
otherwise): it is the tree as it was then. Every verb that reads a saved scan in
place of scanning can give it: the list verbs, `summary`, `explain`, `clean`,
`query`, `export`, `render` and `view`. They read the newest saved scan that
holds the path (the list verbs, one taken with the options they would scan
with), however old: `--max-age` decides only the warning.

What to do: `duscope rescan PATH` measures the tree again with the saved scan's
options (on macOS reading only what changed, where it can), or
`duscope scan --save PATH` takes a new scan; verbs then read the new one. To
accept an older scan, give a longer `--max-age`.

### `scan_age`

`stale` answered from a saved scan older than a tenth of the window it asks
about (`--older-than`, 180 days by default, so 18 days). It judges files by the
times the scan read: a file modified or removed since is still listed as the
scan saw it.

What to do: `duscope rescan PATH`, then `duscope stale` again.

### `atime_unreliable`

`stale --atime` judges files by when they were last read, and a volume the scan
entered is mounted `noatime`, `relatime` or `lazytime`, under which reading a
file does not always update its access time: a file read recently can look
untouched. `count` says how many such volumes.

What to do: `duscope stale` without `--atime`, which judges files by their
modification and change times.

### `tz_unavailable`

`--since` or `--until` was given a date as `YYYY-MM-DD`, which means midnight in
the local time zone, and the system has no zoneinfo for that zone (a minimal
container, or `TZ` naming a zone the system lacks), so the date was read as
midnight UTC.

What to do: install the time-zone database (the `tzdata` package on Debian and
Ubuntu), or give the date in RFC 3339 with its offset
(`2026-01-01T00:00:00+02:00`).

### `excluded_truncated`

A list names in `scan.excluded` at most the first 20 directories, in the scan's
order, that the excludes (`--exclude`, `--exclude-from`, `--ignore-from`,
`--exclude-caches`, `--skip-containers`, the configuration's `[excludes]`) kept
the scan out of under the path. `count` says how many more there are;
`scan.excluded_count` is the whole scan's number.

What to do: the command in the hint (`duscope summary --scan ID PATH --json`)
lists every one under the path.

### `spilled`

The JSON envelope was longer than `--max-chars` (24,000 characters by default,
about what an agent's tool reads inline), so it went to a file under the index
directory's `out/`, mode 0600, and stdout got a short envelope instead:
`spilled` names the file with its rows and bytes, and `preview` holds the first
five rows. `count` is the number of rows. `scans --prune` removes such files
after seven days.

What to do: read the file; or ask for less (`--fields`, a smaller `--limit`);
or write the whole envelope where you choose with `-o FILE`, which no limit
applies to. `--max-chars 0` turns the limit off.

### `changed_since_scan`

`clean` found items that differ from the saved scan's rows (their size, times or
links, or for a directory what it holds), so its plan measured them from the
tree as it is now. `count` says how many. A directory measured that way is an
upper bound in the plan. An item given as a `--from` handle that has changed
fails instead ([`path_changed`](#path_changed)).

What to do: review the plan. The hint's command (`duscope rescan`) measures the
tree again, so that the next plan starts from it.

### `index_untouched`

`clean` moved items to the Trash or deleted them, or the interactive viewer
moved them to the Trash, and the saved scans still hold them: a saved scan is
never changed. Until the tree is measured again, answers from those scans still
count what went.

What to do: the command in the hint, `duscope rescan` (for a tree read by
`import`, which cannot be rescanned, `duscope scan --save` of its root).

### `attrs_not_collected`

The conventions list this code among the warnings, but duscope raises it as an
error, exit 1. The command needs an attribute that only a scan with
`--attrs full` collects (the access or birth time, mode, owner or group), and
the scan it read has the basic attributes: `stale --atime`, `--time atime` or
`btime` with a time filter, `--fields` naming `atime`, `btime`, `mode`, `uid` or
`gid`, or `export --format qdirstat`.

What to do: run the command again with `--attrs full`. A verb given it reads
only saved scans that have the full attributes, and scans when there is none.

### `deprecated_flag`

Reserved for a flag that is deprecated: such a flag keeps working for one minor
release, with this warning naming what replaces it, before it is removed. No
flag of this release is deprecated, so no command gives it.

## Errors

### `usage`

Exit 2. The command line is wrong: an unknown flag or verb, a missing or bad
value (`m` in a duration, which could be minutes or months), flags that
conflict, or a `--limit` above 100 without `-o FILE`, `--all` or
`--format ndjson`. The message says which.

What to do: correct the command; `duscope VERB --help` lists the verb's flags.

### `scan_not_found`

Exit 3. The path does not exist, or the saved scan asked for cannot be found:
`--scan` names no saved scan (or an id prefix several share, which is `usage`),
`--no-scan` was given and no saved scan holds the path, `--under` names a path
the scan does not hold, or `clean` found no saved scan holding its items.

What to do: check the path; `duscope scans` lists the saved scans with their
ids and roots.

### `permission_denied`

Exit 4. The directory to scan cannot be listed by this user; or a list verb was
given `--strict` and directories under the path could not be read
([`partial_read`](#partial_read)).

What to do: scan a directory you can read, or scan as a user who can read it;
without `--strict` the list verbs answer with the readable part.

### `io_error`

Exit 1, and running the command again may succeed. Reading or writing failed in
a way the other codes do not name: the root cannot be read for a reason other
than permission (an I/O error, a loop of links), the working directory cannot be
read, a move to the Trash failed for a reason other than a volume without one
(on macOS, no permission or a locked file), the envelope could not be written to
its spill file, or the scan was too large to hold
([limits.md](limits.md#at-most-4-gib-of-names)). The message names what failed.

What to do: fix what the message names, then run the command again.

### `interrupted`

Exit 130. Ctrl-C stopped the command, while it read the tree or while it saved
the scan, which then saved nothing. It prints one line on stderr and no
envelope. Under `duscope mcp`, a tool call cancelled or past its deadline before
`clean` acted gets it too: nothing was moved or deleted.

What to do: run the command again.

### `index_write_failed`

Exit 1, and running the command again may succeed. The result was printed, but
the scan could not be saved: there is no index directory (neither `HOME` nor
`XDG_CACHE_HOME` is set), or it cannot be written (permissions, a full disk).
The scan id in the output then names no file. Under `duscope mcp` the error
replaces the result, so a client never holds an id that names nothing.

What to do: give `--index-dir` (or `DUSCOPE_INDEX_DIR`) a writable directory, or
`--no-save` to scan without saving.

### `index_corrupt`

Exit 5. A saved scan's file is damaged: SQLite cannot read it, or its rows do
not hold the layout duscope writes. `duscope scans --check SCAN` runs SQLite's
own check on it. When duscope looks for the newest saved scan it passes over a
file it cannot open, and `duscope scans` names such a file on stderr.

What to do: remove the file (`scans/<id>.db` in the index directory) and scan
the tree again, `duscope scan --save PATH`.

### `index_version_mismatch`

Exit 5. The saved scan was written in an index format whose major version this
duscope does not read: it reads `index_version` 2.x
([index-format.md](index-format.md)), and the file is of another major version,
written by an older or a newer duscope.

What to do: scan the tree again with this duscope, or read the file with the
duscope that wrote it.

### `untrusted_index`

Exit 1. `--scan` names an index file outside the index directory. An index file
is data from whoever wrote it, so duscope reads one from elsewhere only when
`--trust` names it ([SECURITY.md](../SECURITY.md)).

What to do: if you trust the file, run the command again with
`--trust FILE`, as the hint spells it.

### `index_stale`

Exit 1, from `duscope scans --check` only: a directory of the tree changed since
the saved scan was taken (an entry added, removed or renamed in it, for one),
and the line on stderr names the first. The file itself is sound. What the
check sees and misses is in
[limits.md](limits.md#scans---check-sees-structure-not-contents).

What to do: the command in the hint, `duscope rescan`, or for an import
`duscope scan --save` of its root.

### `incompatible_scans`

Exit 2. `diff` was given two saved scans with different roots and no `--under`,
or an `--under` path that one of them does not hold.

What to do: give `--under` a path both scans hold.

### `invalid_cursor`

Exit 2. `--cursor` is not a cursor duscope gave, or it pages another scan or
another query: the verb, path, depth, sort and filters must be those of the
command that returned it.

What to do: run the command without `--cursor` for the first page, then page
with the `next_cursor` it returns.

### `unknown_field`

Exit 2. `--fields` names a field the verb's rows do not have; the message lists
the ones they have.

What to do: `duscope VERB --fields help` lists them.

### `sql_denied`

Exit 1. `query --sql` refused the statement: it is not one read-only `SELECT`
over the tables `duscope schema` lists, or it needed more memory than
`query --sql` allows, or a value larger than it allows.

What to do: rewrite it as one `SELECT` of those tables;
[index-format.md](index-format.md) describes them.

### `sql_timeout`

Exit 1, and running it again may succeed. The statement ran past `--timeout`
(5 seconds by default, 60 at most).

What to do: narrow the statement (a subtree with `under(id, :root_id)`, a
`LIMIT`), or give a longer `--timeout`.

### `not_available`

Exit 2. What was asked cannot be done here: duscope runs on Linux and macOS
only; or `rescan --incremental` was asked where only a full read would do, which
on Linux is always, since Linux keeps no change history, and on macOS when a
check of the history fails (the message gives the reason).

What to do: `duscope rescan` without `--incremental` reads the whole tree, and
`duscope diff OLD latest` shows what changed.

### `not_compiled`

Exit 2. `--treemap pixels` needs the `pixels` feature, which no prebuilt package
has.

What to do: `cargo install duscope --locked --features pixels` (Rust 1.90 or
later), or `--treemap always` for the map in cells.

### `confirmation_required`

Exit 1. `clean` without `--yes` is a plan, and it says so with this code:
nothing was moved or deleted. On a terminal, unless `--json`, `--plain` or
`--no-input` is given, it shows the plan and asks for `yes` once; another answer
ends here too.

What to do: review the plan, then run the same command with `--yes`.

### `refused_path`

Exit 1. `clean` refused an item, and nothing was moved or deleted. Each refused
item gives its `reason`:

- `symlink`: the item is a symbolic link.
- `firmlink`: the item is a macOS firmlink.
- `mount_point`: the item is a mount point, or, for the Trash, a directory with
  a mount point anywhere under it, or one where the mount table cannot be read:
  a move would take the mounted volume into the Trash. On Linux an overlayfs
  file can be refused this way too
  ([limits.md](limits.md#overlayfs-without-xino-clean-refuses-files-as-mount-points-linux)).
- `root`: the item is the saved scan's root, or `/`.
- `outside_root`: the item is not under the saved scan's root.
- `read_only_volume`: the item's volume is mounted read-only.
- `expectation_mismatch`: under `duscope_delete`, the item frees other than the
  client expected.

`--permanent` also stops at a mount point, or a directory it cannot tell is not
one, under an item: that item fails `refused_path`, and what was removed before
it stays removed. Under `duscope mcp`, a path outside `--roots`, or longer than
4096 bytes, is refused with this code ([mcp.md](mcp.md#the---roots-rule)).

What to do: `--skip-refused` acts on the other items. For a mount point, unmount
it first, as the hint says.

### `expectation_mismatch`

Exit 1. The plan frees bytes not within 1% of `--expect-bytes`, or an item could
not be read whole, so what it frees is unknown: nothing was moved or deleted.

What to do: run `duscope clean` without `--yes` and review the plan; when the
tree changed since the figure was taken, take the figure again.

### `trash_unavailable`

Exit 1. An item's volume has no Trash duscope can use, and the item was left
where it is: on macOS a volume that has no Trash; on Linux a volume whose
`.Trash/<uid>` or `.Trash-<uid>` at its top cannot be used or made, or no home
Trash when neither `XDG_DATA_HOME` nor `HOME` is set. On Linux duscope only ever
renames an item into a Trash on its own filesystem, never copies it.

What to do: `--permanent` deletes the item instead, as the hint says; only when
you mean it.

### `handle_invalid`

Exit 1. The `--from` handle is not one this duscope issued: it is garbled, or
the key that signed it (`handle.key` in the index directory) was removed or is
another index directory's.

What to do: run `duscope explain --json` on the path again for a fresh handle.

### `handle_expired`

Exit 1. The `--from` handle is more than 15 minutes old.

What to do: run `duscope explain --json` on the path again for a fresh handle.

### `path_changed`

Exit 1. The path of a `--from` handle no longer holds what `explain` examined:
another file or directory is there (another device and inode), or the same one
changed since the saved scan.

What to do: rescan the tree that holds it (`duscope rescan`), then run
`duscope explain --json` again for a handle of what is there now.

### `deadline_exceeded`

Exit 1, under `duscope mcp` only: a tool call ran past its deadline
(`duscope_scan` 10 minutes, the other tools 30 seconds), and a scan stopped
then saved nothing ([mcp.md](mcp.md#limits)).

What to do: call the tool again on a smaller directory, or run the command in a
terminal, where no deadline applies.

## Other messages

Some lines on stderr carry no code:

- `duscope: warn: the saved scans are not private: …`: the index directory, its
  `scans` directory or an index file can be read by other users, or belongs to
  another user, and the saved scans name every path they measured. Where the
  mode is the cause, the line names the `chmod` that makes them private again.
- `duscope scans` writes a `duscope: warn:` line for each file in the index
  directory it cannot read as an index, and lists the others (see
  [`index_corrupt`](#index_corrupt)).
