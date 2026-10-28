#!/bin/sh
# Writes debian/changelog from CHANGELOG.md, at package build time (P1-PKG-01):
# one entry per released version, newest first, each section's lines as the
# entry's items. A pre-release X.Y.Z-rc.1 is written X.Y.Z~rc.1, which dpkg
# sorts before X.Y.Z, as `just deb` names the package. `just deb` and
# release.yml run it before cargo-deb.
#
#   debian/changelog.sh [CHANGELOG.md] [debian/changelog]
#
# PYTHON names the Python 3 to run (default python3). The maintainer line comes from [package.metadata.deb] in duscope/Cargo.toml.
set -eu
here=$(cd "$(dirname "$0")/.." && pwd)
in=${1:-$here/CHANGELOG.md}
out=${2:-$here/debian/changelog}
maintainer=$(sed -n 's/^maintainer = "\(.*\)"$/\1/p' "$here/duscope/Cargo.toml" | head -n 1)
[ -n "$maintainer" ] || { echo "changelog.sh: no maintainer in duscope/Cargo.toml" >&2; exit 1; }
exec "${PYTHON:-python3}" - "$in" "$out" "$maintainer" <<'PY'
import datetime, re, sys, textwrap

source, target, maintainer = sys.argv[1:4]
versions = []  # (version, date, [(section, text)])
current = None  # the items of the released version being read
for line in open(source, encoding="utf-8"):
    line = line.rstrip("\n")
    # A released version's heading, which may carry a note (`[YANKED]`).
    head = re.match(r"^## \[(\d+\.\d+\.\d+[^\]]*)\] - (\d{4}-\d{2}-\d{2})(\s.*)?$", line)
    if head:
        versions.append((head.group(1), head.group(2), []))
        current, section = versions[-1][2], None
        continue
    if line.startswith("## "):
        current = None  # an unreleased section: its lines are not packaged
        continue
    if current is None:
        continue
    items = current
    if line.startswith("### "):
        section = line[4:].strip()
    elif section is None:
        continue  # a version's notes before its sections stay in CHANGELOG.md
    elif line.startswith("- "):
        items.append([section, line[2:].strip()])
    elif line.startswith("  ") and items and line.strip():
        items[-1][1] += " " + line.strip()
if not versions:
    sys.exit(f"changelog.sh: no released version in {source}")
with open(target, "w", encoding="utf-8") as f:
    for version, date, items in versions:
        f.write(f"duscope ({version.replace('-', '~', 1)}-1) unstable; urgency=medium\n\n")
        if not items:
            items = [[None, f"Release {version}; see CHANGELOG.md."]]
        for section, text in items:
            text = f"{section}: {text}" if section else text
            text = re.sub(r"`([^`]*)`", r"\1", text)
            # A URL longer than the line stays whole (lintian allows a long
            # line that is one word).
            lines = textwrap.wrap(text, 76, break_on_hyphens=False, break_long_words=False)
            f.write(f"  * {lines[0]}\n")
            for more in lines[1:]:
                f.write(f"    {more}\n")
        when = datetime.datetime.strptime(date, "%Y-%m-%d").replace(tzinfo=datetime.timezone.utc)
        f.write(f"\n -- {maintainer}  {when.strftime('%a, %d %b %Y %H:%M:%S %z')}\n\n")
PY
