# Limits

What duscope cannot measure, or measures only within bounds, and why; with what
to do where something can be done. Some limits come from the operating systems
and hold for any disk-usage tool; the others are limits of duscope as it is
built. The warnings named here are explained in
[troubleshooting.md](troubleshooting.md). Where duscope differs from what a
`du` user expects (`-x` inside a macOS volume group, `--exclude-caches`
markers, patterns and case), see CAVEATS in `duscope man`.

## Shared blocks

### APFS clones written to since they were cloned

duscope counts an APFS clone once, by default: files that share all their
blocks under one clone id are charged to one of them, and the others count
nothing. `--no-clones` counts each in full, as `du` does, and `summary`'s
`du_compatible_bytes` gives that figure either way.

A clone written to since it was cloned has blocks of its own and still shares
the rest, and APFS says only that such a file may share blocks, not which.
duscope counts it in full and marks every directory above it as an upper
bound: the `upper_bound` warning, the `upper_bound` flag on those rows, and
`freeable_kind: upper_bound` from `explain`. Deleting the file frees its own
blocks, somewhere between nothing and its size.

### Reflinks on Linux

Linux filesystems report no clone ids, so a scan counts every reflinked copy
(`cp --reflink`, deduplicated ranges, and on btrfs the extents a file shares
with a snapshot) in full, as `du` does. Nothing marks such totals as upper
bounds: on btrfs and XFS a total can be larger than the space its files take,
with no warning. `--clones` on Linux only adds the warning `clones_unavailable`.

`duscope explain --clones PATH` measures the sharing where it matters: it opens
each file under the path read-only for its extent map (FIEMAP), without reading
the file, and takes the shared bytes off what deleting the path frees, as a
lower bound. The map says that an extent is shared, not with which file.

### Snapshots

Deleting a file frees nothing that a snapshot still holds, and duscope reads
the live tree, never the snapshots: what `explain` and `clean` report a deletion
frees is the file's bytes on the volume, and the free space can grow by less
until the snapshots holding it go.

- **Time Machine, macOS.** Time Machine takes a local snapshot of the startup
  disk about every hour and keeps each for 24 hours, and one of the last
  successful backup until its space is needed; it deletes snapshots sooner when
  the space is needed for other things. `tmutil listlocalsnapshots /` lists
  them, and `tmutil deletelocalsnapshots` removes them.
- **btrfs and ZFS, Linux.** A snapshot keeps every block it shares until the
  snapshot is deleted. For each ZFS dataset under the path, `summary` gives its
  `referenced_bytes`, not the space its snapshots use (`usedbysnapshots`),
  which takes the pool's own statistics that `zfs list` reports and duscope does
  not read.

## Access

### Full Disk Access is a heuristic (macOS)

macOS has no API that tells a process whether it has Full Disk Access. Before a
scan whose walk reaches your `~/Library`, duscope tries to open
`~/Library/Mail` and `~/Library/Safari`, and warns `full_disk_access_missing`
when the privacy protections refuse both. So:

- When either folder is missing, or opens, there is no warning, whatever the
  grant.
