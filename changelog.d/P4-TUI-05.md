### Added

- The viewer's four views: `1` the treemap, `2` the current folder as a table (the `s` sort: size, name, modified, count), `3` the largest files under the map's root, `4` the classes as `by-type` reads them, each a table in the design system's colours.
- `s` sorts the siblings pane and the list; `t` labels the map's tiles of at least 8×2 cells; `c` cycles the colour mode (type, folder, level, age, `LS_COLORS`) and `p` the theme pack, redrawing the map and its legend.
- `/` masks the map by a glob, as `--glob` matches it: the tiles that do not match are not painted, the layout stays, and `Esc` clears it; `⏎` in the legend or the types table filters the map to that row. The tables keep to the mask too.
- `⇥`/`Shift-⇥` move the focus between the map (or table), the siblings pane and the legend, whose arrows then act there; `?` lists every key the viewer answers.
- `y` copies the selection's path with OSC 52 where the terminal allows it, and prints it on the key line either way; `o` opens the selection through `open` or `xdg-open`, detached from the terminal.
