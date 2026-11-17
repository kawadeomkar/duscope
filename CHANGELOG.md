# Changelog

All notable changes to duscope are recorded in this file. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Changes are not written here directly. Each pull request with a user-visible
change adds a fragment, `changelog.d/<ID>.md`, in the format given in
[changelog.d/README.md](changelog.d/README.md). At each release
`scripts/changelog.py` assembles the fragments into the version's dated section
and deletes them.

## [0.3.0] - 2026-11-20

### Added

- The manual page has two new sections. MCP SERVER says what `duscope mcp` serves and how, the
  `--roots` rule, each tool, the server's limits, and the commands that register it with Claude
  Code, Codex CLI and Gemini CLI. AGENTS says which format a verb writes for an agent's client,
  what `duscope agent` prints, and where `duscope agent --skill` writes the skill, which the
  README now recommends.
- `duscope mcp` offers its first six tools. `duscope_scan` scans a directory under the roots and saves the scan, or with `max_age` returns a recent one; `duscope_scans` lists the saved scans of the roots; `duscope_summary`, `duscope_top`, `duscope_by_type` and `duscope_stale` read a saved scan, never scanning. Each tool's result is its verb's `--json` envelope, and its input schema is in `schemas/mcp/`, with `schemas/mcp/MAPPING.md` naming the verb flag each parameter sets and why the others are left out. `duscope_top` takes `older_than`, as `top` takes `--older-than`, and reads the modification time alone (for a directory, the newest under it). `duscope_stale`, like `stale`, lists a file only when its change time (ctime), as well as its modification time, is older than `older_than`; its description says so and names `duscope_top` with `files` and `older_than` for the modification time alone.
- `scan --reuse AGE` prints the newest saved scan of the path taken with the same options, when it finished within AGE, instead of scanning; nothing new is saved. It is refused with the treemap and `-i`.
- `scans PATH...` lists only the saved scans of those directories and of directories under them.
- `duscope mcp` runs tool calls one at a time, in the order they arrive, while it
  keeps answering `server/discover`, `tools/list` and `ping` at once. A client
  can cancel a call with `notifications/cancelled`: a waiting call never runs, a
  running `duscope_scan` stops its walk, and the call gets no response.
- `duscope_scan` sends `notifications/progress` every two seconds with the
  entries counted so far, when the request names a `progressToken`.
- Tool calls have deadlines: 10 minutes for `duscope_scan` and 30 seconds for
  the other tools (`duscope_query`'s statement keeps the sandbox's 5 seconds and
  its `sql_timeout`). A call past its deadline returns an error result with the
  `deadline_exceeded` code, and a scan past it stops.
- `duscope mcp` offers five more tools. `duscope_caches` lists the regenerable caches under a path; `duscope_explain` gives the facts about one file or directory; `duscope_query` runs one read-only SQL statement in the same sandbox as `duscope query`; `duscope_schema` lists the index's tables, or a verb's envelope as JSON Schema; `duscope_diff` compares two saved scans under a path inside the roots. `duscope_query` and `duscope_schema` read a whole scan, so they read only a scan of a root or of a directory under one: with no `scan_id`, the newest such scan that holds the path.
- `duscope_top`, `duscope_by_type`, `duscope_stale` and `duscope_caches` take `strict`, the tools' `--strict`: where the scan could not read some directories, the call fails with `permission_denied` instead of answering with a `partial_read` warning, whose hint offers `--strict`.
- `docs/mcp.md` documents `duscope mcp`: the `--roots` rule, the one-line registration for Claude Code, Codex CLI and Gemini CLI, each tool, and a Claude Code hook that asks before `duscope clean` runs. `server.json` describes the server to the MCP Registry.
- The manual page describes the interactive viewer and `clean`. INTERACTIVE VIEWER says
  how `duscope view` and `duscope scan -i` find their scan and when they refuse to start,
  what the screen, its panes and the four views show, what every key does, and how the
  mark pane moves marked entries to the Trash. CLEANING UP says what `clean`'s plan holds
  and when it acts, what each refusal reason means, where the Trash is on macOS and on
  Linux and how `--permanent` differs, and how `--from` handles are checked. The README
  lists the viewer's keys.
- The viewer (`duscope view`, `duscope scan -i`) has a cursor. The arrow keys and `h` `j` `k` `l` move it to the neighbouring tile among its siblings, `⏎` opens the folder under it (its largest child, or the way back down after `⌫`), `⌫` and `Esc` go up to the parent, `g`/`Home` and `G`/`End` jump to the largest and the smallest sibling and `PgUp`/`PgDn` move nine siblings at a time, reaching entries too small to draw. `z` zooms the map to the folder under the cursor, `Z` zooms out one step and `0` all the way. The cursor is an accent outline with the entry's name and size, and the folders around it are drawn in the link colour; the key line lists the navigation keys. With `--treemap pixels` (the `pixels` feature) the picture follows the zoom in the whole map's colours, the arrow keys move among its tiles, and the cursor's outline and its folders' borders are drawn into the picture.
- `i` in the viewer (`duscope view`, `duscope scan -i`) grows the selected pane into the full fact list: the other size measure; a folder's newest modification, its child changed least recently and its largest child with its share; hard links (a file's link count and the path of the link its bytes are charged to, a folder's links outside it); clones; what deleting it frees now and how exact that is; the dataless files under a folder; a cache folder's regenerating command and its `CACHEDIR.TAG`; what git says of it; the directories under it the scan could not read; its storage flags; its access and creation times (in a basic scan, `attrs basic` and the `--attrs full` hint); and the scan's volume, the bytes the volume uses that the scan did not find, when the scan ran and for how long, the size measure and the threads. The facts are `explain`'s, of the saved scan the viewer shows or of the one `view` saved; `scan -i` without `--save` answers from a fresh `stat`, as `explain` does for a path no saved scan holds. Below 110 columns the facts open over the map. `i` again or `Esc` closes them.
- The selected pane's mode row names the owner and group as numbers (`-rw-r--r-- · 501:20`) in a scan with `--attrs full`, and reads `—` in a basic one.
- The siblings pane follows the selection past its first nine rows (`G`, `End`, `PgDn`), to entries too small for a tile of their own: it shows the nine the selection falls among, with the larger and the smaller siblings each pooled into a `… n more` row.
- The viewer's four views: `1` the treemap, `2` the current folder as a table (the `s` sort: size, name, modified, count), `3` the largest files under the map's root, `4` the classes as `by-type` reads them, each a table in the design system's colours.
- `s` sorts the siblings pane and the list; `t` labels the map's tiles of at least 8×2 cells; `c` cycles the colour mode (type, folder, level, age, `LS_COLORS`) and `p` the theme pack, redrawing the map and its legend.
- `/` masks the map by a glob, as `--glob` matches it: the tiles that do not match are not painted, the layout stays, and `Esc` clears it; `⏎` in the legend or the types table filters the map to that row. The tables keep to the mask too; under a mask the largest files are ranked from the first 262,144 entries below the map's root, and where it holds more the title says so.
- `⇥`/`Shift-⇥` move the focus between the map (or table), the siblings pane and the legend, whose arrows then act there; `?` lists every key the viewer answers.
- `y` copies the selection's path with OSC 52 where the terminal allows it, and prints it on the key line either way; `o` shows the selection in the file manager without opening it, so an application or script in a browsed folder does not run: Finder's reveal on macOS; on Linux the file manager selects it (`org.freedesktop.FileManager1`, through `dbus-send`) or, where that is not available, `xdg-open` opens the folder holding it. A path that is not absolute, which only a saved scan's own root can give, is refused, and `o` does nothing while the last reveal is still running.
- `d` marks the selection, and `d` again unmarks it; `D` opens the mark pane, which shows the plan as `clean` prints it and moves the marks to the Trash when `yes` is typed in full. A plan with a refused item moves nothing, as `clean` without `--skip-refused`, and a mark whose folder leads elsewhere since the scan (replaced by a link) is not planned, so the pane never moves another entry in its place. What moved stays on the map as a freed tile until the next rescan, and the viewer says what moved when it closes.
- `r` rescans what the viewer is rooted at, with the saved scan's options, beside the viewer, and reloads when the new index lands, keeping the view, the mask, the marks and the cursor's path. A viewer rooted below the saved scan's root (`view ~/proj` over a scan of `~`) scans that folder alone and saves it as a scan of its own; one at the saved scan's root rescans it as `rescan` does. So does a viewer below it when a saved exclude is anchored at the root or names more than one path component, which would leave out something else in a scan of the folder. So does one, on macOS, where the walk would reach the Data volume another way from the folder than from the saved root (`/System` over a scan of `/`), and one that holds a bind mount the saved scan did not enter as a cycle, which a scan of the folder alone would enter (on Linux, `/mnt` over a scan of `/` with `/mnt/x` bound to `/`).
- The mouse: a click selects the deepest tile under the cell, a row in the siblings pane or a table, and filters by a legend row; a double click zooms, the wheel walks the path. There is no drag.
- `--treemap pixels` draws the treemap in pixels, shaded as `render` shades it,
  on a terminal that shows images through Kitty graphics, Sixel or iTerm2's
  inline images, in the static map and in `scan -i`'s viewer. duscope asks the
  terminal which of them it speaks only when the flag is given, and waits at
  most two seconds for an answer; where none comes, or stdin and stdout are
  not a terminal, the map is drawn in cells, with a warning that says why.
  Inside tmux it does not ask, since asking would turn on tmux's
  `allow-passthrough` option for the pane, and the map is drawn in cells,
  with a warning. It needs the `pixels` Cargo feature, which the Homebrew
  formula, the `.deb` packages and the release archives do not turn on:
  `cargo install duscope --features pixels` builds it, on Rust 1.90 or later.
  A build without it answers `--treemap pixels` with exit 2 and
  `not_compiled`, as before.
