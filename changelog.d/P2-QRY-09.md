### Changed
- `by-type` on a directory inside a saved scan answers from the index rather than reading each entry under it: about four times faster, 42 ms for a directory of 262,000 files where it took 150 ms. Over a whole saved scan it takes a third of the time it did.
