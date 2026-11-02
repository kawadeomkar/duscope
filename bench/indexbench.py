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
- **The first query after a cache drop:** `top --scan`, cold, against a scan
  saved beside the cold trees.
- **The index's size:** bytes per entry of a basic index and of an
  `--attrs full` one.
- **D34:** `top --scan` in interleaved pairs with `dua interactive --import
  --once` (from a snapshot `dua i --once --export` wrote) and with `fdu
  --stale-ok` (after `fdu --cache=on` kept a scan): each competitor's speed-up
  is the median of its time over duscope's in the same round. dua's
  interactive mode wants a terminal, so both of its commands run under
  `script`; fdu keeps its scan under a home of its own in the run's work
  directory.

The result file (`<date>-<platform>-index.json` in `--results-dir`) holds the
usual header, the runs, and `gates`: each line of plan/PROPOSAL.md's 1.0
acceptance targets 3, 4, 6 and 7 that the numbers decide, read from T2 (the
first tree when T2 was not measured), with its limit and whether it holds. A
part that cannot be measured is recorded in `failures` and the rest is still
measured; the command then exits 1. A gate failing is no failure of the run.

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
from contextlib import contextmanager
from typing import Dict, Iterator, List, Optional, Sequence, Tuple

import measure
import tools
import trees

# The competitors of the D34 comparison.
COMPETITORS = ("dua", "fdu")

# plan/PROPOSAL.md's 1.0 acceptance targets 3, 4, 6 and 7, as Gate 2 -> 3
# reads them: (what is measured, how it compares, the limit, the platforms it
# binds, or None for both). The subtree by-type limit is per 100k files of the
# subtree (BY_TYPE_PER).
GATES: Tuple[Tuple[str, str, float, Optional[str]], ...] = (
    ("write_us_per_entry_warm_p50", "<=", 2.5, "linux"),
    ("write_share_of_cold_scan_p50", "<=", 0.25, None),
    ("top_p50_ms", "<=", 20.0, None),
    ("top_p95_ms", "<=", 50.0, None),
    ("stale_p50_ms", "<=", 20.0, None),
    ("stale_p95_ms", "<=", 50.0, None),
    ("explain_p50_ms", "<=", 20.0, None),
    ("explain_p95_ms", "<=", 50.0, None),
    ("by_type_p50_ms", "<=", 50.0, None),
    ("by_type_subtree_p50_ms", "<=", 50.0, None),
    ("first_query_cold_p50_ms", "<=", 250.0, None),
    ("bytes_per_entry_basic", "<=", 110.0, None),
    ("bytes_per_entry_full", "<=", 140.0, None),
    ("dua_speedup_p50", ">=", 20.0, None),
    ("fdu_speedup_p50", ">=", 20.0, None),
)

# The subtree by-type limit counts per this many files of the subtree.
BY_TYPE_PER = 100_000

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


def gates(values: Dict[str, Optional[float]], platform: str) -> List[Dict[str, object]]:
    """Each gate line with the value measured (None when it was not), its limit,
    and whether it holds: None when it was not measured or does not bind this
    platform. `by_type_subtree_files` scales the subtree by-type limit."""
    out = []
    for name, op, limit, binds in GATES:
        if name == "by_type_subtree_p50_ms":
            files = values.get("by_type_subtree_files") or 0
            limit = round(limit * max(1.0, files / BY_TYPE_PER), 3)
        value = values.get(name)
        applies = binds is None or binds == platform
        holds = None if value is None or not applies else (value <= limit if op == "<=" else value >= limit)
        out.append({"name": name, "value": value, "op": op, "limit": limit, "binds": binds, "holds": holds})
    return out


def under_pty(argv: Sequence[str]) -> List[str]:
    """`argv` under `script`, which gives it a pseudo-terminal: util-linux's or BSD's."""
    if sys.platform == "darwin":
        return ["script", "-q", "/dev/null", *argv]
    return ["script", "-qec", shlex.join(argv), "/dev/null"]


def _run(argv: Sequence[str], what: str, timeout: int = 1800) -> subprocess.CompletedProcess:
    try:
        done = subprocess.run(
            list(argv), stdin=subprocess.DEVNULL, capture_output=True, text=True, errors="replace", timeout=timeout
        )
    except subprocess.TimeoutExpired:
        raise measure.MeasureError(f"{what}: `{shlex.join(argv)}` ran past {timeout} s") from None
    if done.returncode != 0:
        tail = " / ".join((done.stderr or done.stdout).strip().splitlines()[-3:])
        raise measure.MeasureError(f"{what}: `{shlex.join(argv)}` exited {done.returncode}: {tail}")
    return done


