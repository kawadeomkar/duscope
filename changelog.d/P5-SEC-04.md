### Fixed
- A saved scan whose rows place an entry where no scan puts one (two directories that name each other as parent, a directory that names itself, a subtree that ends before its directory or past the scan) is `index_corrupt` (exit 5) for every verb that reads such a row, `rescan` included, where `top --files` read such a file for ever.
