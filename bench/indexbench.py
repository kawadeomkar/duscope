"""The phase-2 gate measurement (P2-BENCH-01): what a saved scan costs and how
fast it answers, on T2 unless `--trees` says otherwise.

    bench/run.sh index --duscope target/release/duscope [--trees T2]
                       [--mode both] [--competitors dua,fdu] [--runs 20]

- **The write:** `scan --save -v` prints the write's time and microseconds per
  entry. Warm runs give the cost per entry; cold runs (the caches dropped
  before each) also give the write's share of the scan, its time over the
  walk's and the roll-up's.
- **The queries:** `top`, `stale`, `explain` (of the largest file) and
  `by-type`, over the whole scan and over its largest directory, each against
  the saved scan (`--scan <id>`): p50 and p95 end to end, the process's start
  included, warm.
- **The first query after a cache drop:** `top --scan`, cold.
- **The index's size:** bytes per entry of a basic index and of an
  `--attrs full` one.
- **D34:** `top --scan` in interleaved pairs with `dua aggregate --import`
  (from a snapshot `dua i --once --export` wrote, under a pseudo-terminal,
  which dua's interactive mode needs) and with `fdu --stale-ok` (after
  `fdu --cache=on` kept a scan): each competitor's speed-up is the median of
  its time over duscope's in the same round.

The result file (`<date>-<platform>-index.json` in `--results-dir`) holds the
usual header, the runs, and `gates`: each line of plan/SPEC.md's Gate 2 -> 3
the numbers decide, with its limit and whether it holds. The command exits 0
when everything was measured, whether or not the gates hold, and 1 when
something could not be.

Standard library only; runs on Python 3.9 and later.
"""

from __future__ import annotations

import glob
import json
import os
import re
import shlex
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from typing import Dict, List, Optional, Sequence, Tuple

import measure
import tools
import trees

# The competitors of the D34 comparison.
COMPETITORS = ("dua", "fdu")

# Gate 2 -> 3, as plan/SPEC.md states it: (what is measured, how it compares, the limit).
GATES: Tuple[Tuple[str, str, float], ...] = (
    ("write_us_per_entry_warm_p50", "<=", 2.5),
    ("write_share_of_cold_scan_p50", "<=", 0.25),
    ("top_p50_ms", "<=", 20.0),
    ("top_p95_ms", "<=", 50.0),
    ("stale_p50_ms", "<=", 20.0),
    ("stale_p95_ms", "<=", 50.0),
    ("explain_p50_ms", "<=", 20.0),
    ("explain_p95_ms", "<=", 50.0),
    ("by_type_p50_ms", "<=", 50.0),
    ("by_type_subtree_p50_ms", "<=", 50.0),
    ("first_query_cold_p50_ms", "<=", 250.0),
    ("bytes_per_entry_basic", "<=", 110.0),
    ("bytes_per_entry_full", "<=", 140.0),
    ("dua_speedup_p50", ">=", 20.0),
    ("fdu_speedup_p50", ">=", 20.0),
)

# What `scan -v` prints (duscope/src/verbs/scan.rs), after the logger's prefix.
WRITE_LINE = re.compile(r"write: ([0-9,]+) entries in ([0-9.]+) s: ([0-9.]+) µs/entry")
WALK_LINE = re.compile(r"walk: .*? in ([0-9.]+) s")
ROLLUP_LINE = re.compile(r"roll-up: ([0-9.]+) s")


def parse_verbose(stderr: str) -> Dict[str, float]:
    """The walk's, roll-up's and write's times and the write's cost per entry,
    from `scan --save -v`'s stderr."""
    write = WRITE_LINE.search(stderr)
    walk = WALK_LINE.search(stderr)
    rollup = ROLLUP_LINE.search(stderr)
    if not (write and walk and rollup):
        raise measure.MeasureError(f"`scan --save -v` printed no walk, roll-up and write lines: {stderr.strip()[-300:]}")
    return {
        "entries": int(write.group(1).replace(",", "")),
        "write_s": float(write.group(2)),
        "us_per_entry": float(write.group(3)),
        "walk_s": float(walk.group(1)),
        "rollup_s": float(rollup.group(1)),
    }


def gates(values: Dict[str, Optional[float]]) -> List[Dict[str, object]]:
    """Each gate line with the value measured (None when it was not) and whether it holds."""
    out = []
    for name, op, limit in GATES:
        value = values.get(name)
        holds = None if value is None else (value <= limit if op == "<=" else value >= limit)
        out.append({"name": name, "value": value, "op": op, "limit": limit, "holds": holds})
    return out


