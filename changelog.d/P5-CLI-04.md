### Fixed

- The `partial_read` warning's hint offers `--strict` only after a list verb (`top`, `tree`, `by-type`, `stale`, `caches`), the verbs that take it. After `scan`, `rescan`, `import`, `summary`, `query`, `export`, `render` and `view` it says to run duscope again as a user who can read the directories, where it named a flag those verbs refuse as a usage error.
- The `scan_age` warning of `stale` names the command that measures the tree again, then `stale` again: `duscope rescan` of the saved scan it read, or for an import `duscope scan --save` of its root. It said to run `stale` again, which reads the same saved scan.
- A failed save of `rescan` and of `import` (`index_write_failed`) names that verb in its hint, where it named `duscope scan --save`. `rescan`'s names the saved scan by its file (`--trust FILE --scan FILE`), which it still finds with `--index-dir` naming another directory.