- `path_raw`: every row and object with a `path` (`top`, `tree`, `stale`, `by-type`'s
  largest files, `caches`, `diff`, `explain`, `explain --errors`, `summary` and its
  subvolumes, `clean`'s plan items) carries the path's bytes in standard, padded base64
  (RFC 4648) when they are not valid UTF-8, as `name_raw` does for a name. A path under a
  directory whose name is not UTF-8 reached JSON as lossy text that names no file; decoded,
  `path_raw` is the path to pass back. `caches` and `caches --by-project` carry
  `project_root_raw` the same way. Both fields are absent for a path that is UTF-8, and come
  last in `--fields` and `--detail`.
- The `excluded_truncated` warning: a list's `scan.excluded` names at most 20 directories,
  and the warning's `count` says how many more the excludes kept the scan out of under the
  path; its hint names the `duscope summary` that lists every one.
- `rescan` reads again only what changed, on macOS. It replays each volume's FSEvents history from the place the saved scan recorded, reads from disk the directories the history reports, their subdirectories and the way down to them, and takes every other directory's contents from the saved scan once that directory's own times still match it; hard-linked and cloned files it takes are read again, and the new scan is still a complete new index, with hard links and clones charged again over the whole tree. It reads the whole tree instead when the saved scan is older than `--max-age` (7 days, or the configuration's `max_age`), when a volume's history is another or none, no longer reaches back to the scan, is turned off, or belongs to a network, FAT or exFAT volume, when the files its options name changed or another duscope version took it, when FSEvents dropped events, when more than 15% of its directories changed, or when the post-check, which reads 1% of the directories it took from the saved scan again (at least 1,000), finds one changed. `--incremental` fails with `not_available` rather than read the whole tree, and `--full` always reads it. Linux keeps no such history: every rescan there reads the whole tree, `--incremental` is `not_available`, and `diff` compares the new scan with its parent.
- Every envelope's `scan` block gains `parent_scan_id` (the saved scan a rescan renewed), `rescanned_dirs` (the directories an incremental rescan read) and `fallback_reason` (`index_too_old`, `volume_changed`, `history_gone`, `fsevents_disabled`, `options_changed`, `too_many_changes`, `events_dropped` or `post_check_failed`), null where they do not apply; `incremental` is true for a scan an incremental rescan wrote. The table's footer says `rescanned 1,204 of 143,882 directories in 0.9 s (FSEvents)`, or `full rescan: options changed`.
- The manual's CAVEATS: on the startup disk's Data volume the history does not record changes made from recoveryOS or another system between a shutdown and the next startup, and a rescan keeps the access times (`--attrs full`) of what it takes from the saved scan. So that those are never older than `--max-age`, a scan with `--attrs full` that was itself an incremental rescan is read whole (`index_too_old`).
- `clean --permanent` deletes the items instead of moving them to the Trash, with the same plan, refusals, `--yes` and prompt. Each item is reached from `/` one directory at a time, never through a symbolic link, and is deleted only if its device and inode are still the plan's (`path_changed` otherwise). A directory goes with everything under it, bottom up, without following a symbolic link or crossing into a mount point or another volume: such a mount point stays with the directories above it, and the item fails with `refused_path`. On Linux before 5.8, or without `statx`, a bind mount of the same volume is found through each directory's file handle or the mount table, and where neither answers the directory is refused (`refused_path`) before anything in it is deleted. The envelope reports `deleted_count` and `freed_bytes`, the bytes on disk the deletion freed as `du` counts them. `trash_unavailable`'s hint now names `--permanent`.
- `clean --expect-bytes N` touches nothing unless the plan frees within 1% of N bytes (its `total_frees_bytes`), with the Trash or `--permanent`; otherwise it fails with `expectation_mismatch` (exit 1) and both figures.
- The `changed_since_scan` warning: `clean` measured items from the tree because they changed since the saved scan; its `count` says how many, and its hint names the rescan that measures the tree again. The viewer's mark pane says the same under its plan.
- An incremental rescan also reads again, whatever FSEvents reports, the directory of each file a process of the same user holds open for writing below the root, of each entry of the saved scan whose modification or status-change time lies within four hours before it or after it, and of each file it recorded as sparse. FSEvents reports a change to a file's contents only once the file is closed, so a file still being written, such as a virtual machine's disk, a database, a log or a download, is in none of its reports. The `scan` block's new `volatile_count` counts those entries, on macOS; it is null for any other scan. The files held open are listed through libproc, with no privilege and without opening them; a file idle at the saved scan that another user's process has opened since and still holds is the one case left, described in `docs/limits.md` and the manual's CAVEATS.
- `README.md` has a section on macOS and Linux: what a user meets on each (Full Disk Access,
  APFS clones, the volume group, `~/Library/Containers`, FSEvents rescans, cloud placeholders and
  snapshots; reflinks, full rescans, automount points, subvolumes, names that are not UTF-8,
  `relatime`, the Trash), each with the document that covers it. Its install section says what
  each channel installs and what building from crates.io or the source needs.
- The manual page has a section on `query --sql`, SQL QUERIES: which saved scan it reads, what a
  statement may do and what is refused (`sql_denied`), duscope's functions and parameters, the
  row, time (`sql_timeout`) and memory limits, and the output. CAVEATS explains the Full Disk
  Access probe and `full_disk_access_missing` on macOS; ENVIRONMENT adds `HOME`, `XDG_DATA_HOME`,
  `TMUX`, `STY` and `TERM_PROGRAM`, with what duscope reads each for; FILES adds the `out/`
  folder of spilled envelopes and a user `caches.toml`.
- `docs/troubleshooting.md`: every warning and error code duscope gives, what it means, why it
  happens and what to do about it. `docs/limits.md`: what duscope cannot measure, or measures
  only within bounds, and why: APFS clones written to since they were cloned, reflinks on
  Linux, snapshots, the Full Disk Access probe, the FSEvents history behind a macOS rescan and
  each reason it reads the whole tree, a tree held in memory and its 4 GiB of names, automount
  points, subvolumes, overlayfs, names that are not UTF-8, `--format ndjson` and `--plain`
  output, the Trash on NFS, 9p and FUSE, and the viewer's terminal. The manual page's CAVEATS
  points at both.
- `duscope mcp --allow-delete` offers `duscope_delete`, the tool that moves entries to the Trash and never deletes them permanently. Each item names a handle from `duscope_explain` and the bytes it should free (`expect_bytes`, within 1%); the tool is a dry run unless `dry_run` is false, its result is `clean`'s plan, and a path outside `--roots`, a handle that does not verify or has expired, a refused item, a size off by more than 1%, or an item whose size no saved scan knows stops the call; a call cancelled, past its deadline, or still to move anything when the client closes the server's input moves nothing, and one that has begun is answered with what it moved; when some items move and one cannot, the result's message says how many of them moved. Without the flag the tool is not listed and a call to it is an unknown tool. The tool is marked for confirmation on every call (`_meta["anthropic/requiresUserInteraction"]`), which is the client's to honour.
- On macOS a saved scan records, for each volume it entered, where the volume's FSEvents history stood when the scan began: the UUID of its FSEvents database, the system's event id read before the scan read anything, and the system build (`fsevents_uuid`, `fsevents_event_id` and `os_build` in `scan_volumes`, which `duscope query --sql` reads). An incremental rescan will read the changes since from there. The three stay NULL on Linux.
- `explain --clones` sizes the extents the files under a path share with other files or
  snapshots on btrfs and XFS (reflink copies, deduplicated ranges), where the scan recorded no
  clone ids: each file is opened read-only, never read, for its extent map, and only once its
  path, followed through no symbolic link, is found to lead to a regular file on a local
  filesystem that keeps one. A file whose extents are shared is `is_clone`, the bytes it shares
  come off `freeable_now_bytes` as a lower bound, and the evidence says how many files share how
  many bytes.
