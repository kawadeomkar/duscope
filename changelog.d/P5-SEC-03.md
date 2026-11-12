### Fixed
- `explain`, and so `clean`'s plan, the `duscope_explain` tool and the viewer's facts, no longer waits for ever on a FIFO, or fills memory from a device, put where git keeps a file: `.git`, `commondir`, a configuration file, a `.gitignore`, `info/exclude`, the excludes file or the index. Each is read only when it is a regular file and only up to a cap (64 KiB for `.git` and `commondir`, 1 MiB per configuration file, 1 MiB for all the ignore files one lookup reads together, 1 GiB for the index); otherwise the git lookup fails, and `explain` reports `git: null` with low `confidence` and the reason in `evidence`. A configuration file or `commondir` that cannot be read is passed over, as before.

### Changed
- `explain` no longer follows a `.gitignore` that is a symbolic link: it reads as absent, as git has read one since 2.32.
