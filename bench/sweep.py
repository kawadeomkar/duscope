"""The thread sweep (P1-BENCH-01): duscope alone at each thread count, on each
tree and mode, and the fixed rule that turns the curves into a platform's
default thread count.

    bench/run.sh sweep --duscope target/release/duscope [--trees all]
                       [--mode both] [--threads 1,2,4,...] [--runs 5]

The result file (`bench/results/thread-sweep-<platform>-<arch>.json` unless
`--out` says otherwise) holds the usual header (host, harness, trees), one row
per tree, mode and thread count, and `default`: the rule's verdict. A run on
scaled trees (`--scale` below 1) is a smoke run: its file is named
`…-x<scale>.json` and its `default` is null. Files from
the bench runners are committed; the providers' `default_threads` cite them.

The rule, per platform: among the warm T1 rows, the smallest thread count whose
p50 is within 5% of the best p50, rounded to the nearest of 4, 8, 16 and 32
(ties to the smaller).

Standard library only; runs on Python 3.9 and later.
"""

from __future__ import annotations

import os
import shlex
import shutil
import tempfile
import time
from typing import Dict, List, Optional, Sequence

import measure
import tools
import trees

# The thread counts swept by default: every count up to 8, then the steps
# between the rounding targets, up to 64.
THREADS = (1, 2, 3, 4, 5, 6, 7, 8, 10, 12, 16, 20, 24, 32, 48, 64)
# A thread count within this share of the best p50 is as good as the best.
WITHIN = 0.05
# The counts a default is rounded to.
ROUNDED = (4, 8, 16, 32)


def default_threads(rows: Sequence[Dict[str, object]]) -> Optional[int]:
    """The rule's default for these sweep rows; None without warm T1 rows."""
    warm = sorted(
        (int(r["threads"]), float(r["p50_s"]))  # type: ignore[arg-type]
        for r in rows
        if r["tree"] == "T1" and r["mode"] == "warm"
    )
    if not warm:
        return None
    best = min(p50 for _, p50 in warm)
    smallest = next(n for n, p50 in warm if p50 <= best * (1 + WITHIN))
    return min(ROUNDED, key=lambda target: (abs(target - smallest), target))


def _threads(value: str) -> List[int]:
    counts = []
    for part in value.split(","):
        part = part.strip()
        if not part.isdigit() or int(part) < 1:
            raise ValueError(f"--threads takes a comma-separated list of counts of at least 1, not {value!r}")
        counts.append(int(part))
    return sorted(set(counts))


def cmd_sweep(args, harness) -> int:
    """`bench/run.sh sweep`: `harness` is the harness module, for its helpers."""
    if args.runs < 1 or args.warmup < 0:
        raise harness.UsageError("--runs must be at least 1 and --warmup at least 0")
    try:
        counts = _threads(args.threads)
    except ValueError as e:
        raise harness.UsageError(str(e)) from None
    key = tools.platform_key()
    plat, arch = key.split("-")
    modes = ["warm", "cold"] if args.mode == "both" else [args.mode]
    tree_list = harness._trees(args)
    scaled = any(float(t.scale) != 1.0 for t in tree_list)
    hyperfine = shlex.split(args.hyperfine)
    if "cold" in modes and not args.cold_cmd.strip():
        raise harness.UsageError("--cold-cmd is empty: a cold run needs the command that drops the caches before every run")
    started = time.gmtime()
    hyperfine_version = measure.hyperfine_version(hyperfine)
    if "cold" in modes:
        measure.check_cold(args.cold_cmd)
    duscope = tools.resolve(tools.PINS["duscope"], key, args.tools_dir, False, args.duscope, harness.log)
    if duscope.status != "ready" or duscope.path is None:
        raise measure.MeasureError(f"duscope at {args.duscope} is unavailable: {duscope.reason}")
    stamps = [trees.build(t, args.trees_dir, args.jobs, log=harness.log) for t in tree_list]

    rows: List[Dict[str, object]] = []
    raw_dir = tempfile.mkdtemp(prefix="duscope-sweep-")
    try:
        for t in tree_list:
            path = trees.tree_root(t, args.trees_dir)
            for mode in modes:
                session = measure.Session(
                    hyperfine, t.name, path, mode, args.runs, args.warmup, args.cold_cmd, raw_dir, harness.log
                )
                for n in counts:
                    command = (
                        f"{shlex.quote(duscope.path)} scan --json --treemap never --threads {n} {{tree}}"
                    )
                    tool = tools.ad_hoc(f"duscope-t{n}", command)
                    measure.smoke(tool, path)
                    row = measure.row(t.name, mode, tool.tool.name, tool.command(), session.alone(tool))
                    row["threads"] = n
                    rows.append(row)
    finally:
        shutil.rmtree(raw_dir, ignore_errors=True)
    finished = time.gmtime()

    result = {
        "schema_version": harness.SCHEMA_VERSION,
        "date": time.strftime("%Y-%m-%d", started),
        "started_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", started),
        "finished_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", finished),
        "platform": plat,
        "subject": "duscope",
        "host": harness.host(),
        "harness": {
            "commit": harness.git_commit(),
            "hyperfine": hyperfine_version,
            "runs": args.runs,
            "warmup": args.warmup,
            "cold_command": args.cold_cmd if "cold" in modes else None,
        },
        "duscope": {"path": duscope.path, "version": duscope.version},
        "trees": [
            {
                "name": t.name,
                "scale": float(t.scale),
                "dirs": t.dirs,
                "files": t.files,
                "digest": stamp["digest"],
                "filesystem": harness.filesystem(trees.tree_root(t, args.trees_dir)),
            }
            for t, stamp in zip(tree_list, stamps)
        ],
        "threads": counts,
        "rule": {
            "tree": "T1",
            "mode": "warm",
            "within": WITHIN,
            "rounded_to": list(ROUNDED),
        },
        # A scaled tree is a smoke run: it sets no default, and its file is not
        # the one the providers' defaults are held to.
        "default": None if scaled else default_threads(rows),
        "results": rows,
    }
    name = f"thread-sweep-{plat}-{arch}" + (f"-x{float(args.scale):g}" if scaled else "")
    out = args.out or os.path.join(args.results_dir, f"{name}.json")
    harness.write_result(result, out)
    harness.log(f"default threads by the rule: {result['default']}")
    print(out)
    return 0
