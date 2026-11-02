# Index format

Each saved scan is one SQLite file, `scans/<scan_id>.db` under the index
directory (`$XDG_CACHE_HOME/duscope`, else `~/Library/Caches/duscope` on macOS,
or `--index-dir`/`DUSCOPE_INDEX_DIR`), mode 0600. duscope writes the file once,
after the scan, and never changes it: a rescan writes a new file. This page
describes `index_version` 2.0, whose DDL is
[schemas/index/v2.sql](../schemas/index/v2.sql).

## Reading the file

Open it read-only. The file never changes, so `immutable=1` spares SQLite its
locks:

```sh
sqlite3 'file:/path/to/a3f9c2e81b07.db?mode=ro&immutable=1'
```

```sql
-- The ten largest directories under the root.
SELECT CAST(name AS TEXT), cum_allocated
FROM dirs WHERE parent_id = 0
ORDER BY cum_allocated DESC LIMIT 10;

-- The largest files anywhere under directory 1234: a subtree is an id range.
SELECT CAST(f.name AS TEXT), f.allocated
FROM dirs AS d JOIN files AS f ON f.id BETWEEN d.id AND d.last_id
WHERE d.id = 1234
ORDER BY f.allocated DESC LIMIT 10;

-- Bytes on disk, each hard link and clone counted once.
SELECT SUM(allocated * charged) FROM files;
```

DuckDB reads the file through its `sqlite` extension:

```sql
INSTALL sqlite;
LOAD sqlite;
ATTACH '/path/to/a3f9c2e81b07.db' AS scan (TYPE sqlite, READ_ONLY);
SELECT ext, n, allocated FROM scan.type_stats ORDER BY allocated DESC LIMIT 10;
```

Names are BLOBs: `CAST(name AS TEXT)` in SQLite, `decode(name)` in DuckDB
(which fails on bytes that are not UTF-8). `scan_meta.value` declares no type
and holds both integers and text; if DuckDB reports a type mismatch reading it,
`SET sqlite_all_varchar = true` reads every column as text.

`duscope query --sql` runs one read-only `SELECT` over these tables in a
sandbox and prints the rows in the envelope (`{columns, rows, row_count,
truncated}`; a BLOB as `{"$bytes": "<base64>"}`):

```sh
duscope query ~ --sql "SELECT path(id), human_size(allocated) FROM files
                       WHERE under(id, :root_id) ORDER BY allocated DESC LIMIT 10"
```

- **What a statement may do.** Read the tables above (not `sqlite_schema`,
  views or other databases) through SQLite's core, date and JSON functions
  (an aggregate may run over a window, as `sum(allocated) OVER (…)`); every
  write, `PRAGMA`, `ATTACH`, transaction, window-only function (`row_number`,
  `rank`, …) and other function is refused (`sql_denied`). A statement without a `LIMIT` of its own
  returns 25 rows at most; `--max-rows` (10,000 at most) caps one with a
  `LIMIT`. `--timeout` (5 s, at most 60) stops it (`sql_timeout`), and SQLite
  may use 256 MiB of memory for it.
- **duscope's functions.** `path(id)` is an entry's absolute path (a BLOB where
  it is not UTF-8), `human_size(bytes)` a size as tables show it
  (`412.3 GiB`), and `under(id, root_id)` 1 when `id` is `root_id` or below it.
