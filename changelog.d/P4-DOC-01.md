### Added

- The manual page describes the interactive viewer and `clean`. INTERACTIVE VIEWER says
  how `duscope view` and `duscope scan -i` find their scan and when they refuse to start,
  what the screen, its panes and the four views show, what every key does, and how the
  mark pane moves marked entries to the Trash. CLEANING UP says what `clean`'s plan holds
  and when it acts, what each refusal reason means, where the Trash is on macOS and on
  Linux and how `--permanent` differs, and how `--from` handles are checked. The README
  lists the viewer's keys.

### Fixed

- `README.md` said that no release had a command that deletes. `clean` came with v0.2.0;
  the README now says that it is a dry run unless given `--yes`, moves to the Trash unless
  given `--permanent`, and refuses a symbolic link, a mount point and the scanned tree's
  root.
- `README.md` said that the interactive viewer and the MCP server arrive in later releases.
  v0.2.0 carries the viewer, `clean`, `duscope agent` and the MCP server, whose tools come
  with v0.3; the README now says so.
