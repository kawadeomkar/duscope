#!/usr/bin/env bash
# The signed apt repository (P5-PKG-01), served at APT_URL below from the
# gh-pages branch. .github/workflows/apt.yml runs these commands after each
# release, and its pull-request job runs them with a throwaway key; on a Linux
# machine they run the same way. docs/apt.md describes the repository, the key
# and its rotation.
#
#   releases                  the GitHub Releases, one JSON object a line (gh)
#   select TAG                from `releases` on stdin, the release each suite
#                             holds, as stable=<tag> and testing=<tag>: stable
#                             the newest release, testing the newest release or
#                             pre-release, among those with both .debs; TAG, the
#                             release the run is for, must be one of those
#   fetch TAG DIR             TAG's two .debs into DIR, each checked against the
#                             attestation release.yml made for it (gh)
#   check-key [KEYRING] [DOC] the public key (apt/duscope.gpg) exists, and
#                             docs/apt.md records its fingerprint and its signing
#                             subkey's expiry
#   import                    $APT_SIGNING_SUBKEY into $GNUPGHOME: one signing
#                             subkey of the key in $APT_KEYRING, without the
#                             primary key's secret
#   check-expiry [DAYS]       the signing subkey in $GNUPGHOME expires more than
#                             DAYS (60) days from now
#   build OUT STABLE TESTING  the repository in OUT, a new directory, from the
#                             .debs in the directories STABLE and TESTING, signed
#                             with that subkey ($APT_SIGNING_PASSPHRASE) and
#                             verified against $APT_KEYRING, which it publishes
#   install-test IMAGE [SITE] docs/apt.md's install commands in a new IMAGE
#                             container, against the live repository, or against
#                             SITE/apt served on 127.0.0.1 ($APT_TEST_PORT, 8765)
#   wait-live OUT [SECONDS]   until APT_URL serves OUT's Release files (600 s)
#   test-key DIR DAYS         a throwaway key as the key ceremony leaves one:
#                             DIR/duscope.gpg, DIR/subkey.asc, DIR/passphrase,
#                             DIR/doc.md, and DIR/primary.asc, the secret it
#                             never exports; its subkey expires in DAYS days
#   self-test                 the key commands against throwaway keys: what
#                             they accept and what they refuse
#
# APT_KEYRING defaults to apt/duscope.gpg. Linux with reprepro, gpg, gpgv,
# dpkg-deb, jq, curl and python3; install-test runs $DOCKER (docker). `select`
# needs jq alone, and `check-key` fails without gpg only once the key exists.

set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
# Where the repository is served (plan/SPEC.md, P5-PKG-01).
APT_URL=https://kawadeomkar.github.io/duscope/apt
# The days a signing subkey must still have when a run signs with it.
EXPIRY_DAYS=60
# A release tag, as release.yml's verify-tag accepts it.
TAG_RE='^v([0-9]+)\.([0-9]+)\.([0-9]+)(-[0-9A-Za-z.-]+)?$'
APT_KEYRING=${APT_KEYRING:-$root/apt/duscope.gpg}
repo=${GITHUB_REPOSITORY:-kawadeomkar/duscope}

fail() {
  if [[ -n ${GITHUB_ACTIONS:-} ]]; then
    echo "::error title=apt-repo::$*" >&2
  else
    echo "apt-repo: $*" >&2
  fi
  exit 1
}

usage() {
  sed -n '/^#   [a-z]/,/^#$/s/^# \{0,1\}//p' "${BASH_SOURCE[0]}" >&2
  exit 2
}

# A date from seconds since the epoch, with GNU's date or BSD's.
day() { date -u -d "@$1" +%F 2>/dev/null || date -u -r "$1" +%F; }

# Temporary directories, under one the script removes when it exits, with any
# gpg-agent started in them and the install test's server.
scratch=$(mktemp -d)
server=""
cleanup() {
  local home
  [[ -z $server ]] || kill "$server" 2>/dev/null || true
  while IFS= read -r home; do
    gpgconf --homedir "$home" --kill all 2>/dev/null || true
  done < <(find "$scratch" -type d -name gnupg 2>/dev/null)
  rm -rf "$scratch"
}
trap cleanup EXIT
tempdir() { mktemp -d "$scratch/XXXXXX"; }

