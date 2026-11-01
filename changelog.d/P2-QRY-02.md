### Added
- `top`, `stale` and `caches` page: a list `--limit` cuts carries `next_cursor`, and `--cursor` returns the rows after it from the same saved scan. A cursor of another scan or query is refused with `invalid_cursor`.
- `--fields a,b,c` writes those fields of each row, in that order, under `--json`, `--format ndjson` and `--plain`; `--fields help` lists a verb's fields, and an unknown name is refused with `unknown_field`. `--detail` writes every field.
- `-o FILE` writes the output to a file, with no 100-row cap on `--limit`.
- A JSON envelope longer than `--max-chars` (24,000 characters by default; 0 never) goes to a file in the index directory's `out/`, and stdout gets the file's path with the first five rows. `scans --prune` removes such files after a week.
- `--all` streams every row of a list, as ndjson unless `--plain` or `--format` says otherwise.

### Changed
- On a saved scan, `top`, `top --files`, `stale` and `by-type` answer in milliseconds: on a tree of a million entries, a few milliseconds each where they took up to a second.
