### Added

- The viewer's four views: `1` the treemap, `2` the current folder as a table (the `s` sort: size, name, modified, count), `3` the largest files under the map's root, `4` the classes as `by-type` reads them, each a table in the design system's colours.
- `s` sorts the siblings pane and the list; `t` labels the map's tiles of at least 8×2 cells; `c` cycles the colour mode (type, folder, level, age, `LS_COLORS`) and `p` the theme pack, redrawing the map and its legend.
- `/` masks the map by a glob, as `--glob` matches it: the tiles that do not match are not painted, the layout stays, and `Esc` clears it; `⏎` in the legend or the types table filters the map to that row. The tables keep to the mask too; under a mask the largest files are ranked from the first 262,144 entries below the map's root, and where it holds more the title says so.
- `⇥`/`Shift-⇥` move the focus between the map (or table), the siblings pane and the legend, whose arrows then act there; `?` lists every key the viewer answers.
- `y` copies the selection's path with OSC 52 where the terminal allows it, and prints it on the key line either way; `o` shows the selection in the file manager without opening it, so an application or script in a browsed folder does not run: Finder's reveal on macOS; on Linux the file manager selects it (`org.freedesktop.FileManager1`, through `dbus-send`) or, where that is not available, `xdg-open` opens the folder holding it.

### Fixed

- The siblings pane no longer reads every entry of the selection's folder on each frame to add up its `… n more` row: the folder's list is read once, in the order the index keeps it, with the sizes totalled as they run, so in a folder of a million files a move draws in under a millisecond, not a third of a second.
