### Added

- `duscope man` prints the manual page in roff (`duscope man | mandoc -a`
  shows it). It covers every verb's options, the environment variables
  duscope reads, the exit codes, the file formats and examples, and it is
  generated from the command line itself, so it lists every flag.
- `duscope completions bash|zsh|fish|elvish|powershell` prints the
  completion script for a shell.
