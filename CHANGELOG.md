# Changelog

All notable changes to duscope are recorded in this file. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Changes are not written here directly. Each pull request with a user-visible
change adds a fragment, `changelog.d/<ID>.md`, in the format given in
[changelog.d/README.md](changelog.d/README.md). At each release
`scripts/changelog.py` assembles the fragments into the version's dated section
and deletes them.

## [0.0.1] - Unreleased

The phase-0 placeholder release. The binary only prints its version. The release
reserves the name on crates.io and starts the clocks that the Homebrew and
registry paths wait on.

Clock dates. homebrew-core accepts a formula only when its repository is at
least 30 days old.

- 2026-10-23, 04:05:06 UTC: the repository github.com/kawadeomkar/duscope was
  created. It is 30 days old on 2026-11-22. The repository is also the
  homepage, so the homepage clock is the same one.
- 2026-10-23, 07:28:27 UTC: the tap github.com/kawadeomkar/homebrew-tap was
  created with `brew tap-new --github-packages`, keeping its `tests.yml` and
  `publish.yml` and without `autobump.yml`, because release.yml opens the
  version-bump pull requests.
- 2026-10-23: the `gh-pages` branch was created and GitHub Pages serves it, as
  the host of the apt repository (P5-PKG-01).

<!-- Later channels add the date they count from as another line of the list
     above. At release, scripts/changelog.py dates this heading and adds the
     assembled sections below these notes. -->
