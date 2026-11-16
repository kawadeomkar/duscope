# The apt repository

duscope's signed apt repository, at `https://kawadeomkar.github.io/duscope/apt`,
serves the `.deb` packages of the GitHub releases for amd64 and arm64. With it,
`apt install duscope` installs duscope on Debian 12 and later and Ubuntu 22.04
and later, and `apt upgrade` keeps it up to date. The packages are the ones
attached to each release: one statically linked binary (musl), which runs on any
glibc, with the manual page and the shell completions.

**Not live yet.** The repository opens once its signing key exists (the key
ceremony, P5-SEC-02, which records the key's fingerprint [below](#the-signing-key)),
with the first release after that. Until then, install the `.deb` attached to a
release, as the [README](../README.md#debian-and-ubuntu) says.

## Installing

The repository's key goes into a keyring of its own, which the sources file names
with `Signed-By`, so apt trusts it for this repository and no other:

<!-- apt-install:begin -->
```sh
sudo apt-get update
sudo apt-get install -y ca-certificates curl
sudo install -d -m 0755 /etc/apt/keyrings
sudo curl -fsSLo /etc/apt/keyrings/duscope.gpg https://kawadeomkar.github.io/duscope/apt/duscope.gpg
sudo curl -fsSLo /etc/apt/sources.list.d/duscope.sources https://kawadeomkar.github.io/duscope/apt/duscope.sources
sudo apt-get update
sudo apt-get install -y duscope
```
<!-- apt-install:end -->

Each time it publishes the repository, the workflow that builds it runs these
commands, as they stand here, in clean Debian 12 and Ubuntu 22.04 containers on
amd64 and arm64, and fails if `apt-get update` prints a warning.

The sources file is [apt/duscope.sources](../apt/duscope.sources):

```text
Types: deb
URIs: https://kawadeomkar.github.io/duscope/apt
Suites: stable
Components: main
Architectures: amd64 arm64
Signed-By: /etc/apt/keyrings/duscope.gpg
```

To check the key you fetched, compare the fingerprint gpg prints with the one in
[The signing key](#the-signing-key) (`sudo apt-get install -y gpg` first where
gpg is missing, as in minimal images):

```sh
gpg --show-keys /etc/apt/keyrings/duscope.gpg
```

### The testing suite

`stable` holds the newest release. `testing` holds the newest release or
pre-release (a `vX.Y.Z-rc.N` tag), so it differs from `stable` only while a
pre-release is newer than every release. To follow it:

```sh
sudo sed -i 's/^Suites: stable$/Suites: testing/' /etc/apt/sources.list.d/duscope.sources
sudo apt-get update
```

### Removing

```sh
sudo apt-get remove duscope
sudo rm /etc/apt/sources.list.d/duscope.sources /etc/apt/keyrings/duscope.gpg
sudo apt-get update
```

## The signing key

The repository's `Release` files are signed by a subkey of the duscope apt key
(P5-SEC-02, DEV-140):

- **The primary key** (ed25519) certifies and never expires. The key ceremony
  generates it on an offline machine and moves it onto a hardware token, with its
  revocation certificate and an encrypted backup kept offline: no online machine
  holds it.
- **The signing subkey** (ed25519) is valid for twelve months. Its secret, under a
  passphrase of its own, is the `APT_SIGNING_SUBKEY` secret of
  [apt.yml](../.github/workflows/apt.yml), and its passphrase
  `APT_SIGNING_PASSPHRASE`.

`duscope.gpg` holds the public halves of both: `apt/duscope.gpg` in this
repository, which apt.yml publishes beside the packages. The primary key's
fingerprint and the signing subkey's expiry:

```text
fingerprint: PLACEHOLDER, no key yet: the key ceremony (P5-SEC-02) writes the primary key's fingerprint here
signing subkey expires: PLACEHOLDER
```

apt.yml publishes nothing unless `apt/duscope.gpg` exists, its primary key has the
fingerprint above, and its newest signing subkey expires on the date above
(`scripts/apt-repo.sh check-key`). Until the ceremony the file does not exist, so
every run of apt.yml after a release, and every dispatch, fails at its first job's
key check and says so; a pull request's `repo-test`, with a throwaway key, needs
none.

## When the key changes

The signing subkey is replaced every year, before it expires; the primary key,
and the fingerprint above, stay. The keyring on your machine holds the subkeys it
had when you fetched it, so after a rotation `apt-get update` no longer accepts
the repository's `Release` file: it keeps the lists it had, so duscope stops
being upgraded, and warns, naming the new subkey:

```text
W: An error occurred during the signature verification. The repository is not updated and the previous index files will be used. GPG error: https://kawadeomkar.github.io/duscope/apt stable InRelease: The following signatures couldn't be verified because the public key is not available: NO_PUBKEY <the new subkey's ID>
```

Fetch the key again, check that its fingerprint is still the one above, and
update:

```sh
sudo curl -fsSLo /etc/apt/keyrings/duscope.gpg https://kawadeomkar.github.io/duscope/apt/duscope.gpg
gpg --show-keys /etc/apt/keyrings/duscope.gpg
sudo apt-get update
```

The release notes of the first release signed by a new subkey say so. Should the
primary key itself ever change (its token lost, or the key revoked), its
fingerprint above changes, the release notes say why, and the same commands
fetch the new key: check its fingerprint against this page on GitHub, not
against the file you downloaded.

### Rotating the signing subkey

For the maintainer. apt.yml refuses a subkey that expires within 60 days
(`scripts/apt-repo.sh check-expiry`), so the subkey is rotated more than 60 days
before its expiry, about ten months after it was made: earlier than step 8's yearly
rotation of the secrets ([RELEASING.md](RELEASING.md#secrets)), which comes after
the anniversary, when a twelve-month subkey has expired:

1. On the offline machine, with the token, add a signing subkey valid twelve
   months, and export its secret alone, under a new passphrase:

   ```sh
   gpg --quick-add-key <primary fingerprint> ed25519 sign 1y
   gpg --armor --export-secret-subkeys '<new subkey fingerprint>!' > subkey.asc
   gpg --export <primary fingerprint> > duscope.gpg
   ```

2. A pull request replaces `apt/duscope.gpg` with that export, which holds the old
   subkey and the new one, and writes the new subkey's expiry into
   `signing subkey expires:` above. Until the secrets change, the old subkey
   still signs.
3. Set the secrets (`gh secret set APT_SIGNING_SUBKEY < subkey.asc`, then
   `gh secret set APT_SIGNING_PASSPHRASE`), delete `subkey.asc`, and record the
   dates in the secrets table of [RELEASING.md](RELEASING.md#secrets).
4. Run apt.yml for the newest release, which signs with the new subkey:
   `gh workflow run apt.yml -f tag=vX.Y.Z`.
5. A `changelog.d` fragment under `### Changed` tells users to fetch the key
   again, as [When the key changes](#when-the-key-changes) says.

## How the repository is built

[apt.yml](../.github/workflows/apt.yml) runs after each successful run of
release.yml for a tag, and on a dispatch that names a release
(`gh workflow run apt.yml -f tag=vX.Y.Z`). Every run rebuilds the repository from
the GitHub Releases, with `reprepro` and the configuration in
[apt/conf/distributions](../apt/conf/distributions), so it holds what the
releases hold and nothing else:

- `stable`: the newest release, by version;
- `testing`: the newest release or pre-release;

each among the releases that still have both `.deb` assets. A pulled release,
whose assets are deleted ([RELEASING.md](RELEASING.md#pulling-a-release)), drops
out at the next run, and a patch of an older minor leaves both suites at the newer
release.

The run checks the key first (above), then downloads each suite's `.deb`s and
checks each against the attestation release.yml made for it at that tag, imports
the subkey and fails when it expires within 60 days, builds and signs the
repository and verifies its signatures against `apt/duscope.gpg`, installs
duscope from it in Debian 12 and Ubuntu 22.04 containers on both architectures,
and only then replaces `apt/` on the `gh-pages` branch, which GitHub Pages serves,
and asks Pages to build the site (a push made with a workflow's `GITHUB_TOKEN`
starts no Pages build). Once Pages serves the new files, it runs the install
commands above against the live repository.

[scripts/apt-repo.sh](../scripts/apt-repo.sh) does each of these steps, and runs
on a Linux machine with `reprepro`, `gpg` and `gpgv`, `dpkg-deb`, `jq`, `curl`,
`python3`, `gh` (to list and fetch the releases) and Docker (`just setup` installs
`reprepro` and `jq`). A pull request that changes the repository's files runs
apt.yml's `repo-test` job, the same steps with a throwaway key generated in the
job: the checks of the key against throwaway keys (`self-test`, where a subkey
with 30 days left fails the expiry check), then the newest release's `.deb`s
built into a repository, signed with the throwaway subkey and installed from it
in the two containers. Locally, from the top of the repository:

```sh
scripts/apt-repo.sh self-test
scripts/apt-repo.sh test-key /tmp/apt-key 365
tag=$(gh release view --json tagName --jq .tagName)
scripts/apt-repo.sh fetch "$tag" /tmp/apt-debs
export GNUPGHOME=$(mktemp -d) APT_KEYRING=/tmp/apt-key/duscope.gpg
APT_SIGNING_SUBKEY=$(cat /tmp/apt-key/subkey.asc) scripts/apt-repo.sh import
APT_SIGNING_PASSPHRASE=$(cat /tmp/apt-key/passphrase) \
  scripts/apt-repo.sh build /tmp/apt-site/apt /tmp/apt-debs /tmp/apt-debs
scripts/apt-repo.sh install-test debian:12 /tmp/apt-site
```
