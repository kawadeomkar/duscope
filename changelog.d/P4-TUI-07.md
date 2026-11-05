### Added

- `--treemap pixels` draws the treemap in pixels, shaded as `render` shades it,
  on a terminal that shows images through Kitty graphics, Sixel or iTerm2's
  inline images, in the static map and in `scan -i`'s viewer. duscope asks the
  terminal which of them it speaks only when the flag is given, and waits at
  most two seconds for an answer; where none comes, or stdin and stdout are
  not a terminal, the map is drawn in cells, with a warning that says why. It
  needs the `pixels` Cargo feature, which the Homebrew formula, the `.deb`
  packages and the release archives do not turn on: `cargo install duscope
  --features pixels` builds it, on Rust 1.90 or later. A build without it
  answers `--treemap pixels` with exit 2 and `not_compiled`, as before.
