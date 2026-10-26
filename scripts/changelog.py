#!/usr/bin/env python3
"""Check duscope's changelog fragments, and assemble them into CHANGELOG.md at release.

Each pull request with a user-visible change adds a fragment, changelog.d/<ID>.md,
made of keep-a-changelog sections; changelog.d/README.md gives the format. This
script is the one parser of that format.

  scripts/changelog.py check FRAGMENT...
      Validate fragments with the rule of the `changelog` CI job. Prints
      "<path>: parses" for each good fragment and one error per problem, and
      exits 1 when any fragment fails.

  scripts/changelog.py assemble VERSION [--date YYYY-MM-DD] [--root DIR]
      Assemble every fragment in DIR/changelog.d/ (README.md excepted) into a
      "## [VERSION] - DATE" section of DIR/CHANGELOG.md, then delete the
      fragments. Same-named sections of different fragments are merged, in the
      keep-a-changelog order and by fragment file name within a section. An
      undated "## [VERSION]" or "## [VERSION] - Unreleased" section already in
      CHANGELOG.md is filled in, and its notes stay above the assembled
      sections; otherwise the new section goes above the newest one. Fails and
      changes nothing when there is no fragment, when a fragment does not
      parse, or when VERSION already has a dated section. VERSION may carry the
      tag's "v"; DATE defaults to today in UTC; DIR defaults to the current
      directory.

Inside GitHub Actions (GITHUB_ACTIONS=true) errors are workflow annotations on
stdout, exactly as the `changelog` job prints them; elsewhere they are
"path:line: message" lines on stderr.

Standard library only; runs on Python 3.9 and later.
"""

from __future__ import annotations

import argparse
import datetime
import os
import re
import sys
import tempfile
from pathlib import Path

SECTIONS = ("Added", "Changed", "Deprecated", "Removed", "Fixed", "Security")
FRAGMENT_DIR = "changelog.d"
FRAGMENT_README = "README.md"
CHANGELOG = "CHANGELOG.md"

DATE = re.compile(r"\d{4}-\d{2}-\d{2}")
VERSION = re.compile(r"\d+\.\d+\.\d+")
VERSION_HEADING = re.compile(r"## \[(?P<version>[^\]]+)\](?:\s+-\s+(?P<date>\S.*?))?\s*")
SUBSECTION_HEADING = re.compile(r"#{3,6}(\s|$)")
LINK_DEFINITION = re.compile(r"\[[^\]]+\]:\s")


def in_github_actions() -> bool:
    return os.environ.get("GITHUB_ACTIONS") == "true"


class ChangelogError(Exception):
    """A reason to stop with nothing changed."""


class Fragment:
    """One parsed fragment: its errors, and its items by section.

    An item is the list of its lines: the "- " line, then its continuation
    lines, with one empty line where blank lines separated them.
    """

    def __init__(self, path: str) -> None:
        self.path = path
        self.errors: list[tuple[int, str]] = []
        self.items: dict[str, list[list[str]]] = {}

    def error(self, line: int, message: str) -> None:
        self.errors.append((line, message))


