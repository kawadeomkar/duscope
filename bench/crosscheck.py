"""The off-Proxmox cross-check of the Linux gate figures (DEV-157).

    bench/run.sh crosscheck CURRENT BASELINE... [--last 1] [--tolerance 0.15]

Every Linux gate figure so far is bench-linux's, a VM on a Proxmox host whose
memory is suspect. `linux-crosscheck.yml` runs the harness on a GitHub-hosted
runner instead; this reads that result file (CURRENT) and holds each of its
ratios, duscope's time over a pinned competitor's in the same rounds
(`subject_ratio_p50`), to bench-linux's for the same tree, mode and competitor
in its latest nightly result among BASELINE (result files, or directories of
them such as a checkout of the `bench-results` branch). Absolute times are not
compared: a hosted runner is another machine (DEV-20).

A ratio lies within the noise when it differs from bench-linux's by at most
`--tolerance` either way: the 15% `bench-regression` allows a ratio on one
machine (DEV-59), where a ratio decides when absolute times moved (DEV-76).
DEV-59 also saw a ratio differ by as much between two CPU models of the same
runners, so a ratio outside it is a finding to read, with both machines named,
not a verdict on its own. Each line also says whether the hosted runner's ratio
meets the goal-1 target that pair is held to.

`--last N` takes, for each ratio, the median of the newest N nightly results
from the latest one's machine instead of the latest alone.

Exit status: 0 when every ratio lies within the noise, 1 when one does not, 2
when nothing can be compared. Inside GitHub Actions (GITHUB_ACTIONS=true) a
ratio outside the noise is also a warning annotation.

Standard library only; runs on Python 3.9 and later.
"""

from __future__ import annotations

import argparse
import os
import statistics
import sys
from typing import Dict, List, Tuple

import acceptance
import compare


class InputError(Exception):
    """Nothing that can be compared."""


def is_nightly(data: dict, subject: str) -> bool:
    """A harness result from Linux with `subject` as its subject, not an index,
    redraw or acceptance file."""
    return data.get("kind") is None and data.get("platform") == "linux" and data.get("subject") == subject


def pick(candidates: List[dict], last: int) -> List[dict]:
    """The newest candidate and, up to `last` in all, the newest ones before it
    from the same machine."""
    if not candidates:
        return []
    ordered = sorted(candidates, key=lambda d: d["started_at"], reverse=True)
    newest = ordered[0]
    return [d for d in ordered if compare.machine(d) == compare.machine(newest)][:last]


def targets(mode: str, competitor: str) -> List[acceptance.Target]:
    return [
        t for t in acceptance.TARGETS
        if t.platform == "linux" and t.mode == mode and competitor in t.against
    ]


def describe(label: str, path: str, data: dict) -> str:
    host = data.get("host") or {}
    commit = ((data.get("harness") or {}).get("commit") or "unknown")[:12]
    return (
        f"{label}: {os.path.basename(path)}, {data.get('started_at')}, {host.get('cpu')}, "
        f"{host.get('logical_cpus')} CPUs, commit {commit}"
    )


def lines(current: dict, chosen: List[dict], tolerance: float) -> Tuple[List[str], List[str], int]:
    """The comparison's lines, those outside the noise, and how many ratios
    were compared."""
    subject = current["subject"]
    out: List[str] = []
    outside: List[str] = []
    compared = 0
    for r in current["results"]:
        ratio = r.get("subject_ratio_p50")
        if r.get("paired_with") != subject or not isinstance(ratio, (int, float)):
            continue
        tree, mode, tool = r["tree"], r["mode"], r["tool"]
        digest = compare.digest(current, tree)
        theirs = [
            compare.ratios(b, tree, mode, subject)[tool]
            for b in chosen
            if compare.digest(b, tree) == digest and tool in compare.ratios(b, tree, mode, subject)
        ]
        what = f"{tree} {mode} {subject}/{tool}: {ratio:.3f} here"
        held = [
            f"{'meets' if ratio <= t.limit else 'over'} the target {t.limit:g}" for t in targets(mode, tool)
        ]
        if not theirs:
            out.append(f"{what}, no bench-linux ratio on the same tree to compare")
            continue
        base = statistics.median(theirs)
        change = ratio / base - 1
        compared += 1
        if abs(change) <= tolerance:
            verdict = f"within {tolerance:.0%}"
        else:
            verdict = f"OUTSIDE {tolerance:.0%}, {'higher' if change > 0 else 'lower'} here"
        line = f"{what}, {base:.3f} on bench-linux ({change:+.1%}): {verdict}" + (
            f"; {', '.join(held)} here" if held else ""
        )
        out.append(line)
        if abs(change) > tolerance:
            outside.append(line)
    return out, outside, compared


def main(argv: List[str], out=None) -> int:
    parser = argparse.ArgumentParser(
        prog="bench/run.sh crosscheck",
        description="Hold a hosted runner's ratios to bench-linux's latest nightly ones (DEV-157).",
    )
    parser.add_argument("current", metavar="CURRENT", help="the hosted runner's result file")
    parser.add_argument("baselines", metavar="BASELINE", nargs="+", help="bench-linux's result files, or directories of them")
    parser.add_argument("--last", type=int, default=1, help="nightly results to take the median of (default 1: the latest)")
    parser.add_argument("--tolerance", type=float, default=0.15, help="the difference either way within the noise (default 0.15)")
    args = parser.parse_args(argv)
    if args.last < 1:
        parser.error("--last must be at least 1")
    if args.tolerance < 0:
        parser.error("--tolerance may not be negative")

    def say(line: str) -> None:
        print(acceptance.ascii(line), file=out or sys.stdout)

    try:
        current = compare.load(args.current)
        if current.get("platform") != "linux" or current.get("kind") is not None:
            raise InputError(f"{args.current}: not a Linux harness result")
        here = os.path.realpath(args.current)
        found: List[Tuple[str, dict]] = []
        for path in compare.result_files(args.baselines):
            if os.path.realpath(path) == here:
                continue
            try:
                data = compare.load(path)
            except compare.InputError as e:
                print(f"crosscheck: left out {e}", file=sys.stderr)
                continue
            if is_nightly(data, current["subject"]):
                found.append((path, data))
        chosen = pick([d for _, d in found], args.last)
        if not chosen:
            raise InputError("no bench-linux nightly result among the baselines")
        names: Dict[int, str] = {id(d): p for p, d in found}
        say(describe("here", args.current, current))
        for data in chosen:
            say(describe("bench-linux", names[id(data)], data))
        commits = {(d.get("harness") or {}).get("commit") for d in chosen}
        if (current.get("harness") or {}).get("commit") not in commits:
            say("note: the two measured different commits")
        body, outside, compared = lines(current, chosen, args.tolerance)
        if compared == 0:
            raise InputError("no ratio of the same tree, mode and competitor in both: compare full-scale trees")
    except (InputError, compare.InputError) as e:
        print(f"crosscheck: {e}", file=sys.stderr)
        return 2
    for line in body:
        say(line)
    say(f"{compared - len(outside)} of {compared} ratios within {args.tolerance:.0%} of bench-linux's, {len(outside)} outside")
    if os.environ.get("GITHUB_ACTIONS") == "true":
        for line in outside:
            say(f"::warning title=linux-crosscheck::{line}")
    return 1 if outside else 0
