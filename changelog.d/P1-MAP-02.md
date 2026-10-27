### Added

- `scan` on a terminal draws a coloured treemap of the tree, nested to file
  level. Depth-1 folders are boxes titled `name · size`, deeper folders are
  boxes or gutter groups, and files are tiles labelled with their name and
  size where they fit. What is too small to draw is pooled into a
  `+N small` tile. A legend of the file classes, the totals, the first
  warning and the next commands follow the map.
- `--treemap auto|never|always|pixels`: `always` draws at 80×24 when stdout
  is not a terminal, and `pixels` reports `not_compiled` in this build.
  `--depth` caps how deep the map is drawn, `--min-tile 1|2|4` raises the
  smallest tile drawn, and `--pool-files` gathers each directory's files
  into one tile.
- `--theme mocha|tokyo-night|dracula|gruvbox|light|terminal-native|no-color`,
  or the path of a TOML theme file (`docs/rules.md`), also through
  `DUSCOPE_THEME`. Without one, a light terminal (`COLORFGBG`) gets the
  light pack and anything else gets Catppuccin Mocha. Colours are 24-bit
  when `COLORTERM` says so and quantised to 256 colours otherwise.
- `--color-by type|folder|level|age|lscolors` chooses what colours the
  tiles. Under `NO_COLOR` or `--color never`, tiles are `░▒▓█` shades with
  the class letter in each label, and terminals without UTF-8 get `+-|`
  borders and `#=-+` shades.