# gpg reading a key file without importing it, in a home of its own.
show_keys() {
  local home
  home=$(tempdir)/gnupg
  mkdir -m 700 "$home"
  gpg --homedir "$home" --batch --with-colons --show-keys "$1"
}

# The primary key's fingerprint of the one key in a key file.
primary_fingerprint() {
  local fingerprints
  fingerprints=$(show_keys "$1" | awk -F: '$1 == "pub" { want = 1; next } $1 == "fpr" && want { print $10; want = 0 }')
  [[ $(wc -l <<<"$fingerprints") -eq 1 && -n $fingerprints ]] ||
    fail "$1 holds $(grep -c . <<<"$fingerprints") keys, not one"
  printf '%s\n' "$fingerprints"
}

# "fingerprint expiry" for each signing subkey that is neither expired nor
# revoked, in a key file (show_keys) or in $GNUPGHOME's secret keys, where it
# must hold the secret (`+`, not a stub `#` or a card `>`).
signing_subkeys() {
  awk -F: -v secret="$1" '
    $1 == "sub" || $1 == "ssb" {
      want = $2 !~ /[erdi]/ && $12 ~ /s/ && (!secret || $15 == "+")
      expires = $7
      next
    }
    $1 == "fpr" && want { print $10, expires }
    { want = 0 }'
}

# The value of a `key: value` line in docs/apt.md.
doc_value() {
  sed -n "s/^$2: //p" "$1" | head -n 1
}

cmd_releases() {
  gh api --paginate "repos/$repo/releases" \
    --jq '.[] | {tag: .tag_name, prerelease, draft, assets: [.assets[].name]}'
}