- **Parameters.** `:root_id` is the entry of the path `query` was given (the
  scan's root by default) and `:root_last` the last id below it, so a subtree
  is `id BETWEEN :root_id AND :root_last`.

## Conventions

- **Ids.** Every entry has an id, numbered in pre-order from 0 (the root) in one
  sequence across `dirs` and `files`. A directory's own files come before its
  subdirectories, so its subtree is `id .. last_id` and its own files are
  `id + 1 .. files_end`. Siblings are ordered by name, bytewise
  (`sibling_order`).
- **Sizes** are bytes. `apparent` is the length of the data (`st_size`);
  `allocated` the space the filesystem allocated for it (`st_blocks × 512` on
  Linux, every fork's allocation on macOS, which is what `du` adds up).
- **Times** are signed nanoseconds since the Unix epoch, UTC.
- **Names and paths** are the bytes the filesystem returned, stored as BLOBs:
  Linux names need not be UTF-8.
- **Unsigned 64-bit values** (`dev`, `ino`, `clone_id`) are stored as the signed
  64-bit integer with the same bits, so a value above 2⁶³ − 1 reads as negative.
- **Booleans** are integers, 0 or 1.
- **Platform columns.** A column marked macOS or Linux is NULL, or a flag bit
  stays 0, on the other platform.

## Tables

### `scan_meta`

One row per key; every key in [`scan_meta` keys](#scan_meta-keys) is present.
WITHOUT ROWID.

| Column | Type | NULL | Meaning |
| --- | --- | --- | --- |
| `key` | TEXT | no | the key |
| `value` | none | yes | the key's value: an integer or text, by key |

### `scan_volumes`

One row for every volume the scan met, whether it entered it or not: a scan of
`/` on macOS meets the sealed System volume, the Data volume and any external
disks.

| Column | Type | NULL | Meaning |
| --- | --- | --- | --- |
| `dev` | INTEGER | no | device id, as in `dirs.dev` |
| `mnt_id` | INTEGER | no | mount id: the kernel's unique mount id on Linux 6.8 and later, its reusable one before; the device id on macOS |
| `mount_point` | BLOB | no | where the volume is mounted |
| `fs_type` | TEXT | no | filesystem type as the kernel names it, lower-cased (`apfs`, `ext4`, `nfs4`, …) |
| `read_only` | INTEGER | no | 1 when mounted read-only |
| `entered` | INTEGER | no | 1 when the scan walked into the volume |
| `reason` | TEXT | yes | why the scan did not enter it: `firmlinked`, `kernel`, `network`, `one_file_system` or `bind_cycle`; NULL when it did |
| `case_sensitive` | INTEGER | yes | 1 when names that differ only by case are different names; NULL where it cannot be told without entering the volume |
| `normalization` | TEXT | no | `insensitive` when the NFC and NFD forms of a name are the same name (APFS, HFS+), else `none` |
| `fsevents_uuid` | TEXT | yes | macOS: the volume's FSEvents UUID, for incremental rescans; NULL on Linux, on the sealed System volume and where macOS gives none |
| `fsevents_event_id` | INTEGER | yes | macOS: the volume's latest FSEvents event id when the scan started; NULL where `fsevents_uuid` is |
| `os_build` | TEXT | yes | macOS: the build of the system the event id was read under, since event ids do not carry over an upgrade; NULL where `fsevents_uuid` is |
| `total_bytes` | INTEGER | yes | the volume's size when the scan met it |
| `free_bytes` | INTEGER | yes | the bytes free on it for an unprivileged user |
| `options` | TEXT | yes | its mount options, comma-separated (`rw,noatime`), which tell whether access times are kept |

### `dirs`

One row per directory, the root included.

| Column | Type | NULL | Meaning |
| --- | --- | --- | --- |
| `id` | INTEGER | no | the directory's id; the root is 0 |
| `last_id` | INTEGER | no | the last id of its subtree; equal to `files_end` when it has no subdirectory |
| `files_end` | INTEGER | no | the id of its last own file; equal to `id` when it has none |
| `parent_id` | INTEGER | yes | the id of the directory it is in; NULL for the root |
| `depth` | INTEGER | no | levels below the root; the root is 0 |
| `name` | BLOB | no | its name; for the root, the root's path (`scan_meta.root`) |
| `apparent` | INTEGER | no | the directory's own apparent size, not its contents' |
| `allocated` | INTEGER | no | the directory's own allocated size, not its contents' |
| `cum_apparent` | INTEGER | no | apparent bytes of the subtree, itself included, a hard-linked file counted once (as GNU `du --apparent-size`) |
| `cum_allocated` | INTEGER | no | allocated bytes of the subtree, itself included, each hard link and clone group counted once, at its charged member |
| `cum_du_compatible` | INTEGER | no | allocated bytes of the subtree, hard links counted once and clones not deduplicated: what `du` prints on APFS and ext4 |
| `n_files` | INTEGER | no | entries in the subtree that are not directories |
| `n_dirs` | INTEGER | no | directories in the subtree, itself excluded |
| `mtime` | INTEGER | no | modification time |
| `ctime` | INTEGER | no | inode change time, which moves on rename, link, chmod and chown |
| `max_mtime` | INTEGER | no | the newest `mtime` in the subtree, itself included |
| `dev` | INTEGER | no | device id |
| `flags` | INTEGER | no | [flag bits](#flags) |
| `errors` | INTEGER | yes | the errno that kept the directory from being read (listed with no children); NULL when it was read whole |
| `atime` | INTEGER | yes | the directory's own access time; NULL unless the scan collected `full` attributes |
| `btime` | INTEGER | yes | its creation time; NULL unless collected and the filesystem has one |
| `mode` | INTEGER | yes | its mode bits; NULL unless collected |
| `uid` | INTEGER | yes | its owner's user id; NULL unless collected |
| `gid` | INTEGER | yes | its group id; NULL unless collected |

### `files`

One row per entry that is not a directory: regular files, symbolic links,
sockets, FIFOs and devices.

| Column | Type | NULL | Meaning |
| --- | --- | --- | --- |
| `id` | INTEGER | no | the entry's id |
| `parent_id` | INTEGER | no | the id of its directory |
| `name` | BLOB | no | its name |
| `apparent` | INTEGER | no | apparent size; a symbolic link's is the length of its target |
| `allocated` | INTEGER | no | allocated size, the inode's own even for a hard link counted elsewhere; 0 for devices and sockets |
| `charged` | INTEGER | no | 1 when this row's `allocated` counts in `cum_allocated`; 0 for every hard link and clone but the charged one |
| `mtime` | INTEGER | no | modification time |
| `ctime` | INTEGER | no | inode change time |
| `atime` | INTEGER | yes | access time; NULL unless the scan ran with `--attrs full`; unreliable under `noatime` and `relatime` |
| `btime` | INTEGER | yes | creation time; NULL unless `--attrs full`, and on a filesystem that keeps none (Linux reports it for ext4, XFS, btrfs, ZFS, F2FS, bcachefs and ntfs3) |
| `mode` | INTEGER | yes | `st_mode`, file type and permission bits; NULL unless `--attrs full` |
| `uid` | INTEGER | yes | owner's user id; NULL unless `--attrs full` |
| `gid` | INTEGER | yes | owner's group id; NULL unless `--attrs full` |
| `nlink` | INTEGER | no | hard links to the inode |
| `kind` | TEXT | no | `file`, `symlink`, `socket`, `fifo`, `char`, `block` or `unknown` |
| `flags` | INTEGER | no | [flag bits](#flags) |
| `ext` | TEXT | no | the extension by-type classifies by: the last suffix of the name, lower-cased, at most 16 bytes of letters, digits, `+`, `~` and `-`, with `tar.gz`, `tar.bz2`, `tar.xz`, `tar.zst` and `tar.lz4` kept whole; empty for a dotfile, an undotted name and a trailing dot |

### `hardlinks`

One row per inode with more than one link (not a directory) that the scan met.
WITHOUT ROWID.

| Column | Type | NULL | Meaning |
| --- | --- | --- | --- |
| `dev` | INTEGER | no | device id |
| `ino` | INTEGER | no | inode number |
| `first_id` | INTEGER | no | the lowest id among its links: the one charged with its bytes |
| `nlink` | INTEGER | no | links to the inode |
| `seen` | INTEGER | no | links the scan met; fewer than `nlink` when some lie outside the scan |

### `clones`

One row per clone group: files that share their blocks through APFS clones.
macOS scans with `--clones` (the default) only. WITHOUT ROWID.

| Column | Type | NULL | Meaning |
| --- | --- | --- | --- |
| `dev` | INTEGER | no | device id |
| `clone_id` | INTEGER | no | the clone id APFS reports for the group |
| `first_id` | INTEGER | no | the lowest id in the group: the member charged with its bytes |

### `shared_members`

One row per file that is one of several links of an inode, or a member of a
clone group, naming its groups: how `explain` tells the links and members
inside a subtree from those outside it.

| Column | Type | NULL | Meaning |
| --- | --- | --- | --- |
| `id` | INTEGER | no | the file's id |
| `link_first` | INTEGER | yes | `hardlinks.first_id` of its inode; NULL when it has one link |
| `clone_first` | INTEGER | yes | `clones.first_id` of its clone group; NULL when it is not a clone |

### `scan_errors`

One row per entry a directory listed but the scan could not read; a directory
that could not be read at all has its errno in `dirs.errors` instead. WITHOUT
ROWID.

| Column | Type | NULL | Meaning |
| --- | --- | --- | --- |
| `dir_id` | INTEGER | no | the id of the directory that listed it |
| `name` | BLOB | no | the entry's name |
| `errno` | INTEGER | no | the error |

### `type_stats`

The whole scan's files by extension, counted as the totals are: a hard-linked
file once, at its charged link; a clone's allocated bytes at its charged
member and its apparent bytes at every member.

| Column | Type | NULL | Meaning |
| --- | --- | --- | --- |
| `ext` | TEXT | no | the extension, as in `files.ext`; empty for files without one |
| `n` | INTEGER | no | files |
| `allocated` | INTEGER | no | their allocated bytes |
| `apparent` | INTEGER | no | their apparent bytes |

## Indexes

Created after the rows are inserted:

| Index | Columns | For |
| --- | --- | --- |
| `dirs_parent_name` | `dirs (parent_id, name)` | resolving a path downwards, one component at a time, and listing a directory's subdirectories |
| `dirs_cum_allocated` | `dirs (cum_allocated DESC)` | the largest directories |
| `files_allocated` | `files (allocated DESC)` | the largest files |
| `files_mtime` | `files (mtime)` | files by age |
| `dirs_errors` | `dirs (id) WHERE errors IS NOT NULL` | the unreadable directories of a subtree, counted without reading every row |
| `dirs_excluded` | `dirs (id) WHERE flags & 3072 != 0` | the directories the excludes kept the scan out of (flags bits 10 and 11) |
| `files_dataless` | `files (id) WHERE flags & 4 != 0` | the dataless files (flags bit 2), which `summary` adds up without reading every file |

There is no name or extension index, and no path column: a path is resolved
through `parent_id`, and a directory's own files are an id range. For the
largest files of a small subtree the id range is faster than either index;
duscope chooses between them itself (with `INDEXED BY`), since SQLite's planner
prefers the range even where the index is thousands of times faster.

## Flags

`dirs.flags` and `files.flags` hold these bits. A bit a platform cannot set
stays 0; bits 9 to 11 are set on directories only, and bit 12 on files only.

| Bit | Value | Name | Set on | Meaning |
| --- | --- | --- | --- | --- |
| 0 | 1 | sparse | macOS, Linux | the file has holes |
| 1 | 2 | compressed | macOS, Linux | the filesystem compresses the file |
| 2 | 4 | dataless | macOS | the content lives in the cloud and is not on disk |
| 3 | 8 | purgeable | macOS | the system may delete the file to free space |
| 4 | 16 | may_share | macOS | the file may share blocks with another, though no clone id says with which |
| 5 | 32 | shares_all | macOS | every block of the file is shared with a clone |
| 6 | 64 | mount_root | Linux | the directory is the root of a mount |
| 7 | 128 | subvolume | Linux | the directory is a btrfs subvolume or a ZFS dataset |
| 8 | 256 | firmlink | macOS | the directory is a firmlink into the Data volume |
| 9 | 512 | upper_bound | macOS | the directory's totals are an upper bound: a file under it may share blocks with another that no clone id names |
| 10 | 1024 | excluded | macOS, Linux | the scan listed the directory but did not enter it, because an exclude pattern matched it |
| 11 | 2048 | excluded_cache | macOS, Linux | the scan listed the directory but did not enter it, because a cache rule matched it (`--exclude-caches`) |
| 12 | 4096 | later_link | macOS, Linux | a later link of a hard-linked file: its bytes, apparent ones too, count at its first link |

## Accounting

- **Hard links.** A file with several links is charged to the link with the
  lowest id, so two scans of an unchanged tree charge the same directory. Every
  link's row keeps the inode's own `allocated`; `charged` says which one counts
  in the totals. `SUM(allocated)` over a subtree is the per-path view,
  `SUM(allocated * charged)` the deduplicated one.
- **Clones.** A clone group is charged to its member with the lowest id; the
  others have `charged = 0`. A file flagged `may_share` without a clone id
  makes its directories' totals an upper bound.
- **`du` compatibility.** `cum_du_compatible` counts hard links once and
  clones at every member, which is what `du` reports on both APFS and ext4.
  `cum_apparent` counts hard links once, as GNU `du --apparent-size` does.

## `scan_meta` keys

| Key | Value | Meaning |
| --- | --- | --- |
| `index_version` | TEXT | this file's version, `major.minor`: `2.0` |
| `scan_id` | TEXT | the scan's id: the first 12 lowercase hex digits of SHA-256(root ‖ `started_at` ‖ 8 random bytes), and the file's name |
| `root` | BLOB | the scanned root, symbolic links resolved |
| `root_as_given` | BLOB | the root as it was typed |
| `started_at` | INTEGER | when the scan started |
| `finished_at` | INTEGER | when it finished |
| `attrs` | TEXT | `basic`, or `full` when the scan collected `atime`, `btime`, `mode`, `uid` and `gid` |
| `options` | TEXT | the scan's options as JSON: `clones`, `one_file_system`, `include_network`, `follow_symlinks`, `excludes`, `exclude_files`, `ignore_files`, `exclude_caches`, `threads`, `size_kind`; a rescan reuses them |
| `platform` | TEXT | the operating system and its kernel or Darwin version |
| `scanning_uid` | INTEGER | the user id the scan ran as |
| `thread_count` | INTEGER | the worker threads the walk ran |
| `size_kind` | TEXT | the size output shows by default, `allocated` or `apparent`; both are stored |
| `volume_total` | INTEGER | the size of the root's volume |
| `volume_free` | INTEGER | the bytes free on it for an unprivileged user |
| `unaccounted_bytes` | INTEGER | for a root that is a mount point, the volume's used bytes the scan did not find; NULL for any other root |
| `error_count` | INTEGER | directories and entries the scan could not read |
| `vanished_count` | INTEGER | entries that disappeared during the scan |
| `excluded_count` | INTEGER | files left out and directories not entered because an exclude or cache rule matched them |
| `clones_unavailable` | INTEGER | 1 when clones were to be counted on a filesystem that reports no clone ids (every one but APFS) |
| `n_entries` | INTEGER | rows in `dirs` and `files` together |
| `incremental` | INTEGER | 1 when an incremental rescan wrote the file |
| `parent_scan_id` | TEXT | the scan a rescan started from; NULL for a fresh scan |
| `source` | TEXT | `scan`, or `import:<format>` for an imported tree |
| `tool_version` | TEXT | the duscope version that wrote the file |
| `sibling_order` | TEXT | how siblings are ordered: `bytewise` |

## Compatibility

A reader opens every minor version of its major and ignores columns it does not
know; a minor version only adds nullable columns, tables or keys. A change to
what an existing column means is a new major version, which an older reader
refuses (exit 5) with a hint to rescan.

## Changelog

### 2.0

The first version, written from the first release that saves indexes.