def parse_fragment(path: str) -> Fragment:
    """Parse a fragment with the rule of the `changelog` CI job.

    The checks, their order and their messages are those of the program the job
    runs (a test compares the two on every fixture), so that the job can call
    `check` instead of carrying its own copy of the rule.
    """
    fragment = Fragment(path)
    try:
        with open(path, encoding="utf-8") as f:
            text = f.read()
    except (OSError, UnicodeDecodeError) as e:
        fragment.error(1, f"cannot read the fragment as UTF-8: {e}")
        return fragment

    section = None
    counts: dict[str, int] = {}  # items per section, as the CI rule counts them
    item: list[str] = []  # the lines of the last item
    blank = False  # a blank line since the last non-blank one
    for n, line in enumerate(text.splitlines(), 1):
        if not line.strip():
            blank = True
            continue
        heading = re.fullmatch(r"### (.+?)\s*", line)
        if heading:
            section = heading.group(1)
            if section not in SECTIONS:
                fragment.error(
                    n, f"unknown section '### {section}': use one of {', '.join(SECTIONS)}"
                )
            elif section in counts:
                fragment.error(n, f"section '### {section}' appears twice")
            counts[section] = 0
            fragment.items.setdefault(section, [])
        elif line.startswith("#"):
            fragment.error(
                n,
                "only '### <Section>' headings belong in a fragment; "
                "the release's `scripts/changelog.py assemble` writes the version heading",
            )
        elif section is None:
            fragment.error(n, "text before the first '### <Section>' heading")
        elif re.match(r"- \S", line):
            counts[section] += 1
            item = [line]
            fragment.items[section].append(item)
        elif re.match(r"\s+\S", line) and counts[section]:
            # A continuation of the item above; a blank line inside the item stays.
            if blank:
                item.append("")
            item.append(line)
        else:
            fragment.error(n, "expected a '- ' list item or an indented continuation line")
        blank = False

    if not counts:
        fragment.error(1, f"no '### <Section>' heading: use one of {', '.join(SECTIONS)}")
    for name, count in counts.items():
        if count == 0:
            fragment.error(1, f"section '### {name}' has no '- ' items")
    return fragment


def report(fragment: Fragment) -> None:
    for line, message in fragment.errors:
        if in_github_actions():
            print(f"::error file={fragment.path},line={line}::{message}")
        else:
            print(f"{fragment.path}:{line}: {message}", file=sys.stderr)


def check(paths: list[str]) -> int:
    failed = False
    for path in paths:
        fragment = parse_fragment(path)
        report(fragment)
        if fragment.errors:
            failed = True
        else:
            print(f"{path}: parses")
    return 1 if failed else 0


def strip_blank_lines(lines: list[str]) -> list[str]:
    start, end = 0, len(lines)
    while start < end and not lines[start].strip():
        start += 1
    while end > start and not lines[end - 1].strip():
        end -= 1
    return lines[start:end]


def render_section(version: str, date: str, notes: list[str], fragments: list[Fragment]) -> list[str]:
    """The lines of the version's section, ending with a blank line."""
    lines = [f"## [{version}] - {date}", ""]
    if notes:
        lines += notes + [""]
    for name in SECTIONS:
        items = [item for fragment in fragments for item in fragment.items.get(name, [])]
        if items:
            lines += [f"### {name}", ""]
            for item in items:
                lines += item
            lines.append("")
    return lines


def insert_section(text: str, version: str, date: str, fragments: list[Fragment]) -> str:
    """CHANGELOG.md's text with the version's section assembled into it."""
    lines = text.splitlines()
    headings = []  # (index, version, date or None, pending)
    for i, line in enumerate(lines):
        m = VERSION_HEADING.fullmatch(line)
        if m:
            when = m.group("date")
            pending = when is None or when.casefold() == "unreleased"
            if not pending and not DATE.match(when):
                raise ChangelogError(
                    f"{CHANGELOG}:{i + 1}: cannot read the heading {line!r}: "
                    "expected '## [X.Y.Z] - YYYY-MM-DD' or '## [X.Y.Z] - Unreleased'"
                )
            headings.append((i, m.group("version"), when, pending))

    notes: list[str] = []
    same = [h for h in headings if h[1] == version]
    if same:
        start, _, when, pending = same[0]
        if not pending:
            raise ChangelogError(f"{CHANGELOG} already has a section for {version}, dated {when}")
        # The undated section runs to the next "## " heading or link definition.
        end = start + 1
        while end < len(lines) and not (
            lines[end].startswith("## ") or LINK_DEFINITION.match(lines[end])
        ):
            end += 1
        notes = strip_blank_lines(lines[start + 1 : end])
        for line in notes:
            if SUBSECTION_HEADING.match(line):
                raise ChangelogError(
                    f"{CHANGELOG}: the undated section for {version} has the heading {line!r}; "
                    "changes go into changelog.d/ fragments, and only notes stay in the section"
                )
        before, after = lines[:start], lines[end:]
    else:
        undated = [h for h in headings if h[3]]
        if undated:
            raise ChangelogError(
                f"{CHANGELOG} has the undated section [{undated[0][1]}]: "
                f"assemble {undated[0][1]} first, or remove the section"
            )
        if headings:
            at = headings[0][0]
        else:
            # Above any link definitions at the end of the file.
            at = len(lines)
            while at > 0 and (not lines[at - 1].strip() or LINK_DEFINITION.match(lines[at - 1])):
                at -= 1
        before, after = lines[:at], lines[at:]

    before = strip_blank_lines(before)
    out = before + ([""] if before else [])
    out += render_section(version, date, notes, fragments)
    out += strip_blank_lines(after)
    return "\n".join(strip_blank_lines(out)) + "\n"