@contextmanager
def _home(path: str) -> Iterator[None]:
    """HOME and XDG_CACHE_HOME at `path` for what runs inside, as for fdu,
    whose kept scan then lives in the run's work directory."""
    saved = {k: os.environ.get(k) for k in ("HOME", "XDG_CACHE_HOME")}
    os.makedirs(os.path.join(path, ".cache"), exist_ok=True)
    os.environ["HOME"] = path
    os.environ["XDG_CACHE_HOME"] = os.path.join(path, ".cache")
    try:
        yield
    finally:
        for k, v in saved.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v


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

    def first_row(self, tree: str, scan: str, *extra: str) -> Tuple[str, int]:
        """The path and file count of `top`'s first row; the tree's when it has
        none (a flat tree has no directory below its root)."""
        done = _run(self.argv("top", tree, "--scan", scan, "--limit", "1", "--json", *extra), "reading top")
        answer = json.loads(done.stdout)
        if answer["entries"]:
            first = answer["entries"][0]
            return first["path"], int(first["file_count"])
        return tree, int(answer["scan"]["file_count"])


def _write_runs(duscope: str, tree: str, index_dir: str, runs: int, warmup: int, cold_cmd: Optional[str], log) -> List[Dict[str, float]]:
    """`runs` saves of `tree` into `index_dir` with `-v`, after `warmup`
    unrecorded ones; the caches dropped before each when `cold_cmd` is given.
    Each index is removed once read."""
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
    others = [n for n in harness._pairs(args.tool_path, "--tool-path") if n not in COMPETITORS]
    if others:
        raise harness.UsageError(
            f"--tool-path names {', '.join(others)}: index measures {' and '.join(COMPETITORS)} beside the --duscope it is given"
        )
    started = time.gmtime()
    hyperfine_version = measure.hyperfine_version(hyperfine)
    if "cold" in modes:
        measure.check_cold(args.cold_cmd)
    duscope = tools.resolve(tools.PINS["duscope"], key, args.tools_dir, False, args.duscope, harness.log)
    if duscope.status != "ready" or duscope.path is None:
        raise measure.MeasureError(f"duscope at {args.duscope} is unavailable: {duscope.reason}")
    competitors = harness._resolve_tools(args, names, key)
    cold_cmd = args.cold_cmd if "cold" in modes else None
    # The cold runs can read trees of their own, as in `run` (bench/README.md,
    # "Cold runs"); the saved scans each mode reads live beside its trees.
    tree_dirs = {"warm": args.trees_dir, "cold": args.cold_trees_dir or args.trees_dir}
    dirs = list(dict.fromkeys(tree_dirs[m] for m in modes))
    built = {d: [trees.build(t, d, args.jobs, log=harness.log) for t in tree_list] for d in dirs}
    stamps = built[dirs[0]]

    rows: List[Dict[str, object]] = []
    per_tree: List[Dict[str, object]] = []
    failures: List[str] = []
    raw_dir = tempfile.mkdtemp(prefix="duscope-index-")
    work = {d: tempfile.mkdtemp(prefix=".duscope-index-", dir=d) for d in dirs}
    try:
        for t in tree_list:
            run = Run(args, harness, t, duscope.path, hyperfine, raw_dir, rows, failures)
            per_tree.append(run.measure(tree_dirs, work, modes, cold_cmd, competitors))
    finally:
        shutil.rmtree(raw_dir, ignore_errors=True)
        for d in work.values():
            shutil.rmtree(d, ignore_errors=True)
    finished = time.gmtime()

    # The gates read T2's numbers, as the targets name it.
    judged = next((i for i, t in enumerate(tree_list) if t.name == "T2"), 0)
    values = per_tree[judged].pop("gate_values") if per_tree else {}
    for found in per_tree:
        found.pop("gate_values", None)
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
                "filesystem": harness.filesystem(trees.tree_root(t, dirs[0])),
                "cold_filesystem": harness.filesystem(trees.tree_root(t, dirs[1])) if len(dirs) > 1 else None,
                **found,
            }
            for t, stamp, found in zip(tree_list, stamps, per_tree)
        ],
        "gates_tree": tree_list[judged].name if tree_list else None,
        "gates": gates(values, plat),
        "failures": failures,
        "results": rows,
    }
    name = f"{result['date']}-{plat}-index" + (f"-x{float(args.scale):g}" if scaled else "")
    out = args.out or os.path.join(args.results_dir, f"{name}.json")
    harness.write_result(result, out)
    for gate in result["gates"]:
        value = gate["value"]
        shown = "not measured" if value is None else f"{value:g}"
        verdict = {True: "holds", False: "FAILS", None: "-"}[gate["holds"]]
        if value is not None and gate["holds"] is None:
            verdict = f"binds {gate['binds']} only"
        harness.log(f"gate {gate['name']} {gate['op']} {gate['limit']:g}: {shown} ({verdict})")
    for failure in failures:
        harness.log(f"not measured: {failure}")
    print(out)
    return 1 if failures else 0


