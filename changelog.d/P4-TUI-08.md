### Fixed

- `view` and `scan -i` give the terminal back when `SIGTERM` (a plain `kill`) or `SIGHUP` (the
  terminal closed) ends them, and still end by that signal: the shell is no longer left in raw
  mode on the alternate screen, with the mouse reported.
- Resizing the viewer's window draws one frame, at the size the window comes to, not one for
  every size it passed through.
- A click or an arrow key onto a `+N small` tile reads the folder's entries once, not on every
  click: in a folder of 100,000 files ten clicks read each entry once, where they read it ten
  times. Under a filter it lands on the largest entry the filter keeps.
