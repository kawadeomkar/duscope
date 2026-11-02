# Documentation

The index of duscope's documentation: the documents in this directory, then the
ones at the top of the repository.

## In this directory

- [github-settings.md](github-settings.md): the repository settings duscope
  depends on (branch protection and the required checks, merge methods,
  Actions, security, labels), as a checklist with the command that applies each.
- [index-format.md](index-format.md): the SQLite file a saved scan is, table by
  table and column by column, with its flags, accounting, `scan_meta` keys, how
  to read it with `sqlite3`, DuckDB or `duscope query --sql`, and what duscope
  refuses to open.
- [manual-checks.md](manual-checks.md): the acceptance checks done by hand
  (GrandPerspective opening an export, browsers drawing `render`'s SVG), with
  the program's version and the last result.
- [RELEASING.md](RELEASING.md): the checklist every release tag follows, the
  patch-release subset, the security-release procedure, the secrets table and
  how to pull a release.
- [rules.md](rules.md): the schemas of `classes.toml`, `caches.toml` and user
  theme files, with an example of each.
- [runners.md](runners.md): the two self-hosted machines the nightly benchmarks
  run on, how each is set up and registered, its network and security limits,
  and what to do when one is offline.

More documents arrive with the features they describe.

## At the top of the repository

- [README.md](../README.md): what duscope is, and how to install it.
- [CONTRIBUTING.md](../CONTRIBUTING.md): setup, the pinned tools, the `just`
  recipes, the package workflow, tests, benchmarks, packaging and releases.
- [CLAUDE.md](../CLAUDE.md): the rules for agents working in this repository.
- [SECURITY.md](../SECURITY.md): the threat model, how to report a
  vulnerability, the supported versions and how a fix is released.
- [CHANGELOG.md](../CHANGELOG.md): what changed in each release.
  [changelog.d/README.md](../changelog.d/README.md) gives the format of the
  fragments it is assembled from.
- [DEPENDENCIES.md](../DEPENDENCIES.md): every dependency, why it is there, and
  the policy for adding one.
- [VENDOR.md](../VENDOR.md): code copied into the repository from other
  projects, and how it is kept in sync.