def under_pty(argv: Sequence[str]) -> List[str]:
    """`argv` under `script`, which gives it a pseudo-terminal: util-linux's or BSD's."""
    if sys.platform == "darwin":
        return ["script", "-q", "/dev/null", *argv]
    return ["script", "-qec", shlex.join(argv), "/dev/null"]


def _run(argv: Sequence[str], what: str, timeout: int = 1800) -> subprocess.CompletedProcess:
    done = subprocess.run(
        list(argv), stdin=subprocess.DEVNULL, capture_output=True, text=True, errors="replace", timeout=timeout
    )
    if done.returncode != 0:
        tail = " / ".join((done.stderr or done.stdout).strip().splitlines()[-3:])
        raise measure.MeasureError(f"{what}: `{shlex.join(argv)}` exited {done.returncode}: {tail}")
    return done


def _ms(seconds: float) -> float:
    return round(seconds * 1000, 3)


class Duscope:
    """The duscope executable, its saved scans in `index_dir`."""

    def __init__(self, path: str, index_dir: str):
        self.path = path
        self.index_dir = index_dir

    def argv(self, *args: str) -> List[str]:
        return [self.path, *args, "--index-dir", self.index_dir]

    def save(self, tree: str, *extra: str) -> str:
        """Saves a scan of `tree`; its id."""
        done = _run(self.argv("scan", tree, "--save", "--json", *extra), "saving a scan")
        return json.loads(done.stdout)["scan"]["id"]

    def saved(self) -> Dict[str, Dict[str, object]]:
        """The saved scans by id, as `scans --json` lists them."""
        done = _run(self.argv("scans", "--json"), "listing the saved scans")
        return {e["id"]: e for e in json.loads(done.stdout)["entries"]}

    def first_path(self, tree: str, scan: str, *extra: str) -> str:
        """The path of `top`'s first row; the tree's when it has none (a flat
        tree has no directory below its root)."""
        done = _run(self.argv("top", tree, "--scan", scan, "--limit", "1", "--json", *extra), "reading top")
        entries = json.loads(done.stdout)["entries"]
        return entries[0]["path"] if entries else tree


def _write_runs(duscope: str, tree: str, work: str, runs: int, warmup: int, cold_cmd: Optional[str], log) -> List[Dict[str, float]]:
    """`runs` saves of `tree` with `-v`, after `warmup` unrecorded ones; the caches
    dropped before each when `cold_cmd` is given. Each index is removed once read."""
    index_dir = os.path.join(work, "write")
    os.makedirs(index_dir, mode=0o700, exist_ok=True)
    out = []
    for r in range(warmup + runs):
        if cold_cmd and subprocess.run(cold_cmd, shell=True, capture_output=True).returncode != 0:
            raise measure.MeasureError(f"`{cold_cmd}` failed before a cold write")
        done = _run(
            [duscope, "scan", tree, "--save", "-v", "--plain", "--treemap", "never", "--index-dir", index_dir],
            "timing the write",
        )
        for file in glob.glob(os.path.join(index_dir, "scans", "*.db")):
            os.remove(file)
        if r < warmup:
            continue
        got = parse_verbose(done.stderr)
        out.append(got)
        mode = "cold" if cold_cmd else "warm"
        log(f"write, {mode}, run {r - warmup + 1}/{runs}: {got['us_per_entry']:.2f} µs/entry, {got['write_s']:.3f} s")
    return out


def _summary(values: Sequence[float]) -> Dict[str, float]:
    return {
        "p50": round(measure.percentile(values, 0.50), 6),
        "p95": round(measure.percentile(values, 0.95), 6),
        "min": round(min(values), 6),
        "max": round(max(values), 6),
    }