- `summary` lists the btrfs subvolumes and ZFS datasets under the path in `subvolumes`, each
  with the bytes the scan found on it, less the subvolumes below it, and a ZFS dataset's
  referenced bytes.
- `--skip-containers` lists `~/Library/Containers` and `~/Library/Group Containers` without entering them, for every verb that scans. They appear in `scan.excluded` with `reason: skipped_containers` and count in `excluded_count`; a saved scan and its `rescan` keep them out. The flag is accepted on Linux too, where a home seldom has those folders.
- A scan whose root holds the home's `Library` (the home, `/`, or `~/Library` itself, also by the Data volume's own path such as `/System/Volumes/Data`) checks whether macOS refuses `~/Library/Mail` and `~/Library/Safari`, and then warns `full_disk_access_missing` with the app to grant Full Disk Access to: the terminal, or under `duscope mcp` the app running the server.
- Cloud placeholders (iCloud, Dropbox, OneDrive files whose content is not on the disk) under the path warn `dataless_present` with their count and logical size, for a fresh scan and a saved one.
- A row's `flags` can name `automount`: on Linux, a directory the scan listed and did not enter because it is an automount point the kernel had not mounted. A saved scan records it as bit 14 of `dirs.flags` (docs/index-format.md).

### Changed

- Building duscope from source needs Rust 1.89 or later; it needed 1.88.
- The treemap and the viewer measure names by Unicode 18's character widths, where they used
  Unicode 17's: the halfwidth katakana sound marks `ﾞ` and `ﾟ` (U+FF9E, U+FF9F) take a column
  each, as terminals draw them, where they took none, and the characters Unicode 18 adds take
  the width it gives them.
- Saved scans are written as `index_version` 2.1, which adds index flag bits 13 (`skipped_containers`) and 14 (`automount`) and the `dirs_automount` index. Every reader of 2.x, v0.2.0's included, opens the files of both minors and answers alike; a new flag bit now takes a new minor version, and a new or dropped secondary index none ([docs/index-format.md](docs/index-format.md#compatibility)).
- Saved scans are written without the `dirs_cum_allocated` and `files_mtime` indexes, which no query read: a save is faster and each index file smaller. Dropping them changes no index version: older files open and answer as before, and only a hand-written `query --sql` that sorts all directories by size or all files by age runs slower.
- `scan --save` holds much less memory: the index is built with SQLite's default page cache, and its sort spills to a temporary file (in `SQLITE_TMPDIR`, `TMPDIR` or the system's temporary directories, mode 0600, unlinked as it is opened, holding directory names and file sizes). If no temporary directory can hold it, the save is built again with the sort in memory.
- `by-type` with a large `--examples` keeps each class's largest files in time that grows with the logarithm of the number asked for, not the number itself, wherever it counts the files one by one: a scan run for the command, a filter, `--size apparent`, `--sort mtime`, or a folder below a saved scan's root. On a million files with `--examples 100000` that step went from up to 20 s to under 0.05 s. The examples listed, and their order, are the same.
- `top --files` and `stale` on a saved scan answer from the index with `--type`, `--min-size`,
  `--max-size`, `--size apparent`, `--depth`, `--min-depth`, `--max-depth`, the time filters and
  `stale --atime`, instead of reading every entry: on a scan of a million entries, tens of
  milliseconds instead of up to a second and more. The rows are the same. `--glob`, `--regex`,
  `--exclude`, `--sort` other than size, `-r` and `--all` still read every entry.
- `caches` on a saved scan reads the scan's directories and only the files it looks for (each `CACHEDIR.TAG` and the cache rules' marker files) instead of every entry, and reads nothing inside a cache once it is confirmed: about five times faster on a scan of a million entries (422 ms to 81 ms). Its rows, with `--loose` and `--by-project` too, are the same as before.
- `by-type` on a scan saved with `--attrs full` answers from the index, as on a basic one, rather than reading each entry: 63 ms instead of 500 ms over a whole saved scan of a million entries, and 49 ms instead of 150 ms for a directory of 262,000 files. A file without an extension is still `binaries` when an execute bit is set.
- `top` and `stale` with `--format ndjson`, or with `--plain`, write each row as it is built
  instead of holding them all: `top --files --all --format ndjson` on a million entries peaks at
  about 200 MB where it took 760 MB (and 3 GB with `--fields`). The output of a run that succeeds
  is the same, byte for byte. A run whose read of the saved scan fails part way (`index_corrupt`,
  exit 5) now writes the rows read before the failure, where it wrote none, then under
  `--format ndjson` the error envelope in place of the `end` line, and under `--plain` nothing
  more on stdout (the error goes to stderr).
- `tree`, `by-type`, `caches` and `diff` with `--format ndjson`, or with `--plain`, write each row
  as it is built instead of holding them all, as `top` and `stale` do: on a million entries,
  `tree --all --depth 1000 --format ndjson` peaks at about 64 MB where it took 720 MB, and
  `diff --all --format ndjson` of two scans that differ in every file at about 220 MB where it took
  770 MB. The output of a run that succeeds is the same, byte for byte. A run whose read of a saved
  scan fails part way (`index_corrupt`, exit 5) now writes the rows read before the failure, where
  it wrote none, then under `--format ndjson` the error envelope in place of the `end` line, and
  under `--plain` nothing more on stdout (the error goes to stderr).
- `stale`'s help, the manual page and the briefing `duscope agent` prints (and the skill
  `agent --skill` writes) say that `stale` lists a file only when its change time (ctime) is
  older than `--older-than` as well as its modification time, so a file restored, or copied
  with its times kept, is not listed; for the modification time alone they name
  `duscope top --files --older-than AGE`. The briefing answers "large files not modified in
  months" with `top --files --older-than`, and keeps `stale` for files neither modified nor
  changed.
- `duscope mcp` answers a `ping` that names protocol version 2026-07-28 in its `_meta` with `-32601` (method not found), since that revision has no `ping`. A `ping` in a 2025-11-25 session, or ahead of its `initialize`, is answered as before.
- The viewer opens on a large saved scan in less than half the time: it reads the scan once to
  classify its entries, which also gives each folder's colour on the map and the legend's rows,
  where it read it three times. On a million entries (T2) the first frame comes after about 0.5 s
  instead of 1.2 s.
- `--plain --detail` writes one more column at the end of every row: `path_raw` for `top`,
  `tree`, `stale`, `diff` and `explain --errors`; two for `caches`, `path_raw` then
  `project_root_raw`; `project_root_raw` for `caches --by-project`. Each is empty for a path
  that is UTF-8. The columns before them keep their places, so a script that reads them by
  position reads the same values, and one that counts the columns finds one more (two for
  `caches`).
