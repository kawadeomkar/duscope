### Changed

- `by-type` with a large `--examples` keeps each class's largest files in time that grows with the logarithm of the number asked for, not the number itself, wherever it counts the files one by one: a scan run for the command, a filter, `--size apparent`, `--sort mtime`, or a folder below a saved scan's root. On a million files with `--examples 100000` that step went from up to 20 s to under 0.05 s. The examples listed, and their order, are the same.