def cmd_index(args, harness) -> int:
    """`bench/run.sh index`: `harness` is the harness module, for its helpers."""
    if args.runs < 1 or args.warmup < 0:
        raise harness.UsageError("--runs must be at least 1 and --warmup at least 0")
    key = tools.platform_key()
    plat, arch = key.split("-")
    modes = ["warm", "cold"] if args.mode == "both" else [args.mode]
    tree_list = harness._trees(args)
    scaled = any(float(t.scale) != 1.0 for t in tree_list)
    hyperfine = shlex.split(args.hyperfine)
    if "cold" in modes and not args.cold_cmd.strip():
        raise harness.UsageError("--cold-cmd is empty: a cold run needs the command that drops the caches before every run")
    names = harness._names(args.competitors, COMPETITORS, "competitor", allow_none=True)
    overrides = harness._pairs(args.tool_path, "--tool-path")
    started = time.gmtime()
    hyperfine_version = measure.hyperfine_version(hyperfine)
    if "cold" in modes:
        measure.check_cold(args.cold_cmd)
    duscope = tools.resolve(tools.PINS["duscope"], key, args.tools_dir, False, args.duscope, harness.log)
    if duscope.status != "ready" or duscope.path is None:
        raise measure.MeasureError(f"duscope at {args.duscope} is unavailable: {duscope.reason}")
    competitors = [tools.resolve(tools.PINS[n], key, args.tools_dir, True, overrides.get(n), harness.log) for n in names]
    stamps = [trees.build(t, args.trees_dir, args.jobs, log=harness.log) for t in tree_list]
    cold_cmd = args.cold_cmd if "cold" in modes else None

    rows: List[Dict[str, object]] = []
    per_tree: List[Dict[str, object]] = []
    failures: List[str] = []
    raw_dir = tempfile.mkdtemp(prefix="duscope-index-")
    try:
        for t in tree_list:
            path = trees.tree_root(t, args.trees_dir)
            # The saved scans live beside the trees, on their filesystem, so a
            # cold run reads them cold as it reads the trees.
            work = tempfile.mkdtemp(prefix=".duscope-index-", dir=args.trees_dir)
            try:
                found = _measure_tree(args, harness, t, path, work, duscope.path, competitors, hyperfine, raw_dir, modes, cold_cmd, rows, failures)
            finally:
                shutil.rmtree(work, ignore_errors=True)
            per_tree.append(found)
    finally:
        shutil.rmtree(raw_dir, ignore_errors=True)
    finished = time.gmtime()

    first = per_tree[0] if per_tree else {}
    result = {
        "schema_version": harness.SCHEMA_VERSION,
        "kind": "index",
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
            "cold_command": cold_cmd,
        },
        "duscope": {"path": duscope.path, "version": duscope.version},
        "tools": [
            {"name": c.tool.name, "pinned": c.tool.pinned, "status": c.status, "version": c.version, "reason": c.reason}
            for c in competitors
        ],
        "trees": [
            {
                "name": t.name,
                "scale": float(t.scale),
                "dirs": t.dirs,
                "files": t.files,
                "digest": stamp["digest"],
                "filesystem": harness.filesystem(trees.tree_root(t, args.trees_dir)),
                **found,
            }
            for t, stamp, found in zip(tree_list, stamps, per_tree)
        ],
        # The gates read the first tree's numbers: T2's, as the gate names it.
        "gates": gates(first.get("gate_values", {})),  # type: ignore[arg-type]
        "failures": failures,
        "results": rows,
    }
    for tree in result["trees"]:
        tree.pop("gate_values", None)
    name = f"{result['date']}-{plat}-index" + (f"-x{float(args.scale):g}" if scaled else "")
    out = args.out or os.path.join(args.results_dir, f"{name}.json")
    harness.write_result(result, out)
    for gate in result["gates"]:
        value = gate["value"]
        shown = "not measured" if value is None else f"{value:g}"
        verdict = {True: "holds", False: "FAILS", None: "-"}[gate["holds"]]
        harness.log(f"gate {gate['name']} {gate['op']} {gate['limit']:g}: {shown} ({verdict})")
    for failure in failures:
        harness.log(f"not measured: {failure}")
    print(out)
    return 1 if failures else 0


