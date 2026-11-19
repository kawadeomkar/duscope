# Releasing duscope

This is the checklist every `v*` tag follows, step by step, from plan/SPEC.md "Versioning,
branching and releases" (P1-REL-01). A release is a signed tag on a commit of `main`, or, for
a patch `main` cannot carry, of its hotfix branch
([From a hotfix branch](#from-a-hotfix-branch)); pushing it runs
[release.yml](../.github/workflows/release.yml), which builds, attests, hosts and publishes
everything. Nothing is published by hand.

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

4. **Tag and watch.** On `main` at the release commit, sign the tag with the key in
   [.github/allowed_signers](../.github/allowed_signers) and push it. A squash merge leaves the
   pull request's own commits off `main`, and `verify-tag` refuses a tag on one of them:

   ```sh
   git switch main && git pull --ff-only     # the release commit, not a branch's
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
   for f in /tmp/duscope-vX.Y.Z/duscope-*.tar.xz /tmp/duscope-vX.Y.Z/*.deb; do
     gh attestation verify "$f" --repo kawadeomkar/duscope
   done
   ```

   Each binary archive and `.deb` carries an attestation; `source.tar.gz`, cargo-dist's source
   archive, carries none, so the loop leaves it out.

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

A security release may be tagged without a nightly result of the last 72 hours when no bench
runner reports (the nightly skips every platform:
[runners.md](runners.md#when-a-runner-is-offline)), and its release notes say so in one
sentence. Minor releases, and the other patch releases, keep the rule. The sentence is the
note of the version's section: before the assemble, add the section undated above the newest
one in `CHANGELOG.md` with the sentence under it, and `scripts/changelog.py assemble` dates the
heading and keeps the sentence above the assembled sections, which the GitHub Release takes as
its notes:

```markdown
## [X.Y.Z] - Unreleased

No nightly benchmark result of the last 72 hours stands behind this release: no benchmark runner was reporting.
```

#### From a hotfix branch

A patch is tagged on `main` when `main` can be released as it stands. When it cannot, because
it holds work since the minor's tag that a patch must not carry, the patch ships from
`hotfix/vX.Y.Z`, a branch from the tag of the minor's latest release (`vX.Y.0`, or its
latest patch). `verify-tag` accepts a tag whose commit is on `main` or on the hotfix branch of
the tag's own name, and only a patch's: a `vX.Y.0` or a pre-release on a hotfix branch, and a
tag on any other branch, release nothing. The branch holds the fix, the backports of `Fixed`
entries, the version bump and the assembled changelog; nothing else ships. In order:

1. **The branch.** Make it from the tag and push it:

   ```sh
   git switch -c hotfix/vX.Y.Z vX.Y.W        # the minor's latest release
   git push origin hotfix/vX.Y.Z
   ```

   A tag runs the `release.yml` of the commit it tags. On a branch from a tag made before
   `verify-tag` accepted hotfix branches, that copy refuses the tag, so the branch first takes
   the change itself: a cherry-pick of the commit on `main` that made it
   (`git log --oneline origin/main -- .github/workflows/release.yml` lists it), with any later
   fix of `release.yml` the release needs.
2. **The fix and the backports.** Each is a `git cherry-pick -x` of its commit on `main`, with
   its fragment cut to the `Fixed` or `Security` lines it ships, in a pull request into
   `hotfix/vX.Y.Z` labelled `full-ci`, which runs `main`'s full set of checks. No branch
   protection holds a hotfix branch
   ([github-settings.md](github-settings.md#owned-by-other-packages)), so merge each only once
   its checks pass.
3. **The release commit.** Steps 2 and 3, on a branch `rel-vX.Y.Z` from `hotfix/vX.Y.Z` and
   through a pull request into it, labelled `full-ci` as well as `no-changelog`.
4. **The tag.** Step 4 on the branch, at the release commit. `verify-tag` reads the signers
   from `main`'s `.github/allowed_signers`, as for every tag. Keep the branch afterwards: a
   re-run of the whole release verifies the tag again.

   ```sh
   git switch hotfix/vX.Y.Z && git pull --ff-only
   git tag -s vX.Y.Z -m "duscope X.Y.Z"
   git push origin vX.Y.Z
   ```

   The GitHub Release is marked latest, and the tap's formula bumped, only when no release
   tag names a higher version, so a patch of an older minor leaves both at the newer one.
5. **Back on `main`.** After the release, a pull request on `main`, `REL: vX.Y.Z on main` with
   the `no-changelog` label, adds the version's section to `CHANGELOG.md` and removes the
   fragments whose entries the release shipped, so the next minor does not list them again.
   `CHANGELOG.md` changes only at a release, so while `main` has made none since the branch's
   tag, the tag's file is `main`'s with the new section; otherwise copy the section in by
   hand, in version order. The release commit deleted the fragments it assembled: remove each
   from `main`, or, where `main`'s copy holds more than the release shipped, those lines alone.

   ```sh
   git switch -c rel-vX.Y.Z-main origin/main
   git restore --source vX.Y.Z CHANGELOG.md
   git diff --numstat CHANGELOG.md          # lines added, none removed
   git diff --name-only --diff-filter=D vX.Y.Z^ vX.Y.Z -- changelog.d   # the shipped fragments
   # remove those fragments, or cut them, then
   git commit -am "REL: vX.Y.Z on main"
   ```

### Security releases

A vulnerability reported through [SECURITY.md](../SECURITY.md) is fixed in private, then ships
as a patch release of the latest minor, the only one supported before 1.0. In order:

1. **The advisory.** Accept the private report: it becomes the draft GitHub security advisory
   (GHSA). Set the affected versions (`< X.Y.Z`, the coming patch release) and the severity
   there, and keep the discussion in it.
2. **The CVE.** Request one from the advisory page ("Request CVE"). GitHub assigns it, usually
   within a few days, and it shows on the advisory.
3. **The fix.** Open the advisory's temporary private fork ("Start a temporary private fork"),
   and invite the reporter to it when they want to review. The fix, its tests and a fragment
   `changelog.d/GHSA-xxxx-xxxx-xxxx.md` go there, on a branch from `main`. The fragment holds
   only a `### Security` heading and one line naming the advisory and the CVE. CI does not run
   in the private fork, so run `just check-full` on both platforms before merging. Then merge
   the fix from the advisory page into `main`.
4. **The release.** Follow [Patch releases](#patch-releases) at once, without a recent nightly
   result if no bench runner reports, as it says. From `main`, the changelog section is the
   `Security` section alone. `scripts/changelog.py assemble` takes every fragment, so any
   other fragment waiting on `main` is moved aside for the assemble and put back before the
   release commit, for the next release:

   ```sh
   held=$(mktemp -d)
   git ls-files 'changelog.d/*.md' ':!changelog.d/README.md' ':!changelog.d/GHSA-*.md' |
     xargs -I{} mv {} "$held"/
   scripts/changelog.py assemble X.Y.Z
   for f in "$held"/*.md; do [ -e "$f" ] && mv "$f" changelog.d/; done
   ```

   A tag on `main` carries whatever else `main` holds. When that includes work a patch must
   not carry, the release ships from `hotfix/vX.Y.Z` instead
   ([From a hotfix branch](#from-a-hotfix-branch)): the fix, once merged into `main`, is
   cherry-picked onto the branch with its fragment, beside any `Fixed` backports, and the
   section holds their lines alone. No fragment is moved aside there, and the pull request
   back on `main` removes the fix's fragment with the others the release shipped.
5. **Disclosure.** Publish the advisory once the release's assets and crates are live, and no
   later than 90 days after the report. Publishing it adds it, with the CVE, to the GitHub
   Advisory Database, from which Dependabot alerts the crates' dependents.
6. **RUSTSEC.** When a library crate is affected (`duscope-core`, `duscope-walk`,
   `duscope-treemap`, `duscope-tui` or `duscope-mcp`), open a pull request in
   [rustsec/advisory-db](https://github.com/rustsec/advisory-db) that adds
   `crates/<crate>/RUSTSEC-0000-0000.md` (the database assigns the number), with the crate,
   the date, the advisory's URL, the GHSA and CVE ids as `aliases`, and the fixed version
   under `patched`. `cargo audit` and `cargo deny` then report the vulnerable versions to the
   crate's dependents.
7. **The affected versions.** A version too dangerous to leave installable is pulled as
   [Pulling a release](#pulling-a-release) says. Otherwise it stays, and the advisory names
   the fix.

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

**The tap token.** `HOMEBREW_GITHUB_API_TOKEN` is used only at the end of a release, when the
`homebrew` job pushes the tap's branch, so a token without write access fails there with a 403
("Permission to kawadeomkar/homebrew-tap.git denied"). Create it on GitHub's fine-grained token
page (`github.com/settings/personal-access-tokens/new`) with repository access "Only select
repositories" set to `kawadeomkar/homebrew-tap` and the repository permissions Contents and
Pull requests set to "Read and write" (Contents "Read-only" is the 403). GitHub shows the value
once: while it is on screen, check that it can push, then store it with
`gh secret set HOMEBREW_GITHUB_API_TOKEN --repo kawadeomkar/duscope`:

```sh
read -rs T   # paste the token
sha=$(gh api repos/kawadeomkar/homebrew-tap/commits/main --jq .sha)
GH_TOKEN=$T gh api -X POST repos/kawadeomkar/homebrew-tap/git/refs -f ref=refs/heads/token-check -f sha="$sha" --jq .ref &&
  GH_TOKEN=$T gh api -X DELETE repos/kawadeomkar/homebrew-tap/git/refs/heads/token-check &&
  echo "the token can push to the tap"
unset T
```

A failed `homebrew` job is re-run alone once the secret holds a working token
(`gh run rerun <run> --failed`); it opens no second pull request.

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
