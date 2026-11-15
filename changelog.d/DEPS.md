### Changed

- Building duscope from source needs Rust 1.89 or later; it needed 1.88.
- The treemap and the viewer measure names by Unicode 18's character widths, where they used
  Unicode 17's: the halfwidth katakana sound marks `ﾞ` and `ﾟ` (U+FF9E, U+FF9F) take a column
  each, as terminals draw them, where they took none, and the characters Unicode 18 adds take
  the width it gives them.
