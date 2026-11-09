### Added

- `d` marks the selection, and `d` again unmarks it; `D` opens the mark pane, which shows the plan as `clean` prints it and moves the marks to the Trash when `yes` is typed in full. A plan with a refused item moves nothing, as `clean` without `--skip-refused`. What moved stays on the map as a freed tile until the next rescan, and the viewer says what moved when it closes.
- `r` rescans the current root with its saved options, beside the viewer, and reloads when the new index lands, keeping the view, the mask and the cursor's path.
- The mouse: a click selects the deepest tile under the cell, a row in the siblings pane or a table, and filters by a legend row; a double click zooms, the wheel walks the path. There is no drag.
