### Changed
- `by-type` on a directory inside a saved scan answers from the index rather than reading each entry under it: 44 ms for a directory of 262,000 files where it took 146 ms. Over a whole saved scan of a million entries it takes 18 ms where it took 51.
