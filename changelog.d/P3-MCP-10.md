### Added
- `duscope_top`, `duscope_by_type`, `duscope_stale` and `duscope_caches` take `strict`, the tools' `--strict`: where the scan could not read some directories, the call fails with `permission_denied` instead of answering with a `partial_read` warning, whose hint offers `--strict`.
