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
back to a given point. Before it trusts the history, duscope checks:

- the saved scan is younger than `--max-age` (7 days by default);
- the volume's history is the one the scan recorded, its logging is on, the
  system is the same build (an update of macOS makes the next rescan a full
  one), and the event ids have not gone back;
- no events were dropped, the history was read to its end within 30 seconds,
  and it reports at most 15% of the scan's directories;
- the options it scans with are the saved scan's; and network, FAT and exFAT
  volumes never have their history used.

Then, before saving, it reads again 1% of the directories it took from the
saved scan (at least 1,000, or all of them when there are fewer), and every
file it took that shares its blocks or its inode. A check that fails makes it
read the whole tree, and `scan.fallback_reason` says why; for a volume mounted
below the scan's root, it reads that volume's part whole instead. What it cannot
see: on the startup disk's Data volume, changes made from recoveryOS or another
system between a shutdown and the next startup (CAVEATS in `duscope man`).
`duscope rescan --full` always reads the whole tree.

### A Linux rescan reads the whole tree

Linux keeps no persistent change history, so `rescan` there scans the whole
tree again with the saved scan's options, and `rescan --incremental` fails with
`not_available`. `duscope diff OLD latest` then shows what changed.

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
piece. Scan its parts one at a time, or leave large subtrees out with
`--exclude` or `-x`.

### At most 4 GiB of names

The names of a scan's entries are stored together, and their total cannot pass
4 GiB; nor can a scan hold more than about 4.3 billion entries. A larger tree
makes the scan fail with `io_error` ("the scan is too large for the arena: the
names fill 4 GiB") once its walk is over, and nothing is saved. With names of 20
bytes on average, the names fill 4 GiB at about 200 million entries.

## Filesystems

### No `--one-subvolume` (Linux)

`-x` keeps a scan on the root's mount. A btrfs subvolume or a ZFS dataset
mounted on its own is a mount, which `-x` does not enter; a btrfs subvolume
that is only a directory inside its parent's mount is not, and `-x` enters it.
No flag keeps a scan inside one subvolume. `summary` gives the bytes on each
subvolume under the path in `subvolumes[]`, and the rows of their top
directories carry the flag `subvolume`.

### overlayfs without `xino`: `clean` refuses files as mount points (Linux)

On an overlayfs mount whose layers lie on different filesystems, a file reports
the device of the layer it comes from, not the overlay's, unless the overlay
uses `xino` and the file's inode number fits it ([the kernel's overlayfs
documentation](https://docs.kernel.org/filesystems/overlayfs.html)). `clean`
tells a mount point by a device other than its directory's, so it refuses such
a file, named as an item, as `mount_point`. A directory reports the overlay's
device, so a directory there is not refused, and moving or deleting it takes
its files with it.

### Paths that are not UTF-8 (Linux)

A Linux name can be any bytes, and JSON holds text. Each row's `path` and
`name` is the text with invalid bytes replaced, and `path_raw` and `name_raw`
carry the exact bytes in base64 when they are not UTF-8: pass those back.
`--plain` writes each row's path as its bytes. Some fields have no such twin
and stay lossy, among them `scan.root`, `scan.excluded[].path`, `query.path`,
`via`, `mount_point`, the commands in `next`, and the paths `scans` lists.
