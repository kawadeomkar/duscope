### Changed

- An incremental `rescan` (macOS) takes the directories that did not change from the saved scan faster when there are many of them: it prepares its two reads of the saved scan once rather than for each directory it takes. On a tree where it took 19,380 subtrees, that step went from 0.13 s to 0.03 s.
