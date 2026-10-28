### Added

- `duscope scan [PATH]`, or `duscope [PATH]` alone, scans a tree in
  parallel and reports how much space it takes; `top`, `tree`, `by-type`
  and `stale` run the same scan before they list. Without a path, the
  current directory is scanned.
- Every verb takes the scan flags: `--threads N` (or `DUSCOPE_THREADS`,
  up to 1,024; empty or 0 leaves the choice to the platform),
  `-x`/`--one-file-system`, `--include-network`, `--follow-symlinks`,
  `--size allocated|apparent` (or `--apparent-size`), `--attrs basic|full`
  and `--clones`/`--no-clones`. Sizes given to flags take `K`, `M`, `G` and
  `T` as powers of 1,024 and `KB`, `MB`, `GB` and `TB` as powers of 1,000.
- Exit codes: 0 for success, unreadable directories below the root
  included; 1 for a runtime error; 2 for a usage error; 3 when the path
  does not exist; 4 when the root cannot be read. Closing the pipe duscope
  writes to (`duscope … | head`) ends it with 0 and nothing on stderr.