- `scan.excluded` names only the directories the excludes kept the scan out of within the
  path a result was read at, and a list (`scan`, `top`, `tree`, `stale`, `by-type`, `caches`,
  `diff`, `query`, `explain --errors`, `clean`'s plan) names the first 20 of them in the
  scan's order. Every envelope carried the whole scan's list: 300 `CACHEDIR.TAG` folders made
  `top` of a folder holding none spill to a file. `summary` and `explain` still name every
  one under their path; `summary` of the scan's root is unchanged. `scan.excluded_count`
  stays the whole scan's.
- `scan.volumes` names the volume the path lies on and those mounted under it, not every
  volume the scan met; at the scan's root it is unchanged.
- `scans --check` also tells whether the scanned tree changed since the scan. After checking the file for damage, it reads every directory the scan holds again (one `statx` on Linux, one `getattrlist` on macOS each) and compares the later of its modification and status-change times with the scan's: an entry added, removed or renamed, a mode or owner changed, or a directory gone makes the scan stale. So does a directory touched while the scan ran, within two seconds of its start or finish, since a second change in the same clock tick would leave its times as the scan read them. A scan that followed symbolic links has its directories read through them. A stale scan exits 1 with `index_stale` and a hint to rescan that names the scan as it was found (`--index-dir`, `--trust`), its row printed with `structurally_stale: true` (`--plain`'s fourth field); a fresh one exits 0, and a damaged file still exits 5. The index directory, which every save writes to, is left out below the scan's root, so a scan of a tree that holds it can still check fresh once the directory exists before the scan. A file rewritten in place changes no directory, and is not seen.
- `clean --from` fails with `path_changed` when the entry a handle names has changed since the saved scan, as it does when another entry has taken its place: the handle signs what the scan saw. `duscope_delete`, which runs `clean`, refuses such an item the same way, and still refuses a directory whose bytes no saved scan measured (made since it, not entered by it, or holding what it did not enter), whatever its `expect_bytes`.
- When `clean` moves or deletes some items and cannot move or delete another, its error message says how many of the items went before naming the first failure (`2 of the 3 items were moved; one was not: …`). The JSON envelope's `error.message` after acting is now the run's message, as stderr shows it, where it was the first failed item's own; the item keeps its own error in the plan.
- An index directory whose `scans` folder is a symbolic link, a link to nothing included, is now refused (`untrusted_index`, exit 1) by every command that lists, reads or saves a scan there (`scan --save`, `scans`, the newest saved scan a verb looks for, `--scan` with an id, and the MCP server's tools), where v0.2.0 read and saved scans through the link: the saved scans it leads to are outside the index directory, which an index file must not be unless `--trust` names it. Move the folder the link leads to into its place, or point `--index-dir` (or `DUSCOPE_INDEX_DIR`) at a directory that holds it as its `scans`, or link the whole index directory instead, which stays allowed.
- `explain` no longer follows a `.gitignore` that is a symbolic link: it reads as absent, as git has read one since 2.32.
- A pattern `--regex` cannot compile is reported in one sentence, the pattern and what is wrong with it, where its error drew the pattern over several lines.
- `duscope mcp` reads messages of at most 1 MiB, down from 16 MiB: a longer line is refused (`-32600`) and skipped. Every tool's largest request fits, and parsing one message now costs at most about 100 MB, where it could cost 1.3 GiB.
- On Linux, a btrfs subvolume, a ZFS dataset and a btrfs subvolume mounted on its own are
  flagged `subvolume` in the listings: on kernels before 6.10 too, where btrfs tells its
  subvolumes apart only by their device.
- A ZFS dataset mounted with `casesensitivity=insensitive`, and an HFS volume, are
  case-insensitive, so paths on them are looked up as such.
- `explain` leaves dataless placeholders out of `freeable_now_bytes`: a placeholder reports 0, and a directory the bytes on disk of its other files, with an evidence line naming the placeholders.

### Fixed

- When a read of a saved scan fails, `by-type` and `top --files` name the index file once in their `index_corrupt` message, where they named it twice.
- `top --files`, `stale` and `by-type` answer from an index file without the size index
  `files_allocated`, only slower, where they failed `index_corrupt`.
- An envelope longer than `--max-chars`, when it was the first thing written to
  a new index directory, left that directory with the default permissions
  (usually 0755). duscope now makes the index directory, and each directory on
  the way to its `out/`, 0700, as `SECURITY.md` says.
- `duscope mcp`'s `instructions` end with a whole line of the agent briefing, the last that fits in 4,096 characters, where they stopped at the 4,096th character, inside a row of the briefing's table.
- `duscope mcp` reads the configuration file as every other verb does: its `index_dir` is used when neither `--index-dir` nor `DUSCOPE_INDEX_DIR` names a directory, so the server and the command line read and save the same scans, and a configuration file it cannot use stops the server at start (exit 2).
- After `clean` moves or deletes something, its `index_untouched` hint and its `next` command name a rescan that runs: `next` holds `duscope rescan <root> --scan <id> --json`, with `--index-dir` or `--trust` where the scan was found through them, as `scans --check` suggests it. They suggested `duscope rescan <id>`, which `rescan` read as a path and answered `scan_not_found`. For an import, which `rescan` refuses, the hint names a new `scan --save` of its root and `next` is empty; a clean that moved and deleted nothing suggests no rescan.
- `README.md` said that no release had a command that deletes. `clean` came with v0.2.0;
  the README now says that it is a dry run unless given `--yes`, moves to the Trash unless
  given `--permanent`, and refuses a symbolic link, a mount point and the scanned tree's
  root.
- `README.md` said that the interactive viewer and the MCP server arrive in later releases.
  v0.2.0 carries the viewer, `clean`, `duscope agent` and the MCP server, whose tools come
  with v0.3; the README now says so.
- A key pressed as the terminal is resized reaches the viewer at once. It could wait unread until the next key, on macOS nearly every time a resize and a key came together.
- The siblings pane no longer reads every entry of the selection's folder on each frame to add up its `… n more` row: the folder's list is read once, in the order the index keeps it, with the sizes totalled as they run, so in a folder of a million files a move draws in under a millisecond, not a third of a second.
- A saved scan of `/` now tells which mounts it did not enter: `view` and the list verbs measure such a mount instead of showing it empty, and `explain` says the scan did not enter it and rates its figures `low`. The paths in `scan.excluded` and in `--debug`'s messages no longer start with `//` for a scan of `/`.
- `view` and `scan -i` give the terminal back when `SIGTERM` (a plain `kill`) or `SIGHUP` (the
  terminal closed) ends them, and still end by that signal: the shell is no longer left in raw
  mode on the alternate screen, with the mouse reported.
- Resizing the viewer's window draws one frame, at the size the window comes to, not one for
  every size it passed through.
- A click or an arrow key onto a `+N small` tile reads the folder's entries once, not on every
  click: in a folder of 100,000 files ten clicks read each entry once, where they read it ten
  times. Under a filter it lands on the largest entry the filter keeps.
- `query --sql` and `export` whose scan cannot be saved print the error envelope (`index_write_failed`, exit 1), as for any other failure: `query` under `--json` and `--format ndjson`, `export` under `--format ndjson`. They save the scan they run before printing anything, and the failure went to stderr alone, leaving stdout empty. The hint now names `--no-save` and `--index-dir`, where it named `duscope scan --save`.
- `import --json` with no index directory (neither `HOME` nor `XDG_CACHE_HOME` set) prints the error envelope, where stdout was empty.
- The `partial_read` warning's hint offers `--strict` only after a list verb (`top`, `tree`, `by-type`, `stale`, `caches`), the verbs that take it. After `scan`, `rescan`, `import`, `summary`, `query`, `export`, `render` and `view` it says to run duscope again as a user who can read the directories, where it named a flag those verbs refuse as a usage error.
- The `scan_age` warning of `stale` names the command that measures the tree again, then `stale` again: `duscope rescan` of the saved scan it read, or for an import `duscope scan --save` of its root. It said to run `stale` again, which reads the same saved scan.
- A failed save of `rescan` and of `import` (`index_write_failed`) names that verb in its hint, where it named `duscope scan --save`. `rescan`'s names the saved scan by its file (`--trust FILE --scan FILE`), which it still finds with `--index-dir` naming another directory.
- `export --format gpscan` records the volume the exported path lies on, found by its
  directory's device as `scan.volumes` finds it. From a macOS scan of `/`, an export of a
  folder under `/Users`, `/private` or `/opt`, which the Data volume holds through a
  firmlink, recorded the size and free space of the read-only System volume mounted at `/`.
- `summary` of a file names the volume the file lies on, with the file's bytes, in
  `result.volumes`. A file in a saved scan had none, and a file scanned for the command had
  its volume with no bytes on it.
- `clean` no longer moves a folder to the Trash with a volume mounted inside it, which stayed mounted in the Trash and lost its files when the Trash was emptied (since v0.2.0). A folder that holds a mount point at any depth, a bind mount of the same device included where the system tells one, or whose mounts the system cannot tell, is refused as `mount_point` in the plan, in a dry run and in the viewer's mark pane alike, and the mount table is read again just before each move: a volume mounted since the plan leaves the folder where it is, `refused_path`. `--permanent` already stopped at such a mount.
- `clean --from` with handles from two saved scans is a usage error, where every item was planned against the first handle's scan.
- `clean` no longer prices an item that changed since its saved scan at the scan's size. A file whose kind, modification or change time, size, bytes on disk or link count differs from the scan's, and a directory whose files, folders, bytes, bytes on disk, newest modification or change time or sum of change times differ (what lies under a folder the scan did not enter left out), is measured from the tree: a directory by a walk, whose bytes deleting it frees at most (`upper_bound`). `--expect-bytes` is held to those figures, so a directory that grew since its scan no longer passes the figure the scan gave it, and nothing is touched. A write whose modification time was put back, and a hard link whose other half has gone, count as changes.
- A directory made since the scan, or one the scan listed without entering, counts in the plan's `total_frees_bytes` by its walk, where it counted 0, so `--expect-bytes 0` no longer passes for it.
- A directory that holds one the scan listed without entering (`--exclude`, `--exclude-caches`) counts what a walk finds there, as an `upper_bound`, where the plan called the scan's smaller figure `exact` and `--expect-bytes` passed against it.
- A changed file with other links counts its bytes, at most and once however many of its links the plan holds, where it counted nothing and `--expect-bytes 0` passed.
- A directory that cannot be read whole has no `frees_bytes` (a figure from what could be read would look like a measurement): the plan carries a `partial_read` warning, and neither `--expect-bytes` nor `duscope_delete`'s `expect_bytes` is met by it.
- On Linux, `clean` moves an item to the Trash through descriptors, as `--permanent` deletes one (since v0.2.0 the check and the move were by path). The item's folder is reached from `/` one folder at a time, never through a symbolic link (a folder on the way needs only to be searchable, not readable, as for any path, and with `--permanent` too), and the item is moved out of that folder only if its device and inode are still the plan's (`path_changed` otherwise): a folder on the way swapped for a symbolic link can no longer send an entry outside the plan to the Trash. The move never replaces an entry already in the Trash, whose `files` and `info` folders are never reached through a symbolic link either, and an entry that took the item's name just before the move is moved back where it was. Where the filesystem cannot rename without replacing (NFS; NTFS through ntfs-3g and other FUSE filesystems without it), the Trash is checked for the name just before the move, as `mv -n` does.
- On Linux, when what `clean` moved to the Trash cannot be read there again to check it (a stale NFS handle, an I/O error), it is moved back as an entry that took the item's name is, and the error (`io_error`) says that it could not be checked after the move, not that the item changed or that another entry was moved in its place; if it cannot be moved back, the error names where it is in the Trash.
- On macOS, `clean` answers `trash_unavailable`, whose hint suggests `--permanent`, only when the item's volume has no Trash, which it now tells by the system's error code instead of the words of its message. Any other refusal to move an item to the Trash is `io_error`: a file that went missing just before the move, a Trash that could not be made, and any error duscope does not know, which were `trash_unavailable` unless their message spoke of permission. The message ends with the error's domain and code, for example `(NSCocoaErrorDomain 513)` for a locked file.
- Ctrl-C while a scan is being saved stops the save: duscope exits 130 with `duscope: interrupted`
  and leaves neither the index nor its temporary file; the output already printed stands. Ctrl-C
  was ignored there, and the save, seconds long on a large tree, ran to its end.
- Ctrl-C stops `duscope import` as it stops `scan --save`: while the file is read, or while the
  import is saved, duscope exits 130 with `duscope: interrupted` and leaves neither the index nor
  its temporary file; once the index is saved, the import finishes. Ctrl-C used to kill it, with
  no message, and a Ctrl-C during the save left a temporary file for `scans --prune`.
- On Linux, `clean` no longer refuses the files of an overlay as `mount_point` (since v0.2.0). Without `xino`, overlayfs reports a merged folder on the overlay's device and each file on the device of the layer that holds it, so every file of an overlay whose layers lie on other filesystems was taken for a mount point, for the Trash and `--permanent` alike, and by `duscope_delete`. A file is now a mount point when it is the root of a mount, as `statx` tells from Linux 5.8 (before it, its mount id against its folder's, or the mount table), and by its device only where none of them can tell. A folder on another device than its parent is refused as before. A file bound over another of the same device, and with `--permanent` a folder bound so, are now refused in the plan, where they failed only when acted on.
- `README.md` said that duscope reads no file's contents but a `CACHEDIR.TAG`'s signature, and
  that only for `--exclude-caches`. `caches` reads the signature too, and `explain` reads the
  files git reads (the ignore files, the configuration and the repository's index) to tell
  whether git tracks or ignores a path; the README now says so. It also said that `scan` always
  scans afresh, which `--reuse` does not.
- The manual page's EXIT STATUS gave exit 2 for "a flag of a later release", which no flag is
  any more, and read as if exit 5 meant the version that wrote an index could not read it. Exit 2
  is a usage error, a cursor or field a verb cannot take, two scans `diff` cannot compare, or what
  the build or platform cannot do; exit 5 is a damaged index or one of an index version this
  duscope cannot read.
- `docs/index-format.md` gave the index directory for macOS alone, the `sparse` flag for Linux,
  which never sets it, and `firmlinked` among a volume's reasons, which no volume row carries,
  without `excluded_cache`, which a mount point holding a `CACHEDIR.TAG` under `--exclude-caches`
  does.
- `README.md` said that duscope accounts for btrfs and XFS reflinks. A Linux scan counts each
  reflinked copy in full, as `du` does, and `duscope explain --clones` measures what they share;
  the README now says so.
- The viewer's `r` judges a file named by `--exclude-from`, `--ignore-from` or the configuration's `[caches] extra` table as `rescan` has since 0.2.1: it is read only when it is a regular file of at most 1 MiB, links followed.
- An envelope past `--max-chars` is no longer spilled through an index directory's `out` that is a symbolic link, nor is the mode of what the link leads to set to 0700: such an `out`, even a link to nothing, is refused (`io_error`) before anything is written or changed, and an `out` another user owns keeps its mode. The spill file is made through `out` as it was opened, one folder at a time from `/` without following a link, so an `out` replaced by a link while the envelope is spilled never gets the file either.
- `scans --prune` takes as a stray temporary file only a regular file named as a save names one, `<id>.db.tmp-<pid>` with a scan id and the writer's process id, where it took any name ending `.db.tmp-` and a number (a folder so named made it fail), and refuses an index directory whose `scans` is a symbolic link (`refused_path`), as it refuses a linked `out`, before it removes anything.
- `scans --prune` removes each file through the `scans` or `out` folder it opened, one directory at a time from `/` without following a link, and only while the name is still the file it listed, so a folder swapped for a symbolic link while it runs loses nothing. A file replaced since the listing stays, and the run fails `io_error`.
- `explain`, and so `clean`'s plan, the `duscope_explain` tool and the viewer's facts, no longer waits for ever on a FIFO, or fills memory from a device, put where git keeps a file: `.git`, `commondir`, a configuration file, a `.gitignore`, `info/exclude`, the excludes file or the index. Each is read only when it is a regular file and only up to a cap (64 KiB for `.git` and `commondir`, 1 MiB per configuration file, 1 MiB for all the ignore files one lookup reads together, 1 GiB for the index); otherwise the git lookup fails, and `explain` reports `git: null` with low `confidence` and the reason in `evidence`. A configuration file or `commondir` that cannot be read is passed over, as before.
- A saved scan whose rows place an entry where no scan puts one (two directories that name each other as parent, a directory that names itself, a subtree that ends before its directory, past the scan or past its parent's, a directory whose own files take in its subdirectories) no longer makes any verb read for ever, as `top --files` did, or walk the tree in time exponential in its rows, as `tree` and `diff` did; `render` of a folder no longer panics on one (an abort in a release build). A verb that reads such a row fails `index_corrupt` (exit 5), `rescan` included, and so does `view` once it quits; a verb that does not may answer from the rows it read, with no warning.
- A saved scan whose `n_entries` is past its rows is refused when it opens, and one whose ids leave one out fails `index_corrupt` (exit 5) at the first missing id a verb meets, where `top --files --sort mtime` and `render` read every missing id, about 160 hours for a count of 4294967295.
- `scans --check` reads every row of a saved scan and fails `index_corrupt` (exit 5) on one out of the layout, an id that no row holds, or an entry that does not lie where its parent's ids place it, before it sweeps the tree.
- `duscope mcp` no longer grows without limit when a client sends requests faster than they finish: at most 64 requests, holding 32 MiB between them once parsed (their ids included), wait behind the running call, and a request past either is answered at once with the JSON-RPC error `-31000` and never runs. Cancellations, `ping`, `tools/list` and `server/discover` are still handled at once.
- Messages and logs on stderr, clap's usage errors included, show control characters as escapes (`\x1b`), as tables do, so that a file name or argument cannot drive the terminal.
- A scan of a very deep tree no longer takes memory that grows with the square of its depth: each directory on the way down now holds its name rather than its path, so the memory grows in proportion to the depth. A chain of 3,000 directories peaks at about 12 MB instead of 34 MB (before, a chain of 20,000 took about 600 MB). The time of such a scan is unchanged: most of it is the system's, opening each deep directory.
- On Linux, a scan no longer mounts a filesystem by entering an automount point the kernel has not mounted yet (an NFS submount or referral, debugfs's `tracing`). Such a point is listed and left alone like a network mount: skipped as `one_file_system` under `-x`, otherwise as `network` unless `--include-network` is given. Before, one on the scanned root's own filesystem was entered whatever the flags, `-x` included.
- On Linux 4.11 to 5.12, an automount point the kernel has not mounted yet is now left alone too. Those kernels mark it without vouching for the mark in `statx`'s attribute mask (the 5.10 series vouches from 5.10.36), and a scan read it as a plain directory and entered it, mounting it, whatever `-x` and `--include-network` said.
- On Linux, an automount point a scan leaves alone now counts in the `mounts_excluded` warning, of a fresh scan and of a saved one read at any path above it, and the warning's message counts such points apart from the mounted filesystems `scan.volumes` names. A verb asked about the point itself scans it, as it scans a directory a saved scan excluded. Before, the point read as an empty directory, with no warning.
- On Linux, `clean` plans a directory that holds an automount point the scan left alone as it plans one that holds an excluded directory: what a walk finds under the point is added to the scan's figures as an upper bound, and is not read as a change since the scan.
- On Linux, an ncdu export (`export --format ncdu`) marks an automount point the scan left alone `"excluded":"otherfs"`, as ncdu marks a filesystem it did not enter; `import` reads it back as one.

## [0.2.1] - 2026-11-18

### Fixed

- `rescan`, and every command that reads `--exclude-from`, `--ignore-from` or the configuration's `[caches] extra` table, no longer waits for ever on a FIFO, or fills memory from a device, named as one of those files: each is read only when it is a regular file of at most 1 MiB, links followed, and is otherwise refused as a usage error that names the file and why (`not a regular file`, `is a directory`, or the cap).
- `scans --prune` removes from the index directory's `out` only the files a spilled envelope is written to (`<scan>-<verb>-<time>.json`), where it removed every file there older than a week, and refuses an `out` that is a symbolic link (`refused_path`) before it removes anything.

### Security

- rustix 1.1.6, for RUSTSEC-2026-0486: rustix 1.1.5's extended-attribute functions were unsound on a zero-length buffer. duscope calls none of them, so nothing changes in what it does; the binary no longer carries the release the advisory names.
- GHSA-64hc-5m63-wrmm (CVE pending): `export -o` and `agent --skill` no longer write through a symbolic link planted in their way, which could overwrite another of the user's files.

## [0.2.0] - 2026-11-05

### Added

- Every verb's `--json` envelope is described by a JSON Schema under `schemas/envelope/` (`<verb>.schema.json`, with `error.schema.json` for a failure), listing the warning codes, the error codes and the values of every enumerated field. `schemas/envelope/history/` keeps each published version.
- `next` lists up to three commands to run on the same scan, most useful first: the next page when rows remain, then the largest entry to `explain` and the related verbs.
- `export [path] --format F` writes a saved scan (or one run for it) for other tools: `ncdu` (JSON that `ncdu -f` opens), `gpscan` (GrandPerspective's gzip XML; with `-o FILE.gpscan` or into a pipe), `qdirstat` (QDirStat's cache file, with owners and modes: a scan run for it collects them), or `ndjson` and `csv`, a row an entry with its path, kind, sizes, link count, time and a directory's totals. `--size apparent` exports apparent sizes where the format has one measure.
- `import FILE` saves a tree another tool wrote as a scan: ncdu's JSON export, gdu's (whose directory totals are added up again from the entries), or GrandPerspective's `.gpscan`, gzipped or not, from a file or stdin (`-`). `--format ncdu|gdu|gpscan` names the format, or the file shows it. The saved import's `source` is `import:<format>`; `--scan` reads it, and `diff` sets it against a scan of this machine, but `latest` never names it, so no verb reads it in place of a scan. Gzip is read up to 1 GiB of decompressed data.
- `explain --json` returns a `handle`: the path as `explain` found it (device, inode, bytes on disk, the scan) with a fifteen-minute expiry, signed with HMAC-SHA256 by a key that duscope makes on first use in its cache directory (`handle.key`, 32 random bytes, mode 0600; deleting it invalidates every outstanding handle). `clean --from` acts only on what a handle names. A key that others may read issues no handle.
- A configuration file, `~/.config/duscope/config.toml` (or `--config FILE`, `DUSCOPE_CONFIG`; `--config none` reads none), with flag defaults under `[defaults]`, `[excludes] patterns` for every scan, and `[classes]`/`[caches]` extra tables; flags and their environment variables win over it.
- `-v` prints the walk's and the roll-up's timings to stderr, `-vv` each volume and the worker count too, and `--debug` each directory's errors as they happen; `DUSCOPE_LOG` (or `RUST_LOG`) filters the log in `RUST_LOG`'s grammar.
- `-q` keeps warnings off stderr.
- `scan --save` saves the scan as an index, one SQLite file under `$XDG_CACHE_HOME/duscope/scans/` (`~/Library/Caches/duscope` on macOS when unset), and prints its id (`scan.id` in the envelope); `--index-dir` or `DUSCOPE_INDEX_DIR` names another directory. The index is written after the output, all at once and renamed into place; if it cannot be written, duscope says so on stderr and exits 1, and the output stands.
- `-v` reports the index write.
- `top`, `tree`, `by-type` and `stale` read the newest saved scan that holds their path, taken with the same options, instead of scanning again; with none, they scan, save the scan and show its id; if it cannot be saved, the output stands and duscope exits 1. `--no-save` keeps that scan unsaved, and `--no-scan` fails instead of scanning. `--scan` names a saved scan: `latest`, an id or its first four or more digits, or an index file. A saved scan older than `--max-age` (7 days, or `[defaults] max_age`) is still read, with a `stale_index` warning.
- `scans` lists the saved scans; `scans --prune` removes all but the five newest of each root and those under seven days old, and the temporary files of writes that never finished; `scans --check` checks a saved scan's file for damage.
- A saved scan that cannot be read exits 5 with `index_corrupt`, and one written by a version this duscope cannot read with `index_version_mismatch`.
- `rescan [path]` scans a saved scan's root again with the options it was scanned with (excludes, `-x`, clones, links, attributes, threads, size), and saves the result as a new index whose `parent_scan_id` names the old one, which stays until `scans --prune`; queries then read the new one. `--scan` names the saved scan; `--full` is what every rescan does until the incremental one arrives, and `--incremental` answers `not_available` until then.
- `render [path]` draws the treemap as an SVG picture at any size (`--width`, `--height`; 1200 × 800 by default), to stdout or `-o FILE.svg`: tiles shaded with GrandPerspective's gradient, labelled with names and sizes where they fit, with the legend and the totals under the map. `--layout squarify` (the default) or `gp` (GrandPerspective's bisection, files cut from subfolders), `--color-by`, `--theme`, `--depth` and `--pool-files` work as for the terminal map.
- `render --format png` (or `-o FILE.png`) draws the treemap as a PNG, at most 64 Mi pixels: each tile shaded pixel for pixel by GrandPerspective's gradient, labels in a bundled subset of the Inter font (SIL Open Font License 1.1) that holds ASCII and Latin-1, so other characters show as boxes in a PNG (the SVG uses the browser's fonts). duscope rasterises and encodes it itself, with no graphics library.
- On a case-insensitive volume (APFS, HFS+, exFAT), a path typed in another case finds its directory in a saved scan, as does a name typed composed (`é`) that the filesystem stored decomposed.
- `top`, `stale` and `caches` page: a list `--limit` cuts carries `next_cursor`, and `--cursor` returns the rows after it from the same saved scan. A cursor of another scan or query is refused with `invalid_cursor`.
- `--fields a,b,c` writes those fields of each row, in that order, under `--json`, `--format ndjson` and `--plain`; `--fields help` lists a verb's fields, and an unknown name is refused with `unknown_field`. `--detail` writes every field.
- `-o FILE` writes the output to a file, with no 100-row cap on `--limit`.
- A JSON envelope longer than `--max-chars` (24,000 characters by default; 0 never) goes to a file in the index directory's `out/`, and stdout gets the file's path with the first five rows. `scans --prune` removes such files after a week.
- `--all` streams every row of a list, as ndjson unless `--plain` or `--format` says otherwise.
- On a saved scan, `top`, `top --files`, `stale` and `by-type` answer in milliseconds: on a tree of a million entries, a few milliseconds each.
- `summary [path]` gives a scan's totals under a path: bytes on disk, apparent bytes and bytes as `du` counts them (clones each time), each volume's share with its size and free space at the scan's start, the logical size of dataless placeholders, the counts of files, directories, errors and vanished entries, and when and how the scan ran. Where the path is a mount point, `unaccounted_bytes` says what the volume uses that the scan did not find.
- `explain <path>` gives the facts about one path from the newest saved scan that holds it: its size and times, its links and how many lie outside it, its clone group, the bytes deleting it would free now (exact, or a lower or upper bound), the command that regenerates a cache directory and whether it holds a valid `CACHEDIR.TAG`, what git says of it (ignored, precious, tracked files inside), how sure each of these is, and where it came from. A path no saved scan holds is answered from a fresh `stat`, with low confidence.
- `explain --errors <scan>` lists the directories and entries a saved scan could not read.
- `caches [path]` lists the regenerable cache directories under a path, largest first: `node_modules`, `target`, `.venv`, `build`, `Pods`, `vendor` and the others of the cache rules, each confirmed by its marker file in an ancestor (`package.json`, `Cargo.toml`, …), Xcode's `DerivedData`, and any directory holding a valid `CACHEDIR.TAG`. Each row names the command that regenerates the cache and its project, the nearest directory holding the marker. A cache inside a cache is part of the outer one, and a marker beyond a directory of the rule's own name confirms that directory, not the ones inside it.
- `caches --by-project` gives one row per project, with its caches' bytes and the newest modification time anywhere in the project; `--min-size` (10 MiB by default) then applies to a project's total.
- `caches --loose` also lists a cache's name without the marker that confirms it, marked `"confirmed": false`.
- `diff <old> <new>` lists what changed between two saved scans of one tree, largest change first: entries `added` and `removed` (a whole directory as one entry, with how many it held), files `grown` or `shrunk` by at least `--min-delta` (1 MiB by default), and names whose kind changed. `--depth` compares the directories at that depth whole; `--under` compares a path both scans hold. The envelope adds up the bytes added and removed, the net change and the entries added, removed and changed. Scans of other roots are refused with `incompatible_scans`.
- `query --sql "<SELECT>" [path]` runs one read-only statement over a saved scan's tables and prints its columns and rows (a BLOB as `{"$bytes": …}`; `--plain` as tab-separated values with a heading line). A statement without a `LIMIT` returns 25 rows at most, `--max-rows` (10,000 at most) caps one with a `LIMIT`, and `--timeout` (5 s by default, 60 at most) stops it. The statement runs in a sandbox: the index opens read-only, and writes, `PRAGMA`, `ATTACH`, other tables and functions outside an allow-list are refused with `sql_denied`. `path(id)`, `human_size(bytes)` and `under(id, root_id)` are duscope's own functions, and `:root_id` and `:root_last` name the path's subtree.
- `schema [path]` prints the index's layout for `query --sql`: each table with its columns, their types, whether they may be null and the platforms that fill them, and its rows in the newest saved scan that holds the path (or the one `--scan` names). People get the DDL with each table's rows; `--json` gets `tables[] {name, columns[] {name, type, nullable, platforms}, rows}`.
- `schema --envelope <verb>` prints that verb's envelope as JSON Schema 2020-12; `schema --envelope` alone lists the envelopes.
- `by-type` on a directory inside a saved scan answers from the index rather than reading each entry under it: 44 ms for a directory of 262,000 files, and 18 ms over a whole saved scan of a million entries.
- `--trust FILE`, beside every `--scan` (and for `diff`'s scans, `explain --errors` and `scans --check`), reads an index file outside the index directory. Without it, `--scan FILE` naming one is refused with `untrusted_index` (exit 1), since an index file may come from anyone; the next commands an envelope suggests for such a file carry the `--trust` it needs. A link in the index directory's `scans` is not read as a saved scan.
- `SECURITY.md` gives the threat model: the four untrusted inputs (SQL, agent paths and handles, index files, the scanned tree) and the rule for each, the supported versions and the security-release procedure.
- Every index file opens with its views, triggers and attached databases switched off, statements of at most 64 KiB and values of at most 1 MB. A file whose schema has a virtual table or more than 256 objects, or whose `tool_version` is not a duscope version, is refused as `index_corrupt`.
- duscope warns on stderr when the index directory, its `scans` directory or an index file is open to other users or another user's, with the `chmod` that fixes it.
- `duscope agent` prints a briefing for AI agents, in Markdown: when to reach for duscope, the rules that keep it safe (sizes are bytes, `clean` is a dry run unless `--yes`, never pass `--yes` without a person's word), the flow from `explain` to a handle to `clean`, which command answers which question, every verb and exit code of the binary that prints it and every error code the envelope defines, and SQL examples over a saved scan.
- `duscope agent --skill` writes the briefing as a skill that Claude Code and other agents' clients load on their own: `SKILL.md`, whose description carries the words that should bring duscope to mind (disk space, what is eating my disk, large files, node_modules, stale files, clean up, compare scans), and `references/sql.md` with the SQL examples, in `.agents/skills/duscope/` and `.claude/skills/duscope/` under the working directory or `--dir`. A skill that is already there is replaced only with `--force`.
- `duscope mcp --roots DIR` runs an MCP server on stdin and stdout for AI agents, speaking the 2026-07-28 protocol and the 2025-11-25 `initialize` handshake. A client reaches nothing outside `--roots`: a path holding `..`, a link that leads out, and an index file named for a scan are refused with `refused_path`, and a saved scan whose root lies outside the roots is not found. A tool's result is the envelope its verb prints with `--json`, byte for byte, with a one-line summary; `--max-chars` caps every result as it caps the verbs' output, and `--log-level` sets what reaches stderr. The server lists no tools yet: the verbs arrive as tools in a later release.
- `clean PATH… [--from HANDLE…]` moves files and directories to the Trash: on macOS into the volume's Trash through `NSFileManager`, on Linux into the XDG Trash (the home Trash, or the volume's own `.Trash/$uid` or `.Trash-$uid`), by renaming, never by copying. It plans from a saved scan, the newest that holds the first item or `--scan`, and shows the plan first (each item's bytes, what deleting it would free now with links and clone members outside the plan kept, the command that regenerates it) and moves nothing without `--yes`; on a terminal it asks once and acts only on `yes` typed in full; elsewhere, or with `--no-input`, it answers `confirmation_required`. Moving to the Trash frees nothing until the Trash is emptied: the envelope reports `trashed_bytes` and `freed_bytes: 0`.
- `clean` refuses a symbolic link, a mount point, a macOS firmlink, the scan's root (or `/`), a path outside the saved scan and an item on a read-only volume; one refused item makes the whole run a dry run (`refused_path`) unless `--skip-refused`. An item whose volume has no Trash is left where it is (`trash_unavailable`): `clean` never falls back to deleting.
- `--from HANDLE` acts only on what an `explain --json` handle names: the handle must be unexpired and signed by this duscope, and the path must still hold the same device and inode (`path_changed` otherwise).
- `duscope view [path|scan]` opens the treemap in the interactive viewer, from the newest saved scan that holds the path (or the scan named), scanning it otherwise as the list verbs do; `duscope scan -i` scans and opens the result. The viewer takes the terminal (alternate screen, raw mode, mouse) and gives it back as it found it on `q`, `Ctrl-C`, an error or a crash, redraws when the window is resized, and refuses a terminal below 80×24 or a stdout that is not a terminal. Without `--theme` or `COLORFGBG` it asks the terminal for its background and picks the light pack or Mocha.
- The viewer draws the full screen: a header with the `duscope` badge, the scan's path, the view tabs and the scan's totals; a breadcrumb row to the selection; the map in a titled pane, without labels below the top-level folders; beside it the selected pane (size, counts, shares of the parent and of the scan, type, modification time, mode), the siblings pane (the largest nine, then `… n more`) and the legend for the colour mode; and the key line. From 140 columns the right column is 44 wide, from 110 it is 34, and below that the selected pane folds under the map. The selection opens on the largest file.
- `-vv` prints a line for each worker of the walk: the entries it read and its time in the directory reads.
- On Linux, `-v` prints the walk as two lines, `listing:` and `stat:`, that share the walk's time in the proportion the workers spent listing directories and reading entries' attributes.

### Changed

- `schema_version` is `0.2`, and the envelope changes additively only from here.

## [0.1.0] - 2026-10-30

### Added

- `duscope scan [PATH]`, or `duscope [PATH]` alone, scans a tree in
  parallel and reports how much space it takes; `top`, `tree`, `by-type`
  and `stale` run the same scan before they list. Without a path, the
  current directory is scanned.
- Every verb takes the scan flags: `--threads N` (or `DUSCOPE_THREADS`,
  up to 1,024; empty or 0 leaves the choice to the platform),
  `-x`/`--one-file-system`, `--include-network`, `--follow-symlinks`,
  `--size allocated|apparent` (or `--apparent-size`), `--attrs basic|full`
  and `--clones`/`--no-clones`. An APFS clone's shared blocks count once by
  default; `--clones` on a filesystem without clone ids (anything but APFS)
  warns `clones_unavailable`. Sizes given to flags take `K`, `M`, `G` and
  `T` as powers of 1,024 and `KB`, `MB`, `GB` and `TB` as powers of 1,000.
- Exit codes: 0 for success, unreadable directories below the root
  included; 1 for a runtime error; 2 for a usage error; 3 when the path
  does not exist; 4 when the root cannot be read. Closing the pipe duscope
  writes to (`duscope … | head`) ends it with 0 and no error message.
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
- `top [PATH]` lists the largest directories at `--depth` (default 1), with
  the shallower directories that have none below them, so the rows never
  nest. A row whose directory holds a child of at least 80% of its bytes is
  cut through to that child, up to three levels down, and `via` names the
  directories cut through; `--no-cut-through` keeps strict depth. The
  filters judge the rows as listed, after cut-through.
  `--files` lists the largest files instead and marks a later hard link
  `uncharged`.
- `tree [PATH]` lists the entries down to `--depth` (default 2), largest
  first under each directory, with `pct_of_root` and `pct_of_parent`.
  `--min-pct` leaves out what is smaller than that share of its parent.
  `--limit` keeps the shallowest rows, every row of one depth before any
  row below it.
- `by-type [PATH]` adds up bytes and files by class (code, media, archives,
  data, binaries, docs, other), each with its largest files
  (`--examples N`) by path and `name` (and `name_raw` for a name that is
  not UTF-8). A directory rule such as `node_modules` wins over the
  extension of every file beneath it, and `--classes FILE` merges your own
  table over the built-in one.
- `stale [PATH]` lists files whose modification and change times are both
  older than `--older-than` (default 180d) and at least `--min-size`
  (default 10 MiB). `--atime` also requires the access time to be older
  (it needs `--attrs full`) and warns `atime_unreliable` on a `noatime` or
  `relatime` mount.
- Every list verb takes `--glob`, `--regex`, `--invert`, `--exclude`,
  `--type`, `--min-size`/`--max-size`, `--min-depth`/`--max-depth`, and
  `--time` with `--older-than`/`--newer-than` and `--since`/`--until`
  (RFC 3339 or `YYYY-MM-DD`). The filters applied are listed in
  `query.filters`. `--sort size|count|mtime|path` and `-r` set the order,
  and `--strict` makes unreadable directories exit 4. `--limit` returns up
  to 100 rows (25 by default) and refuses more; `--format ndjson` streams
  every row.
- `duscope man` prints the manual page in roff (`duscope man | mandoc -a`
  shows it). It covers every verb's options, the environment variables
  duscope reads, the exit codes, the file formats, examples, and where
  duscope differs from `du` (`-x` inside a macOS volume group,
  `--exclude-caches` markers, case-sensitive patterns), and it is generated
  from the command line itself, so it lists every flag.
- `duscope completions bash|zsh|fish|elvish|powershell` prints the
  completion script for a shell.
- `scan` on a terminal draws a coloured treemap of the tree, nested to file
  level. Depth-1 folders are boxes titled `name · size`, deeper folders are
  boxes or gutter groups, and files are tiles labelled with their name and
  size where they fit. What is too small to draw is pooled into a
  `+N small` tile. A legend of the file classes, the totals, the first
  warning and the next commands follow the map; further warnings follow
  on stderr.
- `--treemap auto|never|always|pixels`: `always` draws at 80×24 when stdout
  is not a terminal, and `pixels` reports `not_compiled` in this build.
  `--depth` caps how deep the map is drawn (a folder at the cap keeps its
  box and title, so `--depth 1` draws one titled box per top-level
  entry), `--min-tile 1|2|4` raises the smallest tile drawn, and
  `--pool-files` gathers each directory's files into one tile.
- `--theme mocha|tokyo-night|dracula|gruvbox|light|terminal-native|no-color`,
  or the path of a TOML theme file (`docs/rules.md`), also through
  `DUSCOPE_THEME`. Without one, a light terminal (`COLORFGBG`) gets the
  light pack and anything else gets Catppuccin Mocha. Colours are 24-bit
  when `COLORTERM` says so and quantised to 256 colours otherwise.
- `--color-by type|folder|level|age|lscolors` chooses what colours the
  tiles. `lscolors` reads the `*suffix=` entries of `LS_COLORS` (`*.png`,
  `*.tar.gz`, `*README`) and `ex=` for executables (under `--attrs full`,
  which collects modes); folders and files no entry matches keep their
  file-class colour, and the other keys (`di=`, `ln=`, `fi=`, …) are not
  read. Under `NO_COLOR` or `--color never`, tiles are `░▒▓█` shades with
  the class letter in each label, and terminals without UTF-8 get `+-|`
  borders and `#=-+` shades.
- `.deb` packages for Linux on amd64 and arm64, attached to each GitHub
  Release: the statically linked binary, the man page, and bash, zsh and fish
  completions. They install on Debian 12 and Ubuntu 22.04 and later with
  `apt install ./duscope_<version>_<arch>.deb`.
- `brew install kawadeomkar/tap/duscope` installs duscope from the Homebrew
  tap, as a bottle on Apple-silicon macOS and on Linux, with the man page
  and shell completions.
- `--follow-symlinks` follows links to files and directories. A link that
  loops, or leads to a directory on its own path, is recorded as an `ELOOP`
  error of the directory holding it, and the scan goes on. A link that leads
  nowhere stays a link.
- Ctrl-C stops a scan within 200 ms with exit 130 and one line on stderr,
  `duscope: interrupted`, and writes nothing else.
- A root given as a symbolic link is scanned as the directory it leads to:
  `scan.root` is the resolved path, `scan.root_as_given` the path as typed.
  One that leads nowhere is an error (exit 3,
  `scan_not_found`), and one that loops is an `io_error`, instead of a
  scan of the link itself.
- Files and directories that disappear while a scan reads them count in
  `scan.vanished_count`, with a `vanished` warning, instead of as errors.
  A directory replaced by a file after it was listed counts as vanished.
- A directory that cannot be read because of `ELOOP`, `ENAMETOOLONG` or
  `EIO` is listed empty and flagged unreadable, as one that cannot be read
  for permission is, and counts in the `partial_read` warning.
- `scan --exclude GLOB` and `--exclude-from FILE` leave out what matches,
  matched against paths below the root. A pattern starting with `/` is
  anchored at the root, any other matches at every depth, `*` stays within
  a component and `**` crosses them, and a trailing `/` matches
  directories only. Patterns match names case-sensitively, also on a
  case-insensitive volume.
- `--ignore-from FILE` reads gitignore-syntax files, negations
  (`!keep.log`), anchors and directory-only patterns included.
- `--exclude-caches` keeps the scan out of cache directories: a name from
  the cache rules whose marker sits beside it (`node_modules` next to
  `package.json`), or a directory holding a valid `CACHEDIR.TAG`.
- An excluded directory is listed but not entered, and appears in
  `scan.excluded` with `reason` `excluded` or `excluded_cache`. Excluded
  files are left out. `scan.excluded_count` counts both.

### Changed

- `SECURITY.md`: from 0.1.0 until 1.0, the latest minor release receives
  security fixes.
- A scan on macOS runs 16 worker threads by default, up from 8: the
  count the thread sweep on the benchmark trees chose. `--threads` and
  `DUSCOPE_THREADS` still set it.
- The Linux release builds (statically linked with musl, the archives and
  `.deb` packages) allocate with mimalloc: a warm scan of a million entries
  takes 0.39 s on the x86_64 benchmark machine, where musl's own allocator
  took 1.02 s. macOS keeps the system allocator, which mimalloc did not
  beat.
- README.md describes what v0.1.0 does, with a tour of `top`, `tree`,
  `by-type`, `stale`, `--plain` and the treemap, checked against the
  fixture tree by a test.

### Fixed

- A directory holding a million entries is read in chunks instead of
  whole: a scan of one on macOS no longer peaks at gigabytes of memory.
  With `--attrs full`, a scan's peak memory stays within 130 bytes per
  entry on the benchmark trees.

## [0.0.1] - 2026-10-25

The phase-0 placeholder release. The binary only prints its version. The release
reserves the name on crates.io and starts the clocks that the Homebrew and
registry paths wait on.

Clock dates. homebrew-core accepts a formula only when its repository is at
least 30 days old.

- 2026-10-23, 04:05:06 UTC: the repository github.com/kawadeomkar/duscope was
  created. It is 30 days old on 2026-11-22. The repository is also the
  homepage, so the homepage clock is the same one.
- 2026-10-23, 07:28:27 UTC: the tap github.com/kawadeomkar/homebrew-tap was
  created with `brew tap-new --github-packages`, keeping its `tests.yml` and
  `publish.yml` and without `autobump.yml`, because release.yml opens the
  version-bump pull requests.
- 2026-10-23: the `gh-pages` branch was created and GitHub Pages serves it, as
  the host of the apt repository (P5-PKG-01).

<!-- Later channels add the date they count from as another line of the list
     above. At release, scripts/changelog.py dates this heading and adds the
     assembled sections below these notes. -->

### Added

- Release archives on GitHub for macOS (arm64 and x86_64) and Linux (arm64 and
  x86_64, statically linked with musl), each with a build attestation that
  `gh attestation verify` checks, and a shell installer.
- The duscope crates on crates.io, so `cargo install duscope --locked` installs
  the release.
- `README.md`: what duscope is, its pre-release status, and each install
  channel with the release that opens it.
- `SECURITY.md`: how to report a vulnerability privately, through GitHub's
  private vulnerability reporting, with the 90-day disclosure window.
- The `duscope` binary as a pre-release placeholder: `duscope --version` prints the version.
- Each crate on crates.io ships a README and the MIT and Apache-2.0 licence texts.