class Run:
    """The measurements of one tree. Each part that fails is recorded in
    `failures` and leaves its numbers out; the others are still taken."""

    def __init__(self, args, harness, t, duscope: str, hyperfine, raw_dir: str, rows, failures):
        self.args = args
        self.log = harness.log
        self.t = t
        self.duscope = duscope
        self.hyperfine = hyperfine
        self.raw_dir = raw_dir
        self.rows = rows
        self.failures = failures
        self.values: Dict[str, Optional[float]] = {}

    def part(self, what: str, measure_it) -> bool:
        """Runs `measure_it`; a failure is recorded as `what`'s."""
        try:
            measure_it()
            return True
        except (measure.MeasureError, OSError, KeyError, ValueError) as e:
            self.failures.append(f"{self.t.name} {what}: {e}")
            return False

    def measure(self, tree_dirs, work, modes, cold_cmd, competitors) -> Dict[str, object]:
        args, t = self.args, self.t
        path = trees.tree_root(t, tree_dirs["warm"])
        ref = Duscope(self.duscope, os.path.join(work[tree_dirs["warm"]], f"{t.name}-ref"))
        os.makedirs(ref.index_dir, mode=0o700)
        found: Dict[str, object] = {}
        scans: Dict[str, str] = {}

        def sizes() -> None:
            scans["basic"] = ref.save(path)
            scans["full"] = ref.save(path, "--attrs", "full")
            saved = ref.saved()
            found["index_size"] = {}
            for label, scan in scans.items():
                entry = saved[scan]
                per_entry = round(int(entry["index_bytes"]) / int(entry["entry_count"]), 3)
                found["index_size"][label] = {
                    "bytes": entry["index_bytes"], "entries": entry["entry_count"], "bytes_per_entry": per_entry,
                }
                self.values[f"bytes_per_entry_{label}"] = per_entry
                self.log(f"{t.name}: a {label} index is {per_entry:.1f} bytes per entry")

        if not self.part("index size", sizes):
            return {"gate_values": self.values, **found}
        found["scans"] = dict(scans)

        write: Dict[str, object] = {}
        for mode in modes:
            self.part(f"{mode} write", lambda mode=mode: write.update({mode: self.write(mode, tree_dirs, work, cold_cmd)}))
        found["write"] = write

        top: List[tools.Resolved] = []

        def queries() -> None:
            largest_file, _ = ref.first_row(path, scans["basic"], "--files")
            largest_dir, files = ref.first_row(path, scans["basic"])
            found["query_targets"] = {"largest_file": largest_file, "largest_dir": largest_dir, "largest_dir_files": files}
            self.values["by_type_subtree_files"] = float(files)
            q = lambda *a: shlex.join(ref.argv(*a))  # noqa: E731
            commands = {
                "top": q("top", path, "--scan", scans["basic"], "--json"),
                "stale": q("stale", path, "--scan", scans["basic"], "--json"),
                "explain": q("explain", largest_file, "--scan", scans["basic"], "--json"),
                "by_type": q("by-type", path, "--scan", scans["basic"], "--json"),
                "by_type_subtree": q("by-type", largest_dir, "--scan", scans["basic"], "--json"),
            }
            session = self.session("warm", path, None)
            for name, command in commands.items():
                tool = tools.ad_hoc(name, command)
                measure.smoke(tool, path)
                row = measure.row(t.name, "warm", name, tool.command(), session.alone(tool))
                row["kind"] = "query"
                self.rows.append(row)
                self.values[f"{name}_p50_ms"] = _ms(float(row["p50_s"]))
                self.values[f"{name}_p95_ms"] = _ms(float(row["p95_s"]))
                if name == "top":
                    top.append(tool)

        self.part("queries", queries)
        if "cold" in modes:
            self.part("first query after a cache drop", lambda: self.first_query(tree_dirs, work, cold_cmd))
        speedups: Dict[str, Optional[float]] = {}
        for competitor in competitors:
            name = competitor.tool.name
            speedups[name] = None
            if competitor.status != "ready" or competitor.path is None:
                self.failures.append(f"{t.name} {name}: {competitor.reason}")
            elif not top:
                self.failures.append(f"{t.name} {name}: no top --scan to pair it with")
            else:
                self.part(name, lambda c=competitor: speedups.update({c.tool.name: self.d34(c, top[0], path, work[tree_dirs["warm"]])}))
        found["speedups"] = speedups
        return {"gate_values": self.values, **found}

    def session(self, mode: str, path: str, cold_cmd: Optional[str]) -> measure.Session:
        warmup = self.args.warmup if mode == "warm" else 0
        return measure.Session(self.hyperfine, self.t.name, path, mode, self.args.runs, warmup, cold_cmd, self.raw_dir, self.log)

    def write(self, mode: str, tree_dirs, work, cold_cmd) -> Dict[str, object]:
        path = trees.tree_root(self.t, tree_dirs[mode])
        index_dir = os.path.join(work[tree_dirs[mode]], f"{self.t.name}-write-{mode}")
        runs = _write_runs(
            self.duscope, path, index_dir, self.args.runs, self.args.warmup if mode == "warm" else 0,
            cold_cmd if mode == "cold" else None, self.log,
        )
        summary: Dict[str, object] = {"runs": runs, "us_per_entry": _summary([r["us_per_entry"] for r in runs])}
        self.values[f"write_us_per_entry_{mode}_p50"] = summary["us_per_entry"]["p50"]  # type: ignore[index]
        if mode == "cold":
            # `-v` prints milliseconds: a scan of a smoke tree may take none.
            shares = [r["write_s"] / max(r["walk_s"] + r["rollup_s"], 0.001) for r in runs]
            summary["share_of_scan"] = _summary(shares)
            self.values["write_share_of_cold_scan_p50"] = summary["share_of_scan"]["p50"]  # type: ignore[index]
        return summary

    def first_query(self, tree_dirs, work, cold_cmd) -> None:
        """`top --scan` of a scan saved beside the cold trees, each run after a cache drop."""
        path = trees.tree_root(self.t, tree_dirs["cold"])
        cold = Duscope(self.duscope, os.path.join(work[tree_dirs["cold"]], f"{self.t.name}-cold"))
        os.makedirs(cold.index_dir, mode=0o700)
        scan = cold.save(path)
        tool = tools.ad_hoc("top", shlex.join(cold.argv("top", path, "--scan", scan, "--json")))
        measure.smoke(tool, path)
        row = measure.row(self.t.name, "cold", "top", tool.command(), self.session("cold", path, cold_cmd).alone(tool))
        row["kind"] = "first_query"
        self.rows.append(row)
        self.values["first_query_cold_p50_ms"] = _ms(float(row["p50_s"]))

    def d34(self, competitor: tools.Resolved, top: tools.Resolved, path: str, work: str) -> float:
        """`top` paired with the competitor's answer from its own snapshot; the speed-up."""
        name = competitor.tool.name
        assert competitor.path is not None
        home = os.path.join(work, f"{self.t.name}-{name}-home")
        with _home(home):
            if name == "dua":
                snapshot = os.path.join(work, f"{self.t.name}-dua.snapshot")
                _run(under_pty([competitor.path, "i", "--once", "--export", snapshot, path]), "writing dua's snapshot")
                if not os.path.isfile(snapshot):
                    raise measure.MeasureError("`dua i --once --export` wrote no snapshot")
                command = shlex.join(under_pty([competitor.path, "i", "--import", snapshot, "--once"]))
            else:
                _run([competitor.path, path, "--cache=on"], "keeping fdu's scan")
                command = shlex.join([competitor.path, path, "--stale-ok"])
            other = tools.ad_hoc(name, command)
            measure.smoke(other, path)
            subject_runs, other_runs = self.session("warm", path, None).pair(top, other)
        self.rows.append(
            dict(measure.row(self.t.name, "warm", "top", top.command(), subject_runs, paired_with=name), kind="d34")
        )
        self.rows.append(
            dict(measure.row(self.t.name, "warm", name, command, other_runs, subject_runs=subject_runs), kind="d34")
        )
        speedup = round(statistics.median(o.wall / s.wall for s, o in zip(subject_runs, other_runs)), 3)
        self.values[f"{name}_speedup_p50"] = speedup
        self.log(f"{self.t.name}: top --scan is {speedup:.1f}x faster than {name}")
        return speedup
