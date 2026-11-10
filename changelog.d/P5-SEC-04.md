### Fixed
- A saved scan whose rows place an entry where no scan puts one (two directories that name each other as parent, a directory that names itself, a subtree that ends before its directory or past the scan) no longer makes any verb read for ever, as `top --files` did. A verb that reads such a row fails `index_corrupt` (exit 5), `rescan` included; a verb that does not may answer from the rows it read, with no warning.
- `scans --check` reads every row of a saved scan and fails `index_corrupt` (exit 5) on one out of the layout, before it sweeps the tree.
