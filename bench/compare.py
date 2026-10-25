#!/usr/bin/env python3
"""Compare a benchmark result with the results before it: the regression check
that ci.yml's `bench-regression` job runs from phase 1 (plan/SPEC.md, "CI/CD").

    bench/compare.py CURRENT [BASELINE...] [--tree T1] [--tool NAME]
                     [--last 5] [--tolerance 0.15]

CURRENT is a result file (bench/README.md, "Result file"). Each BASELINE is a
result file or a directory of them: a checkout of the `bench-results` branch,
or the job's downloaded artifacts. A baseline counts when it ran on the same
machine kind as CURRENT (`platform` and `host.arch`), on the same tree (equal
`trees[].digest`), and measured the tool in the same mode. The newest `--last`
of those by `started_at` make the baseline, and CURRENT regresses in a mode
when its p50 exceeds their median by more than `--tolerance`. CURRENT itself is
never its own baseline, so a directory may hold it.

The tool defaults to CURRENT's subject, and every mode CURRENT measured it in is
checked. One line per mode goes to stdout; inside GitHub Actions
(GITHUB_ACTIONS=true) a regression is also an error annotation and a mode with
no baseline a notice. A baseline file that cannot be read is reported on stderr
and left out.

Exit status: 0 when no mode regressed (including when there is no baseline
yet), 1 when one did, 2 when CURRENT cannot be compared.

Standard library only; runs on Python 3.9 and later.
"""

from __future__ import annotations

import argparse
import json
import os
import statistics
import sys
from typing import List, NamedTuple, Optional, Tuple

SCHEMA_VERSION = 1


class InputError(Exception):
    """A result file that cannot be compared."""


class Check(NamedTuple):
    mode: str
    p50: float
    median: Optional[float]
    count: int

    @property
    def change(self) -> Optional[float]:
        return None if self.median is None else self.p50 / self.median - 1


def load(path: str) -> dict:
    try:
        with open(path, encoding="utf-8") as f:
            data = json.load(f)
    except (OSError, ValueError) as e:
        raise InputError(f"{path}: cannot read a result file: {e}") from e
    if not isinstance(data, dict) or data.get("schema_version") != SCHEMA_VERSION:
        raise InputError(f"{path}: not a result file with schema_version {SCHEMA_VERSION}")
    try:
        machine(data)
        data["trees"], data["results"], data["started_at"]
    except (KeyError, TypeError) as e:
        raise InputError(f"{path}: a result file without {e}") from e
    return data


def result_files(paths: List[str]) -> List[str]:
    files = []
    for path in paths:
        if os.path.isdir(path):
            files += sorted(os.path.join(path, n) for n in os.listdir(path) if n.endswith(".json"))
        else:
            files.append(path)
    return files


def machine(data: dict) -> Tuple[str, str]:
    return data["platform"], data["host"]["arch"]


def digest(data: dict, tree: str) -> Optional[str]:
    return next((t.get("digest") for t in data["trees"] if t.get("name") == tree), None)


def p50(data: dict, tree: str, mode: str, tool: str) -> Optional[float]:
    for r in data["results"]:
        if r.get("tree") == tree and r.get("mode") == mode and r.get("tool") == tool:
            return r.get("p50_s")
    return None


def compare(current: dict, baselines: List[dict], tree: str, tool: str, last: int) -> List[Check]:
    modes = sorted({r.get("mode") for r in current["results"] if r.get("tree") == tree and r.get("tool") == tool})
    if not modes:
        raise InputError(f"no {tool} result on {tree} to compare")
    same = [b for b in baselines if machine(b) == machine(current) and digest(b, tree) == digest(current, tree)]
    checks = []
    for mode in modes:
        now = p50(current, tree, mode, tool)
        prior = sorted((b for b in same if p50(b, tree, mode, tool)), key=lambda b: b["started_at"])[-last:]
        values = [p50(b, tree, mode, tool) for b in prior]
        if not isinstance(now, (int, float)) or now <= 0:
            raise InputError(f"the {tool} p50 on {tree} {mode} is not a positive time: {now!r}")
        checks.append(Check(mode, now, statistics.median(values) if values else None, len(values)))
    return checks


def in_github_actions() -> bool:
    return os.environ.get("GITHUB_ACTIONS") == "true"


def main(argv: List[str]) -> int:
    parser = argparse.ArgumentParser(
        prog="compare.py", description="Compare a benchmark result file with the results before it."
    )
    parser.add_argument("current", metavar="CURRENT", help="the result file to check")
    parser.add_argument("baselines", metavar="BASELINE", nargs="*", help="earlier result files, or directories of them")
    parser.add_argument("--tree", default="T1", help="the tree to compare on (default T1)")
    parser.add_argument("--tool", help="the tool to compare (default: CURRENT's subject)")
    parser.add_argument("--last", type=int, default=5, help="how many of the newest baselines to take (default 5)")
    parser.add_argument(
        "--tolerance", type=float, default=0.15, help="the slowdown allowed over the baseline median (default 0.15)"
    )
    args = parser.parse_args(argv)
    if args.last < 1:
        parser.error("--last must be at least 1")
    if args.tolerance < 0:
        parser.error("--tolerance may not be negative")

    try:
        current = load(args.current)
        tool = args.tool or current.get("subject")
        if not tool:
            raise InputError(f"{args.current}: no subject; pass --tool")
        baselines = []
        here = os.path.realpath(args.current)
        for path in result_files(args.baselines):
            if os.path.realpath(path) == here:
                continue
            try:
                baselines.append(load(path))
            except InputError as e:
                print(f"compare.py: left out {e}", file=sys.stderr)
        try:
            checks = compare(current, baselines, args.tree, tool, args.last)
        except InputError as e:
            raise InputError(f"{args.current}: {e}") from e
    except InputError as e:
        print(f"compare.py: {e}", file=sys.stderr)
        return 2

    regressed = False
    for c in checks:
        what = f"{args.tree} {c.mode} {tool}"
        if c.median is None:
            line = f"{what}: no baseline (no earlier result on this machine kind and tree); p50 {c.p50:.3f} s"
            print(line)
            if in_github_actions():
                print(f"::notice title=bench-regression::{line}")
            continue
        change = c.change or 0.0
        verdict = "ok"
        if change > args.tolerance:
            verdict = f"regression beyond {args.tolerance:.0%}"
            regressed = True
        line = (
            f"{what}: p50 {c.p50:.3f} s against a median of {c.median:.3f} s over {c.count} results "
            f"({change:+.1%}): {verdict}"
        )
        print(line)
        if verdict != "ok" and in_github_actions():
            print(f"::error title=bench-regression::{line}")
    return 1 if regressed else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