def write_atomically(path: Path, text: str) -> None:
    mode = path.stat().st_mode & 0o777
    fd, temporary = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    try:
        with os.fdopen(fd, "w", encoding="utf-8", newline="\n") as f:
            f.write(text)
        os.chmod(temporary, mode)
        os.replace(temporary, path)
    except BaseException:
        os.unlink(temporary)
        raise


def assemble(version: str, date: str, root: Path) -> int:
    if version.startswith("v"):
        version = version[1:]
    if not VERSION.fullmatch(version):
        raise ChangelogError(f"the version must be X.Y.Z or vX.Y.Z, not {version!r}")

    fragment_dir = root / FRAGMENT_DIR
    if not fragment_dir.is_dir():
        raise ChangelogError(f"no fragments: {fragment_dir} is not a directory")
    paths = sorted(
        p for p in fragment_dir.iterdir() if p.suffix == ".md" and p.name != FRAGMENT_README and p.is_file()
    )
    if not paths:
        raise ChangelogError(
            f"no fragments in {fragment_dir}: a release needs at least one {FRAGMENT_DIR}/<ID>.md"
        )

    fragments = [parse_fragment(str(p)) for p in paths]
    broken = [f for f in fragments if f.errors]
    for fragment in broken:
        report(fragment)
    if broken:
        raise ChangelogError(f"{len(broken)} fragment(s) do not parse; nothing was changed")

    changelog = root / CHANGELOG
    try:
        text = changelog.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError) as e:
        raise ChangelogError(f"cannot read {changelog}: {e}") from e

    write_atomically(changelog, insert_section(text, version, date, fragments))
    for p in paths:
        p.unlink()
    print(f"changelog: assembled {len(paths)} fragment(s) into {changelog} as [{version}] - {date}")
    for p in paths:
        print(f"changelog: deleted {p}")
    return 0


def release_date(text: str) -> str:
    try:
        if DATE.fullmatch(text):
            datetime.date.fromisoformat(text)
            return text
    except ValueError:
        pass
    raise argparse.ArgumentTypeError(f"not a YYYY-MM-DD date: {text!r}")


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(
        prog="changelog.py",
        description="Check duscope's changelog fragments, and assemble them into CHANGELOG.md.",
    )
    commands = parser.add_subparsers(dest="command", required=True, metavar="COMMAND")
    check_parser = commands.add_parser(
        "check", help="validate fragments with the rule of the changelog CI job"
    )
    check_parser.add_argument("fragments", nargs="+", metavar="FRAGMENT")
    assemble_parser = commands.add_parser(
        "assemble", help="assemble changelog.d/ into a dated CHANGELOG.md section and delete the fragments"
    )
    assemble_parser.add_argument("version", metavar="VERSION", help="X.Y.Z, or the tag vX.Y.Z")
    assemble_parser.add_argument(
        "--date",
        type=release_date,
        default=datetime.datetime.now(datetime.timezone.utc).date().isoformat(),
        help="the release date, YYYY-MM-DD (default: today in UTC)",
    )
    assemble_parser.add_argument(
        "--root",
        type=Path,
        default=Path("."),
        help="the directory holding CHANGELOG.md and changelog.d/ (default: the current directory)",
    )
    args = parser.parse_args(argv)

    if args.command == "check":
        return check(args.fragments)
    try:
        return assemble(args.version, args.date, args.root)
    except ChangelogError as e:
        if in_github_actions():
            print(f"::error title=changelog::{e}")
        else:
            print(f"changelog.py: {e}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
