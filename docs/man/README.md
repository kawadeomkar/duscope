# The manual page

`duscope man` prints the page in roff, and `duscope man | mandoc -a` (or `man -l -`) shows it. Packages install it as `duscope.1`.

The page is generated from the clap tree, so a flag added to the command line is in the page and the shell completions without further work. `clap_mangen` writes the name, the synopsis, the description and the options. Each flag is described once: the scan and output flags of `duscope` itself under SCAN OPTIONS and OUTPUT OPTIONS, the filters the list verbs share under FILTER OPTIONS, and under VERBS each verb with its usage and the flags only it takes. Flags marked hidden (the test-only ones) are left out of the page and the completions.

The sections clap knows nothing of are written by hand in roff, in `duscope/src/verbs/man/`:

| File | Section |
| --- | --- |
| `environment.roff` | ENVIRONMENT: every variable duscope reads (the `DUSCOPE_TEST_*` knobs are never documented) |
| `exit-status.roff` | EXIT STATUS: the exit codes of the conventions |
| `files.roff` | FILES: theme files, classes tables, ignore and pattern files |
| `examples.roff` | EXAMPLES |
| `caveats.roff` | CAVEATS: where duscope differs from what a `du` user expects (`-x` inside a macOS volume group, `--exclude-caches` markers, case-sensitive patterns) |

They live beside the code rather than in this directory because the published crate ships only its `src/`, and the page is built into the binary. `duscope/src/verbs/man.rs` checks that every visible flag appears in the page and no hidden one does. `mandoc -T lint` checks the roff.

`duscope completions bash|zsh|fish|elvish|powershell` prints the completion script for a shell. The tests check that bash, zsh and fish load it.