def _measure_tree(args, harness, t, path, work, duscope_path, competitors, hyperfine, raw_dir, modes, cold_cmd, rows, failures) -> Dict[str, object]:
    log = harness.log
    values: Dict[str, Optional[float]] = {}
    ref = Duscope(duscope_path, os.path.join(work, "ref"))
    os.makedirs(ref.index_dir, mode=0o700)
    basic = ref.save(path)
    full = ref.save(path, "--attrs", "full")
    saved = ref.saved()
    sizes = {}
    for label, scan in (("basic", basic), ("full", full)):
        entry = saved[scan]
        per_entry = round(int(entry["index_bytes"]) / int(entry["entry_count"]), 3)
        sizes[label] = {"bytes": entry["index_bytes"], "entries": entry["entry_count"], "bytes_per_entry": per_entry}
        values[f"bytes_per_entry_{label}"] = per_entry
        log(f"{t.name}: a {label} index is {per_entry:.1f} bytes per entry")

    # The write.
    write: Dict[str, object] = {}
    for mode in modes:
        runs = _write_runs(duscope_path, path, work, args.runs, args.warmup if mode == "warm" else 0,
                           cold_cmd if mode == "cold" else None, log)
        summary = {"runs": runs, "us_per_entry": _summary([r["us_per_entry"] for r in runs])}
        values[f"write_us_per_entry_{mode}_p50"] = summary["us_per_entry"]["p50"]
        if mode == "cold":
            # `-v` prints milliseconds: a scan of a smoke tree may take none.
            shares = [r["write_s"] / max(r["walk_s"] + r["rollup_s"], 0.001) for r in runs]
            summary["share_of_scan"] = _summary(shares)
            values["write_share_of_cold_scan_p50"] = summary["share_of_scan"]["p50"]
        write[mode] = summary

    # The queries, against the basic scan.
    q = lambda *a: shlex.join(ref.argv(*a))  # noqa: E731
    largest_file = ref.first_path(path, basic, "--files")
    largest_dir = ref.first_path(path, basic)
    queries = {
        "top": q("top", path, "--scan", basic, "--json"),
        "stale": q("stale", path, "--scan", basic, "--json"),
        "explain": q("explain", largest_file, "--scan", basic, "--json"),
        "by_type": q("by-type", path, "--scan", basic, "--json"),
        "by_type_subtree": q("by-type", largest_dir, "--scan", basic, "--json"),
    }
    warm = measure.Session(hyperfine, t.name, path, "warm", args.runs, args.warmup, None, raw_dir, log)
    top_tool = None
    for name, command in queries.items():
        tool = tools.ad_hoc(name, command)
        measure.smoke(tool, path)
        row = measure.row(t.name, "warm", name, tool.command(), warm.alone(tool))
        row["kind"] = "query"
        rows.append(row)
        values[f"{name}_p50_ms"] = _ms(float(row["p50_s"]))
        values[f"{name}_p95_ms"] = _ms(float(row["p95_s"]))
        if name == "top":
            top_tool = tool
    assert top_tool is not None
    if "cold" in modes:
        cold = measure.Session(hyperfine, t.name, path, "cold", args.runs, 0, cold_cmd, raw_dir, log)
        row = measure.row(t.name, "cold", "top", top_tool.command(), cold.alone(top_tool))
        row["kind"] = "first_query"
        rows.append(row)
        values["first_query_cold_p50_ms"] = _ms(float(row["p50_s"]))

    # D34: each competitor's answer from its own snapshot, paired with top --scan.
    speedups: Dict[str, Optional[float]] = {}
    for competitor in competitors:
        name = competitor.tool.name
        if competitor.status != "ready" or competitor.path is None:
            failures.append(f"{name}: {competitor.reason}")
            speedups[name] = None
            continue
        try:
            if name == "dua":
                snapshot = os.path.join(work, "dua.snapshot")
                _run(under_pty([competitor.path, "i", "--once", "--export", snapshot, path]), "writing dua's snapshot")
                if not os.path.isfile(snapshot):
                    raise measure.MeasureError("`dua i --once --export` wrote no snapshot")
                command = shlex.join([competitor.path, "aggregate", "--import", snapshot])
            else:
                _run([competitor.path, path, "--cache=on"], "keeping fdu's scan")
                command = shlex.join([competitor.path, path, "--stale-ok"])
            other = tools.ad_hoc(name, command)
            measure.smoke(other, path)
            subject_runs, other_runs = warm.pair(top_tool, other)
        except measure.MeasureError as e:
            failures.append(f"{name}: {e}")
            speedups[name] = None
            continue
        rows.append(dict(measure.row(t.name, "warm", "top", top_tool.command(), subject_runs, paired_with=name), kind="d34"))
        rows.append(dict(measure.row(t.name, "warm", name, command, other_runs, subject_runs=subject_runs), kind="d34"))
        speedup = round(statistics.median(o.wall / s.wall for s, o in zip(subject_runs, other_runs)), 3)
        speedups[name] = speedup
        values[f"{name}_speedup_p50"] = speedup
        log(f"{t.name}: top --scan is {speedup:.1f}x faster than {name}")

    return {
        "scans": {"basic": basic, "full": full},
        "index_size": sizes,
        "write": write,
        "query_targets": {"largest_file": largest_file, "largest_dir": largest_dir},
        "speedups": speedups,
        "gate_values": values,
    }
