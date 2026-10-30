# Releasing duscope

This is the checklist every `v*` tag follows, step by step, from plan/SPEC.md "Versioning,
branching and releases" (P1-REL-01). A release is a signed tag on a commit of `main`; pushing
it runs [release.yml](../.github/workflows/release.yml), which builds, attests, hosts and
publishes everything. Nothing is published by hand.

Tags `v0.1.0` to `v0.4.0` close phases 1 to 4 and `v1.0.0` the phase-5 gate. Between gates a
patch release (`v0.2.1`) carries `Fixed` and `Security` entries only, never features.

## Release checklist

1. **`main` is green and measured.** The last CI run on `main` passed on all three runners, and
   the latest nightly harness result on the `bench-results` branch is at most 72 hours old and
   within the phase's gate: bench-linux's, while bench-macos is paused
   ([runners.md](runners.md#paused)).

   ```sh
   gh run list --workflow ci.yml --branch main --limit 1
   gh run list --workflow nightly-bench.yml --branch main --limit 3
   ```

2. **The release commit.** On a branch `rel-vX.Y.Z` from `main`, assemble the changelog and bump
   every crate's version in one commit, then merge it through a pull request as usual. The pull
   request adds no fragment (it assembles them), so it carries the `no-changelog` label, which
   the required `changelog` check accepts:

   ```sh
   scripts/changelog.py assemble X.Y.Z          # dates the section, deletes the fragments
   # set version = "X.Y.Z" in [workspace.package] and in the workspace crates'
   # entries of [workspace.dependencies] (Cargo.toml), then:
   cargo update --workspace
   git commit -am "REL: vX.Y.Z"
   ```

3. **Within budget.** On the release commit:

   ```sh
   just size                  # stripped binary against size_budget in the justfile
   just repro                 # twice, on two checkouts: the same SHA-256
   just deny advisories       # and `just deny` for licences, bans and sources
   cargo auditable build --release --locked -p duscope   # the build release.yml makes
   cargo test -p duscope --test dependencies             # DEPENDENCIES.md against cargo metadata
   ```

   `cargo auditable` is clean when its build succeeds: it embeds the Cargo.lock dependency
   list that `just deny advisories` has just checked against the advisory database, as
   release.yml's builds do. The `dependencies` test fails when `DEPENDENCIES.md`'s tables and
   the direct dependencies of the published crates differ (a `cargo tree` count does not
   match the table, which covers all five published crates). The CI jobs `size`, `repro` and
   `build-time` (against its CPU model's budget in `BUILD_TIME_BUDGETS` in `ci.yml`) on that
   commit must be green too.

4. **Tag and watch.** Sign the tag with the key in
   [.github/allowed_signers](../.github/allowed_signers) and push it:

   ```sh
   git tag -s vX.Y.Z -m "duscope X.Y.Z"
   git push origin vX.Y.Z
   # The tag's run: release.yml also runs on every pull request.
   gh run watch "$(gh run list --workflow release.yml --event push --branch vX.Y.Z --limit 1 \
     --json databaseId --jq '.[0].databaseId')"
   ```

   Watch `verify-tag`, the builds, the `.deb` jobs and their install tests, `host` and
   `publish-crates` to the end, then check the attestations:

   ```sh
   gh release download vX.Y.Z --dir /tmp/duscope-vX.Y.Z
   for f in /tmp/duscope-vX.Y.Z/*.tar.* /tmp/duscope-vX.Y.Z/*.deb; do
     gh attestation verify "$f" --repo kawadeomkar/duscope
   done
   ```

   crates.io takes five new crates at once and about one every ten minutes after that, so a
   release that adds crates may need `publish-crates` re-run after the wait (DEV-12).

5. **Homebrew.** `release.yml` opens a pull request in `kawadeomkar/homebrew-tap` with the
   `HOMEBREW_GITHUB_API_TOKEN` secret ([Secrets](#secrets); the tap has no `autobump.yml`, so
   the two never race): the version bump, or, for the first release, when the tap has no
   formula yet, the formula itself, copied from `packaging/homebrew/duscope.rb` with the tag's
   source archive and its checksum. Let the tap's `tests.yml` build the bottles on it (it also
   audits the formula and runs its test), then run the tap's `publish.yml` on the pull
   request, which merges it with the bottle block. A re-run of the release opens no second
   pull request: the job stops when the formula is already at the tag or its pull request is
   open. A change to the formula beyond the version goes into `packaging/homebrew/duscope.rb`
   and, as its own pull request, into the tap.

6. **apt.** `apt.yml` picks up the `.deb` assets, rebuilds the repository with `reprepro`,
   signs it with the subkey (it fails when the subkey expires within 60 days) and publishes it
   to GitHub Pages. Check it in a clean container (from phase 5, when the apt repository opens):

   ```sh
   docker run --rm debian:12 sh -c 'apt-get update && apt-get install -y curl gpg && …'
   ```

7. **After the release.** On a clean machine, `cargo install duscope --locked` and
   `brew install kawadeomkar/tap/duscope`. Commit the release's spec edits in `plan/` (DEV-6),
   and update the Tracker's phase row and current-state line, and the proposal's decision log
   when a gate decision was taken.

8. **Yearly.** At the first release after each anniversary of a secret's creation, rotate the
   secrets in the table below and record the new dates there.

### Patch releases

A patch release (`vX.Y.Z` with Z > 0) follows steps 2 to 4 and 6 to 7. In place of step 1, the
latest nightly result on `main` is at most 72 hours old; the gate is not re-judged.

## Secrets

Every secret the release and benchmark pipelines read, and the one key outside GitHub.

| Name | Where used | Scope | Where set | Created | Expires | Rotation owner |
| --- | --- | --- | --- | --- | --- | --- |
| crates.io trusted publishing | `release.yml`, `publish-crates`: `rust-lang/crates-io-auth-action` trades the job's OIDC token for a crates.io token that lives 30 minutes | the six `duscope*` crates, publishing from `kawadeomkar/duscope`'s `release.yml` only | crates.io, each crate's Settings → Trusted Publishing | at the v0.1.0 release | never (no standing secret) | author |
| `CARGO_REGISTRY_TOKEN` | none from v0.1.0; v0.0.1 published with it | `publish-update` on `duscope*` (DEV-12) | repository secret | 2026-10-25 | revoke after v0.1.0 publishes through trusted publishing | author |
| `HOMEBREW_GITHUB_API_TOKEN` | `release.yml`, the tap's pull request: the version bump, or the first release's formula | fine-grained: contents and pull requests (write) on `kawadeomkar/homebrew-tap` only | repository secret | at the v0.1.0 release | 12 months after creation | author |
| `BENCH_RUNNERS_TOKEN` | `nightly-bench.yml`, the probe that lists the runners (DEV-4) | fine-grained: Administration (read) on `kawadeomkar/duscope`, nothing else | repository secret | 2026-10-25 | 12 months after creation | author |
| `APT_SIGNING_SUBKEY` and `APT_SIGNING_PASSPHRASE` | `apt.yml` (from phase 5) | the signing subkey of the apt key; the primary key stays offline (P5-SEC-02) | repository secrets | at P5-SEC-02 | 12 months after creation; `apt.yml` fails 60 days before | author |
| `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `GEMINI_API_KEY` | the MCP smoke scripts and the eval (from phase 3) | each vendor's key with a monthly budget cap | repository secrets | at phase 3 | 12 months after creation | author |
| Tag-signing key | `git tag -s`; `verify-tag` checks the public half in `.github/allowed_signers` | the author's SSH signing key | the author's machine | before v0.0.1 | at the author's discretion; a new key is a pull request to `allowed_signers` | author |

**Trusted publishing (DEV-22).** From v0.1.0 crates.io publishes through trusted publishing,
not the long-lived `CARGO_REGISTRY_TOKEN`: no standing secret can be stolen and used to
publish, and a crate published from anywhere but `release.yml` on this repository is refused.
crates.io allows it only for a crate that already exists, which all six do since v0.0.1. Before
the v0.1.0 tag the author adds, on crates.io, a trusted publisher to each of the six crates
(repository `kawadeomkar/duscope`, workflow `release.yml`, no environment); after v0.1.0 has
published, the author revokes the token on crates.io and deletes the secret.

## Pulling a release

When a release must be withdrawn, in this order:

1. Yank the crate versions, newest dependent first:

   ```sh
   for crate in duscope duscope-mcp duscope-tui duscope-treemap duscope-core duscope-walk; do
     cargo yank --version X.Y.Z "$crate"
   done
   ```

2. Mark the GitHub Release as a pre-release and delete its assets, so the shell installer's
   "latest" skips it:

   ```sh
   gh release edit vX.Y.Z --prerelease
   gh release view vX.Y.Z --json assets --jq '.assets[].name' |
     xargs -n1 gh release delete-asset vX.Y.Z --yes
   ```

3. Open a pull request in `kawadeomkar/homebrew-tap` that reverts the formula and its bottle
   block to the previous release.
4. Remove the version from the apt repository and publish the previous `.deb` again (from
   phase 5): `reprepro remove stable duscope`, then re-run `apt.yml` on the previous release.
5. Add a `Removed` line for the version to `CHANGELOG.md` through a fragment.

The version number is burned: the fix ships as the next patch release. Tags are never deleted.