cmd_select() {
  (( $# == 1 )) || usage
  local tag=$1 out
  [[ $tag =~ $TAG_RE ]] || fail "$tag is not a release tag (vX.Y.Z or vX.Y.Z-pre)"
  # Versions in semver order: a pre-release before its release, and its
  # dot-separated parts compared as numbers where they are numbers.
  out=$(jq -rs --arg tag "$tag" '
    def version:
      .tag | capture("^v(?<major>[0-9]+)\\.(?<minor>[0-9]+)\\.(?<patch>[0-9]+)(-(?<pre>[0-9A-Za-z.-]+))?$");
    def order:
      version as $v
      | [($v.major | tonumber), ($v.minor | tonumber), ($v.patch | tonumber),
         (if $v.pre == null then 1 else 0 end),
         (($v.pre // "") | split(".") | map(tonumber? // .))];
    def has_deb($arch): any(.assets[]; test("^duscope_[^/]+_\($arch)\\.deb$"));
    def complete: has_deb("amd64") and has_deb("arm64");
    def released: (.prerelease | not) and version.pre == null;
    [.[] | select(.draft | not) | select(.tag | test("^v[0-9]+\\.[0-9]+\\.[0-9]+(-[0-9A-Za-z.-]+)?$"))] as $all
    | ($all | map(select(.tag == $tag))) as $this
    | if ($this | length) == 0 then error("\($tag) is not a published release")
      elif ($this[0] | complete | not) then error("\($tag) has no .deb for amd64 and arm64 among its assets")
      else . end
    | ($all | map(select(complete))) as $ok
    | ($ok | map(select(released)) | max_by(order)) as $stable
    | ($ok | max_by(order)) as $testing
    | if $stable == null then error("no release has both .debs and no pre-release suffix: stable would be empty") else . end
    | "stable=\($stable.tag)", "testing=\($testing.tag)"
  ' 2>&1) || fail "${out#jq: error (at <stdin>:*): }"
  local stable testing
  stable=$(sed -n 's/^stable=//p' <<<"$out")
  testing=$(sed -n 's/^testing=//p' <<<"$out")
  if [[ $tag != "$stable" && $tag != "$testing" ]]; then
    echo "apt-repo: $tag goes in neither suite: stable holds $stable and testing $testing, newer releases" >&2
  fi
  printf '%s\n' "$out"
}

cmd_fetch() {
  (( $# == 2 )) || usage
  local tag=$1 dir=$2 deb
  [[ $tag =~ $TAG_RE ]] || fail "$tag is not a release tag"
  mkdir -p "$dir"
  gh release download "$tag" --repo "$repo" --dir "$dir" --pattern 'duscope_*_amd64.deb' --pattern 'duscope_*_arm64.deb'
  # Signed only when release.yml built it, at this tag: an asset uploaded by
  # hand has no attestation.
  for deb in "$dir"/*.deb; do
    gh attestation verify "$deb" --repo "$repo" --signer-workflow "$repo/.github/workflows/release.yml" \
      --source-ref "refs/tags/$tag" > /dev/null ||
      fail "$(basename "$deb") of $tag has no attestation from release.yml at that tag"
    echo "$(basename "$deb"): built and attested by release.yml at $tag"
  done
}

cmd_check_key() {
  (( $# <= 2 )) || usage
  local keyring=${1:-$root/apt/duscope.gpg} doc=${2:-$root/docs/apt.md}
  local recorded actual subkeys newest expires
  [[ -f $keyring ]] ||
    fail "${keyring#"$root"/} is missing: the public key exists once the key ceremony (P5-SEC-02) has made it, and nothing is signed or published before then (docs/apt.md)"
  recorded=$(doc_value "$doc" fingerprint | tr -d ' ' | tr '[:lower:]' '[:upper:]')
  [[ $recorded =~ ^[0-9A-F]{40}$ ]] ||
    fail "${doc#"$root"/} records no fingerprint (\"$(doc_value "$doc" fingerprint)\"): the key ceremony (P5-SEC-02) writes the primary key's there"
  actual=$(primary_fingerprint "$keyring")
  [[ $actual == "$recorded" ]] ||
    fail "${keyring#"$root"/} is the key $actual, but ${doc#"$root"/} records $recorded"
  subkeys=$(show_keys "$keyring" | signing_subkeys "")
  [[ -n $subkeys ]] || fail "${keyring#"$root"/} has no signing subkey that is neither expired nor revoked"
  newest=$(sort -k2,2n <<<"$subkeys" | tail -n 1)
  expires=${newest#* }
  [[ -n $expires ]] || fail "the signing subkey ${newest% *} has no expiry: the key ceremony gives it twelve months (P5-SEC-02)"
  [[ $(doc_value "$doc" "signing subkey expires") == "$(day "$expires")" ]] ||
    fail "${doc#"$root"/} records the signing subkey's expiry as \"$(doc_value "$doc" "signing subkey expires")\", but ${newest% *} expires on $(day "$expires")"
  echo "${keyring#"$root"/}: the key $actual, as ${doc#"$root"/} records it; its signing subkey ${newest% *} expires on $(day "$expires")"
}

# The one usable secret signing subkey in $GNUPGHOME: "fingerprint expiry".
secret_subkey() {
  local subkeys
  subkeys=$(gpg --batch --with-colons --list-secret-keys | signing_subkeys 1)
  [[ -n $subkeys ]] || fail "the keyring holds no secret signing subkey that is neither expired nor revoked"
  [[ $(wc -l <<<"$subkeys") -eq 1 ]] ||
    fail "the keyring holds $(wc -l <<<"$subkeys") secret signing subkeys; APT_SIGNING_SUBKEY holds one"
  printf '%s\n' "$subkeys"
}

cmd_import() {
  (( $# == 0 )) || usage
  : "${GNUPGHOME:?GNUPGHOME names the keyring to import into}"
  [[ -n ${APT_SIGNING_SUBKEY:-} ]] ||
    fail "APT_SIGNING_SUBKEY is empty: the secret is set at the key ceremony (P5-SEC-02, docs/apt.md)"
  [[ -f $APT_KEYRING ]] || cmd_check_key "$APT_KEYRING"
  mkdir -p "$GNUPGHOME"
  chmod 700 "$GNUPGHOME"
  gpg --batch --quiet --import <<<"$APT_SIGNING_SUBKEY" 2>/dev/null ||
    fail "APT_SIGNING_SUBKEY is not an OpenPGP secret key"
  local primaries primary subkey
  primaries=$(gpg --batch --with-colons --list-secret-keys | awk -F: '$1 == "sec" { print $15 }')
  [[ $(wc -l <<<"$primaries") -eq 1 ]] || fail "APT_SIGNING_SUBKEY holds more than one key"
  # The primary key never leaves the offline machine and the token (DEV-140).
  [[ $primaries == "#" ]] ||
    fail "APT_SIGNING_SUBKEY holds the primary key's secret, which never leaves the offline machine (DEV-140): export the signing subkey alone, with gpg --export-secret-subkeys <its fingerprint>!"
  primary=$(gpg --batch --with-colons --list-secret-keys | awk -F: '$1 == "fpr" { print $10; exit }')
  [[ $primary == "$(primary_fingerprint "$APT_KEYRING")" ]] ||
    fail "APT_SIGNING_SUBKEY is a subkey of $primary, not of the published key in ${APT_KEYRING#"$root"/}"
  subkey=$(secret_subkey)
  echo "imported the signing subkey ${subkey% *} of $primary"
}

cmd_check_expiry() {
  (( $# <= 1 )) || usage
  local days=${1:-$EXPIRY_DAYS} subkey fingerprint expires now left
  [[ $days =~ ^[0-9]+$ ]] || usage
  subkey=$(secret_subkey)
  fingerprint=${subkey% *} expires=${subkey#* }
  [[ -n $expires ]] ||
    fail "the signing subkey $fingerprint has no expiry: the key ceremony gives it twelve months (P5-SEC-02), and this check reads it"
  now=$(date +%s)
  left=$(( (expires - now) / 86400 ))
  (( expires - now > days * 86400 )) ||
    fail "the signing subkey $fingerprint expires on $(day "$expires"), in $left days, within $days: nothing is signed with it; rotate it as docs/apt.md says"
  echo "the signing subkey $fingerprint expires on $(day "$expires"), in $left days"
}

# The .debs of one suite: duscope for amd64 and arm64, one version.
suite_debs() {
  local dir=$1 deb package arch version versions=() arches=()
  shopt -s nullglob
  local debs=("$dir"/*.deb)
  shopt -u nullglob
  (( ${#debs[@]} == 2 )) || fail "$dir holds ${#debs[@]} .deb files, not one for each of amd64 and arm64"
  for deb in "${debs[@]}"; do
    package=$(dpkg-deb -f "$deb" Package) arch=$(dpkg-deb -f "$deb" Architecture) version=$(dpkg-deb -f "$deb" Version)
    [[ $package == duscope ]] || fail "$deb is the package $package, not duscope"
    arches+=("$arch") versions+=("$version")
  done
  [[ $(printf '%s\n' "${arches[@]}" | sort | paste -sd ' ' -) == "amd64 arm64" ]] ||
    fail "$dir holds .debs for ${arches[*]}, not amd64 and arm64"
  [[ ${versions[0]} == "${versions[1]}" ]] || fail "$dir holds two versions, ${versions[*]}"
  printf '%s\n' "${debs[@]}"
}

# InRelease and Release.gpg beside a Release file, with the subkey in $GNUPGHOME.
sign() {
  local release=$1 dir subkey
  dir=$(dirname "$release")
  subkey=$(secret_subkey)
  local gpg_sign=(gpg --batch --yes --pinentry-mode loopback --passphrase-fd 3
    --local-user "${subkey% *}!" --digest-algo SHA512)
  "${gpg_sign[@]}" --clearsign --output "$dir/InRelease" "$release" 3<<<"$APT_SIGNING_PASSPHRASE" ||
    fail "gpg could not sign $release (is APT_SIGNING_PASSPHRASE the subkey's passphrase?)"
  "${gpg_sign[@]}" --armor --detach-sign --output "$dir/Release.gpg" "$release" 3<<<"$APT_SIGNING_PASSPHRASE"
}

# gpgv's status lines for a signature, as apt reads them: a good, valid
# signature by a key in the keyring, and nothing else.
gpgv_check() {
  local keyring=$1 home status bad
  shift
  home=$(tempdir)
  status=$(gpgv --homedir "$home" --status-fd 1 --keyring "$keyring" "$@" 2>/dev/null) ||
    fail "gpgv refuses $1 with ${keyring#"$root"/}"
  if ! grep -q '^\[GNUPG:\] GOODSIG ' <<<"$status" || ! grep -q '^\[GNUPG:\] VALIDSIG ' <<<"$status"; then
    fail "$1 has no good, valid signature"
  fi
  bad=$(grep -E '^\[GNUPG:\] (BADSIG|ERRSIG|EXPSIG|EXPKEYSIG|REVKEYSIG|NO_PUBKEY) ' <<<"$status" || true)
  [[ -z $bad ]] || fail "$1: ${bad%%$'\n'*}"
}

# Both signatures of a Release file verify against the published key, and
# InRelease holds the Release file itself.
verify() {
  local release=$1 keyring=$2 dir home
  dir=$(dirname "$release")
  gpgv_check "$keyring" "$dir/InRelease"
  gpgv_check "$keyring" "$dir/Release.gpg" "$release"
  home=$(tempdir)
  gpgv --homedir "$home" --keyring "$keyring" --output - "$dir/InRelease" 2>/dev/null | cmp -s - "$release" ||
    fail "$dir/InRelease does not hold $release"
}

cmd_build() {
  (( $# == 3 )) || usage
  local out=$1 stable=$2 testing=$3 work keyring suite dir listed release
  local debs=()
  [[ ! -e $out ]] || fail "$out exists: build writes a new repository"
  [[ -n ${APT_SIGNING_PASSPHRASE:-} ]] ||
    fail "APT_SIGNING_PASSPHRASE is empty: the subkey's passphrase is set at the key ceremony (P5-SEC-02)"
  [[ -f $APT_KEYRING ]] || cmd_check_key "$APT_KEYRING"
  keyring=$(cd "$(dirname "$APT_KEYRING")" && pwd)/$(basename "$APT_KEYRING")
  secret_subkey > /dev/null
  work=$(tempdir)
  mkdir -p "$out"
  out=$(cd "$out" && pwd)
  for suite in stable testing; do
    dir=$stable
    [[ $suite == stable ]] || dir=$testing
    listed=$(suite_debs "$dir")
    mapfile -t debs <<<"$listed"
    reprepro --basedir "$work" --confdir "$root/apt/conf" --dbdir "$work/db" --outdir "$out" \
      includedeb "$suite" "${debs[@]}"
  done
  cp "$keyring" "$out/duscope.gpg"
  cp "$root/apt/duscope.sources" "$out/duscope.sources"
  for suite in stable testing; do
    release=$out/dists/$suite/Release
    [[ -f $release ]] || fail "reprepro wrote no $release"
    sign "$release"
    verify "$release" "$out/duscope.gpg"
    echo "$suite: signed and verified"
  done
  (cd "$out" && find . -type f | sort)
}

# docs/apt.md's install commands: the sh block between its two markers.
install_commands() {
  local commands
  commands=$(awk '/<!-- apt-install:begin -->/ { on = 1; next } /<!-- apt-install:end -->/ { on = 0 } on' "$1" | sed '/^```/d')
  [[ -n $commands ]] || fail "$1 has no install commands between <!-- apt-install:begin --> and <!-- apt-install:end -->"
  printf '%s\n' "$commands"
}

cmd_install_test() {
  (( $# == 1 || $# == 2 )) || usage
  local image=$1 site=${2:-} url=$APT_URL dir docker=() port tries=0 status=0
  read -r -a docker <<<"${DOCKER:-docker}"
  dir=$(tempdir)
  install_commands "$root/docs/apt.md" > "$dir/install.sh"
  if [[ -n $site ]]; then
    [[ -f $site/apt/dists/stable/InRelease ]] || fail "$site/apt holds no signed repository"
    port=${APT_TEST_PORT:-8765}
    url=http://127.0.0.1:$port/apt
    # The repository as built, but for the sources file's URI, served on this
    # machine; the commands are docs/apt.md's with its URL replaced.
    cp -R "$site" "$dir/site"
    chmod -R a+rX "$dir/site"
    sed "s|^URIs: $APT_URL\$|URIs: $url|" "$site/apt/duscope.sources" > "$dir/site/apt/duscope.sources"
    grep -qx "URIs: $url" "$dir/site/apt/duscope.sources" || fail "$site/apt/duscope.sources does not name $APT_URL"
    sed "s|$APT_URL|$url|g" "$dir/install.sh" > "$dir/install.local"
    mv "$dir/install.local" "$dir/install.sh"
    python3 -I -m http.server --bind 127.0.0.1 --directory "$dir/site" "$port" > "$dir/server.log" 2>&1 &
    server=$!
    until curl -fsS -o /dev/null "$url/dists/stable/InRelease" 2>/dev/null; do
      (( ++tries < 50 )) || fail "no server on 127.0.0.1:$port: $(cat "$dir/server.log")"
      sleep 0.2
    done
  fi
  cat > "$dir/check.sh" <<'EOF'
set -euo pipefail
url=$1
export DEBIAN_FRONTEND=noninteractive
# The commands are written for a user with sudo; the container runs as root.
printf '#!/bin/sh\nexec "$@"\n' > /usr/local/bin/sudo
chmod 755 /usr/local/bin/sudo
. /etc/os-release
echo "== $PRETTY_NAME: docs/apt.md's install commands"
if ! bash -ex /check/install.sh > /tmp/install.log 2>&1; then
  cat /tmp/install.log
  echo "the install commands failed in $PRETTY_NAME"
  exit 1
fi
cat /tmp/install.log
# No warning from apt-get update: not about the signature, nor anything else.
if grep -E '^(W|E): ' /tmp/install.log; then
  echo "apt warned in $PRETTY_NAME"
  exit 1
fi
apt-cache policy duscope | tee /tmp/policy
grep -qF " $url stable/main " /tmp/policy || { echo "duscope does not come from $url stable"; exit 1; }
duscope --version
duscope top /usr/share/doc --json > /tmp/top.json
grep -q '"ok":true' /tmp/top.json || { cat /tmp/top.json; exit 1; }
echo "== $PRETTY_NAME: the testing suite"
sed -i 's/^Suites: stable$/Suites: testing/' /etc/apt/sources.list.d/duscope.sources
apt-get update > /tmp/testing.log 2>&1 || { cat /tmp/testing.log; exit 1; }
cat /tmp/testing.log
if grep -E '^(W|E): ' /tmp/testing.log; then
  echo "apt warned on the testing suite in $PRETTY_NAME"
  exit 1
fi
apt-cache policy duscope | tee /tmp/policy
grep -qF " $url testing/main " /tmp/policy || { echo "testing does not offer duscope"; exit 1; }
echo "== $PRETTY_NAME: duscope $(dpkg-query -W -f='${Version}' duscope) installed from $url"
EOF
  timeout 900 "${docker[@]}" run --rm --network host -v "$dir:/check:ro" "$image" bash /check/check.sh "$url" || status=$?
  (( status == 0 )) || fail "installing from $url in $image failed (exit $status)"
}

cmd_wait_live() {
  (( $# == 1 || $# == 2 )) || usage
  local out=$1 seconds=${2:-600} suite deadline
  deadline=$(( $(date +%s) + seconds ))
  for suite in stable testing; do
    # GitHub Pages builds the branch, then its CDN keeps a copy up to 10 minutes.
    until curl -fsS --max-time 30 "$APT_URL/dists/$suite/InRelease" 2>/dev/null | cmp -s - "$out/dists/$suite/InRelease"; do
      (( $(date +%s) < deadline )) || fail "$APT_URL does not serve the new $suite/InRelease after $seconds s"
      sleep 15
    done
    echo "$APT_URL serves the new $suite/InRelease"
  done
}

cmd_test_key() {
  (( $# == 2 )) || usage
  local dir=$1 days=$2 home pass primary subkey expires
  [[ $days =~ ^[0-9]+$ ]] || usage
  mkdir -p "$dir"
  home=$(tempdir)/gnupg
  mkdir -m 700 "$home"
  pass=$(head -c 18 /dev/urandom | base64)
  local g=(gpg --homedir "$home" --batch --quiet --pinentry-mode loopback --passphrase "$pass")
  # As the ceremony makes it (P5-SEC-02): a primary key that certifies and
  # never expires, and a signing subkey with an expiry.
  "${g[@]}" --quick-gen-key "duscope apt test key, thrown away <apt-test@example.invalid>" ed25519 cert never 2>/dev/null
  primary=$("${g[@]}" --with-colons --list-keys | awk -F: '$1 == "fpr" { print $10; exit }')
  "${g[@]}" --quick-add-key "$primary" ed25519 sign "${days}d"
  read -r subkey expires < <("${g[@]}" --with-colons --list-keys "$primary" | signing_subkeys "")
  "${g[@]}" --export "$primary" > "$dir/duscope.gpg"
  "${g[@]}" --armor --export-secret-subkeys "$subkey!" > "$dir/subkey.asc"
  "${g[@]}" --armor --export-secret-keys "$primary" > "$dir/primary.asc"
  printf '%s\n' "$pass" > "$dir/passphrase"
  printf 'fingerprint: %s\nsigning subkey expires: %s\n' "$primary" "$(day "$expires")" > "$dir/doc.md"
  echo "a throwaway key $primary, its signing subkey $subkey expiring on $(day "$expires"), in $dir"
}

cmd_self_test() {
  (( $# == 0 )) || usage
  local keys good short
  keys=$(tempdir)
  good=$keys/good short=$keys/short
  cmd_test_key "$good" 365 > /dev/null
  cmd_test_key "$short" 30 > /dev/null
  printf 'fingerprint: PLACEHOLDER\nsigning subkey expires: PLACEHOLDER\n' > "$keys/placeholder.md"
  sed 's/^signing subkey expires: .*/signing subkey expires: 2000-01-01/' "$good/doc.md" > "$keys/stale.md"
  # expect NAME WANT KEYRING SECRET COMMAND...: this script's COMMAND in a new
  # keyring, with APT_KEYRING=KEYRING, after importing the file SECRET unless
  # it is empty. An empty WANT: it passes; otherwise it fails, saying WANT.
  expect() {
    local name=$1 want=$2 keyring=$3 secret=$4 home said=0
    shift 4
    home=$(tempdir)/gnupg
    if (
      export GNUPGHOME=$home APT_KEYRING=$keyring
      if [[ -n $secret ]]; then
        # errexit does not hold inside an `if` condition.
        APT_SIGNING_SUBKEY=$(cat "$secret") bash "$root/scripts/apt-repo.sh" import || exit
      fi
      bash "$root/scripts/apt-repo.sh" "$@"
    ) > "$keys/out" 2>&1; then
      [[ -z $want ]] || { cat "$keys/out" >&2; fail "self-test: $name passed"; }
      echo "self-test: $name: passes"
      return
    fi
    grep -qF -- "$want" "$keys/out" && said=1
    if [[ -z $want || $said == 0 ]]; then
      cat "$keys/out" >&2
      fail "self-test: $name failed${want:+, but not saying \"$want\"}"
    fi
    echo "self-test: $name: refused: $(grep -F -- "$want" "$keys/out" | head -n 1 | sed 's/^::error title=apt-repo:://; s/^apt-repo: //')"
  }
  expect "check-key, the key docs/apt.md records" "" "" "" \
    check-key "$good/duscope.gpg" "$good/doc.md"
  expect "check-key, no key yet" "the key ceremony (P5-SEC-02)" "" "" \
    check-key "$keys/missing.gpg" "$good/doc.md"
  expect "check-key, the placeholder" "records no fingerprint" "" "" \
    check-key "$good/duscope.gpg" "$keys/placeholder.md"
  expect "check-key, another key" "is the key" "" "" \
    check-key "$short/duscope.gpg" "$good/doc.md"
  expect "check-key, another expiry" "records the signing subkey's expiry" "" "" \
    check-key "$good/duscope.gpg" "$keys/stale.md"
  expect "check-expiry, 365 days left" "" "$good/duscope.gpg" "$good/subkey.asc" \
    check-expiry
  expect "check-expiry, 30 days left" "within $EXPIRY_DAYS" "$short/duscope.gpg" "$short/subkey.asc" \
    check-expiry
  expect "import, the primary key's secret" "the primary key's secret" "$good/duscope.gpg" "$good/primary.asc" \
    check-expiry
  expect "import, another key's subkey" "not of the published key" "$short/duscope.gpg" "$good/subkey.asc" \
    check-expiry
}

(( $# >= 1 )) || usage
command=$1
shift
case $command in
  releases) cmd_releases "$@" ;;
  select) cmd_select "$@" ;;
  fetch) cmd_fetch "$@" ;;
  check-key) cmd_check_key "$@" ;;
  import) cmd_import "$@" ;;
  check-expiry) cmd_check_expiry "$@" ;;
  build) cmd_build "$@" ;;
  install-test) cmd_install_test "$@" ;;
  wait-live) cmd_wait_live "$@" ;;
  test-key) cmd_test_key "$@" ;;
  self-test) cmd_self_test "$@" ;;
  *) usage ;;
esac
