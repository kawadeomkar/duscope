### Added
- A configuration file, `~/.config/duscope/config.toml` (or `--config FILE`, `DUSCOPE_CONFIG`; `--config none` reads none), with flag defaults under `[defaults]`, `[excludes] patterns` for every scan, and `[classes]`/`[caches]` extra tables; flags and their environment variables win over it.
- `-v` prints the walk's and the roll-up's timings to stderr, `-vv` each volume and the worker count too, and `--debug` each directory's errors as they happen; `DUSCOPE_LOG` (or `RUST_LOG`) filters the log in `RUST_LOG`'s grammar.
- `-q` keeps warnings off stderr.