- Only the home in `HOME` is probed: a scan that does not reach your own
  `~/Library` (another user's home alone, say) gives no warning.
- The warning comes with the scan that probed; a saved scan read later does not
  repeat it.

Whatever the probe says, every folder macOS refuses is counted as unreadable,
and `partial_read` reports it. The grant belongs to the app that runs duscope
(a terminal app, an editor's terminal, the client that starts `duscope mcp`),
never to duscope.

### Folders no process may read (macOS)

Some folders macOS keeps from every process but its own, root included (its
data vaults). A scan counts them as unreadable (`partial_read`), and their
bytes are not in the totals.

### `~/Library/Containers` can be slow (macOS)

Since macOS 14, reading `~/Library/Containers` and `~/Library/Group Containers`
can take minutes, a cost of the system's access checks on app containers that
duscope cannot avoid while it reads them. `--skip-containers` lists the two
folders without entering them: their bytes are then left out of the totals,
and `scan.excluded` names them with the reason `skipped_containers`.

## Change history

### FSEvents history is advisory (macOS rescans)

On macOS, `rescan` reads again only the directories the volume's FSEvents
history reports changed since the saved scan. macOS keeps that history for a
time it does not document, and says nothing when the history no longer reaches
back to a given point. So duscope checks the history before it trusts it, and
again before it saves. A check that fails makes it read the whole tree, and
`scan.fallback_reason` names the check; for a volume mounted below the scan's
root, it reads that volume's part whole instead and records no reason. The
checks:

- `index_too_old`: the saved scan is older than `--max-age` (7 days by
  default), or, taken with `--attrs full`, was itself an incremental rescan.
- `volume_changed`: a volume's history is not the one the scan recorded
  (another device, another FSEvents database), or the scan recorded none.
- `fsevents_disabled`: logging is off on the volume (`.fseventsd/no_log`), or
  the volume is neither APFS nor HFS+. The history of a FAT or exFAT volume,
  whose coarse times need not change when a directory's entries do, and of a
  network volume, which holds none of the changes other machines make, is
  never used.
- `history_gone`: the history cannot stand in for the scan: the system is
  another build (an update of macOS makes the next rescan a full one), the
  event ids went back or wrapped, the volume was mounted again since, the
  history was not read to its end within 30 seconds, or the scan followed
  symbolic links.
- `events_dropped`: FSEvents dropped events since the scan.
- `too_many_changes`: the history reports more than 15% of the scan's
  directories.
- `options_changed`: another version of duscope took the saved scan, or a file
  its options name (`--exclude-from`, `--ignore-from`, the configuration's
  cache rules) changed since.
- `post_check_failed`: the root, or a volume's mount point below it, is no
  longer the directory the scan read; or, before saving, it reads again 1% of
  the directories it took from the saved scan (at least 1,000, or all of them
  when there are fewer), and every file it took that shares its blocks or its
  inode, and one of them changed where the history reports no change.

What it cannot see: on the startup disk's Data volume, changes made from
recoveryOS or another system between a shutdown and the next startup (CAVEATS
in `duscope man`). FSEvents records no reads either, so a rescan of a scan
taken with `--attrs full` keeps the access times that scan read for what it
takes from it, and `stale --atime` judges those files by the times read then.
`duscope rescan --full` always reads the whole tree.

### A Linux rescan reads the whole tree

Linux keeps no persistent change history, so `rescan` there scans the whole
tree again with the saved scan's options, and `rescan --incremental` fails with
`not_available`. `duscope diff OLD latest --under ROOT`, with `OLD` the saved
scan's id and `ROOT` its root, then shows what changed: without `--under`,
`latest` is the newest scan that holds the working directory.

### `scans --check` sees structure, not contents

`duscope scans --check` compares each directory's modification and change times
with the scan's. An entry added, removed or renamed in a directory, or a change
to the directory itself (its mode, owner or links), makes the scan stale
(`index_stale`), and so does a directory changed while the scan ran, whatever it
reads now. A file written in place under an unchanged directory does not.

The index directory and everything under it are left out, since every save
writes there; the scan's root never is. The first save into an index directory
that does not exist yet creates it, which changes its parent: where the scanned
tree holds that parent, that one scan checks stale.

## Size and memory

### The whole tree is held in memory

A scan holds every entry in memory until the walk is over and the index is
written: a 48-byte record per entry and another per directory, beside every
name and the smaller tables. Nothing streams entries into the index during the
walk, so a tree too large for the machine's memory cannot be scanned in one
piece. Scan its parts one at a time, or leave large subtrees out of the scan
with `duscope scan --exclude` or `-x`. (On the list verbs, `--exclude` leaves
rows out of the answer, not directories out of the scan.)

### At most 4 GiB of names

The names of a scan's entries are stored together, and their total cannot pass
4 GiB; nor can a scan hold more than about 4.3 billion entries. A larger tree
makes the scan fail with `io_error` ("the scan is too large for the arena: the
names fill 4 GiB") once its walk is over, and nothing is saved. With names of 20
bytes on average, the names fill 4 GiB at about 200 million entries.

### Fixed limits on input

Some inputs have a fixed limit, so that a client or a file cannot make duscope
hold or do without bound; they are listed in
[SECURITY.md](../SECURITY.md#limits-on-memory-and-time), and the MCP server's
in [mcp.md](mcp.md#limits). Among them: an MCP message of at most 1 MiB; at
most 64 requests, or 32 MiB of them, waiting behind the running call, past
which a request is answered `-31000` ("the server is busy"); a statement of
`query --sql` within its `--timeout` (5 seconds by default, 60 at most) and
256 MiB of SQLite's heap; and a compressed file `import` reads, 1 GiB once
decompressed. A scan has no such limit: nothing caps its depth, nor its
entries short of what the arena holds (above), and the time a chain of very
deep directories takes grows faster than its depth.

## Filesystems

### Automount points (Linux)

An automount point the kernel has not mounted yet (an NFS submount, debugfs's
`tracing`) is listed and not entered, since opening it would mount a
filesystem: only `--include-network` without `-x` enters it, and so mounts it.
Nothing records the point: it has no row in `scan.volumes`, adds nothing to
the `mounts_excluded` warning and carries no flag, so it reads as an empty
directory.

Under `--follow-symlinks`, a symbolic link that leads to such a point mounts
it when the scan follows the link, whatever `-x` and `--include-network` say;
whether the scan then enters what was mounted follows the usual rules.

### No `--one-subvolume` (Linux)

`-x` keeps a scan on the root's mount. A btrfs subvolume or a ZFS dataset
mounted on its own is a mount, which `-x` does not enter; a btrfs subvolume
that is only a directory inside its parent's mount is not, and `-x` enters it.
No flag keeps a scan inside one subvolume. `summary` gives the bytes on each
subvolume under the path in `subvolumes[]`, and the rows of their top
directories carry the flag `subvolume`.

### overlayfs: what the Trash cannot take (Linux)

On an overlayfs mount `clean` plans files and directories as on any volume. The
Trash that takes them is the overlay's: the home Trash when the home is on the
overlay, otherwise `.Trash/<uid>` or `.Trash-<uid>` at the overlay's top. A move
to the Trash renames the item inside the overlay, and overlayfs first copies an
item that comes from a lower layer up into the upper one ([the kernel's
overlayfs documentation](https://docs.kernel.org/filesystems/overlayfs.html)).
So, for an item from a lower layer:

- What it frees is overstated, for the Trash and `--permanent` alike: the lower
  layer is read-only and keeps the item's bytes. Emptying the Trash deletes the
  copy in the upper layer, and `--permanent` leaves a whiteout there that hides
  the item; `freed_bytes` counts the item's blocks either way.
- A file with more than one hard link is not moved the first time, unless the
  overlay is mounted with `index=on`: the copy-up makes it another inode, so the
  move is undone and the item fails `path_changed`, left where it was. It has
  been copied up by then, so running the same `clean` again moves it.
- A directory is moved only when the overlay is mounted with `redirect_dir=on`;
  otherwise the rename fails across the layers and the item fails
  `trash_unavailable`, left where it was.

Where the system cannot tell whether a file is the root of a mount (no mount-root
attribute from `statx` before Linux 5.8, no file handles, and no readable
`/proc/self/mountinfo`), a file of an overlay whose layers lie on other
filesystems, which reports another device than its directory, is refused as
`mount_point`.

### Paths that are not UTF-8 (Linux)

A Linux name can be any bytes, and JSON holds text. Each row's `path` and
`name` is the text with invalid bytes replaced, and `path_raw` and `name_raw`
carry the exact bytes in base64 (standard, padded) when they are not UTF-8:
decoded, they are the path to give a command on the command line. `--plain`
writes each row's path as its bytes. Some fields have no such twin and stay
lossy, among them `scan.root`, `scan.excluded[].path`, `query.path`, `via`,
`mount_point`, the commands in `next`, and the paths `scans` lists.

Over MCP such a path cannot be passed back: a tool takes a path as a JSON
string, which holds text only, and the one tool that takes a handle instead,
`duscope_delete`, takes it from `duscope_explain`, which takes a path. So no
tool can be asked about an entry whose path is not UTF-8. A tool can still be
asked about a directory above it whose path is.

## Output

### `--limit` does not apply to `--format ndjson`

Under `--format ndjson` the list verbs (`top`, `tree`, `by-type`, `stale`,
`caches`) and `diff` write every row, as `--all` does: `--limit` is not read,
so `--limit 2` writes all of them. Read only the lines needed, or leave out
`--format ndjson` for a page of `--limit` rows.

### A stream cut short has no `end` line

With `--format ndjson` or `--plain`, `top` and `stale` write each row as they
read it from the saved scan. A read that fails after rows were written (the
file damaged, or overwritten while it is read: `index_corrupt`) does not stop
the rows: every row after it is still written, empty (its `path` the scan's
root, its `name` empty, its sizes 0), and only then does the output end:
`ndjson` has no `end` line, and the error envelope follows on stdout;
`--plain`, which has no trailer, writes the error on stderr. The
warnings go with it: `ndjson` carries them only in its `end` line, and
`--plain` writes them on stderr after the rows. Act on streamed rows only once
the `end` line has come, or, for `--plain`, once the command has exited 0.

## Deleting

### A btrfs subvolume is a mount point to `clean` (Linux)

A btrfs subvolume reports a device of its own, another than its parent
directory's, and `clean` tells a mount point by its device: a subvolume named
as an item is refused as `mount_point`, and `--permanent` stops at one under an
item (`refused_path`). The hint's `umount` does not apply to a subvolume that
is not mounted on its own; `btrfs subvolume delete` deletes one.

### The Trash on NFS, 9p and FUSE (Linux)

`clean` moves an item into the Trash with `renameat2` and `RENAME_NOREPLACE`,
so that the move never replaces an entry already there. Some filesystems
refuse that flag: NFS, 9p and Ceph, and a FUSE filesystem whose server does
not offer it (NTFS through ntfs-3g among them). There duscope first looks for
the name, and renames when nothing is there, as `mv -n` does. An entry another
program makes under that name in between is replaced: in the Trash, on the way
in, and at the item's own name when what was moved proves to be another entry
and is moved back. Only a program writing in that same directory at that
moment can bring this about.

## The interactive viewer

### `kill -INT` and `kill -QUIT` leave the terminal raw

The viewer gives the terminal back (raw mode off, the main screen, mouse
reporting off) when it ends by `q` or `Ctrl-C`, an error, a panic, or the
signals `SIGTERM` and `SIGHUP`. In the viewer `Ctrl-C` and `Ctrl-\` are keys,
not signals; a `SIGINT` or `SIGQUIT` sent from outside it (`kill -INT`,
`kill -QUIT`) is not answered, and where it ends the viewer the terminal is
left as the viewer had it. `reset` gives it back. To end the viewer from
outside, send `SIGTERM`, `kill`'s default.
