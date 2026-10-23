"""hyperfine in interleaved pairs, and the statistics of the result file.

Each competitor is measured in a pair with the subject: one hyperfine
invocation per round, one run of each command, the subject first on even
rounds and second on odd ones, so drift and order effects fall on both alike.
Warm pairs start with warm-up runs; cold runs drop the caches before every run
with hyperfine's --prepare. The percentiles interpolate linearly between the
closest ranks (NumPy's default), so p50 is the median.
"""

from __future__ import annotations

import json
import math
import os
import shlex
import statistics
import subprocess
from dataclasses import dataclass
from typing import Callable, Dict, List, Optional, Sequence, Tuple

from tools import Resolved, first_line

# What hyperfine records for every run: wall clock, user and system CPU time,
# and peak RSS. Hardware counters are left out: containers often forbid them.
METRICS = "time_wall_clock,time_user,time_system,memory_peak_resident"


class MeasureError(Exception):
    """A measurement could not be taken."""


@dataclass(frozen=True)
class Run:
    """One measured run."""

    wall: float
    user: Optional[float]
    system: Optional[float]
    rss: Optional[int]


def hyperfine_version(hyperfine: Sequence[str]) -> str:
    """The version of the hyperfine command, which must be 2.x or later."""
    line = first_line([*hyperfine, "--version"])
    if line is None:
        raise MeasureError(
            f"`{shlex.join(hyperfine)}` does not run: install hyperfine 2.0.0 with `just setup`, "
            "or pass --hyperfine"
        )
    version = line.split()[-1] if line.split() else ""
    try:
        major = int(version.split(".")[0])
    except ValueError:
        raise MeasureError(f"cannot read a version from `{line}`") from None
    if major < 2:
        raise MeasureError(
            f"hyperfine {version} is too old: the harness reads hyperfine 2's JSON export; "
            "install 2.0.0 with `just setup`"
        )
    return version


def read_export(path: str) -> Dict[str, List[Run]]:
    """The runs of each command name in a hyperfine 2 `--export-json` file."""
    with open(path, encoding="utf-8") as f:
        data = json.load(f)
    if data.get("schema_version") != 2:
        raise MeasureError(
            f"{path}: export schema_version {data.get('schema_version')!r}; "
            "the harness reads hyperfine 2's schema_version 2"
        )

    def value(measurement: dict, metric: str) -> Optional[float]:
        entry = measurement.get(metric)
        return None if entry is None else float(entry["value"])

    runs: Dict[str, List[Run]] = {}
    for result in data["results"]:
        name = result.get("name") or result["command"]
        for m in result["measurements"]:
            rss = value(m, "memory_peak_resident")
            runs.setdefault(name, []).append(
                Run(
                    wall=float(m["time_wall_clock"]["value"]),
                    user=value(m, "time_user"),
                    system=value(m, "time_system"),
                    rss=None if rss is None else int(rss),
                )
            )
    return runs


def percentile(values: Sequence[float], q: float) -> float:
    """The q-quantile (0 <= q <= 1) by linear interpolation between closest ranks."""
    if not values:
        raise ValueError("percentile of no values")
    ordered = sorted(values)
    h = (len(ordered) - 1) * q
    low = math.floor(h)
    high = min(low + 1, len(ordered) - 1)
    return ordered[low] + (h - low) * (ordered[high] - ordered[low])


def pair_order(subject: str, other: str, round_index: int) -> Tuple[str, str]:
    """The order of a pair's commands in a round: the subject first on even rounds."""
    return (subject, other) if round_index % 2 == 0 else (other, subject)


def _seconds(x: float) -> float:
    return round(x, 9)


def _mean(values: Sequence[Optional[float]]) -> Optional[float]:
    present = [v for v in values if v is not None]
    return _seconds(statistics.fmean(present)) if present else None


def row(
    tree: str,
    mode: str,
    tool: str,
    command: str,
    runs: Sequence[Run],
    paired_with: Optional[str] = None,
    subject_runs: Optional[Sequence[Run]] = None,
) -> Dict[str, object]:
    """One entry of the result file's `results` (bench/README.md, "Result file")."""
    walls = [r.wall for r in runs]
    rss = [r.rss for r in runs if r.rss is not None]
    ratio = None
    subject_times = None
    if subject_runs is not None:
        subject_times = [_seconds(r.wall) for r in subject_runs]
        ratio = _seconds(statistics.median(s.wall / r.wall for s, r in zip(subject_runs, runs)))
    return {
        "tree": tree,
        "mode": mode,
        "tool": tool,
        "command": command,
        "paired_with": paired_with,
        "run_count": len(walls),
        "times_s": [_seconds(w) for w in walls],
        "p50_s": _seconds(percentile(walls, 0.50)),
        "p95_s": _seconds(percentile(walls, 0.95)),
        "mean_s": _seconds(statistics.fmean(walls)),
        "stddev_s": _seconds(statistics.stdev(walls)) if len(walls) > 1 else None,
        "min_s": _seconds(min(walls)),
        "max_s": _seconds(max(walls)),
        "user_s": _mean([r.user for r in runs]),
        "system_s": _mean([r.system for r in runs]),
        "peak_rss_bytes": max(rss) if rss else None,
        "subject_times_s": subject_times,
        "subject_ratio_p50": ratio,
    }


def check_cold(command: str) -> None:
    """Run the cache-drop command once; fail clearly when it cannot run."""
    done = subprocess.run(command, shell=True, capture_output=True, text=True, errors="replace")
    if done.returncode != 0:
        detail = (done.stderr or done.stdout).strip() or f"exit status {done.returncode}"
        raise MeasureError(
            f"cold runs drop the filesystem caches before every run with `{command}`, which "
            f"failed: {detail}. The default needs root or passwordless sudo for /usr/sbin/purge "
            "(macOS) or `/usr/sbin/sysctl -q vm.drop_caches=3` (Linux), as bench/README.md "
            "shows; run warm only (--mode warm, the default) or pass --cold-cmd"
        )


def smoke(tool: Resolved, tree_path: str) -> None:
    """Run a tool once on the tree; a failure names the tool before hyperfine runs."""
    argv = tool.argv(tree_path)
    done = subprocess.run(argv, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    if done.returncode != 0:
        tail = done.stderr.decode("utf-8", "replace").strip().splitlines()[-3:]
        raise MeasureError(f"`{shlex.join(argv)}` exited {done.returncode}: {' / '.join(tail)}")


@dataclass
class Session:
    """Everything one tree and one mode share."""

    hyperfine: Sequence[str]
    tree: str
    tree_path: str
    mode: str  # warm | cold
    runs: int
    warmup: int
    cold_cmd: Optional[str]
    raw_dir: str
    log: Callable[[str], None]

    def _invoke(self, tools: Sequence[Resolved], label: str, warmup: int) -> Dict[str, Run]:
        export = os.path.join(self.raw_dir, f"{self.tree}-{self.mode}-{label}.json")
        argv = [
            *self.hyperfine, "--runs", "1", "--warmup", str(warmup), "--style", "none",
            "--metrics", METRICS, "--export-json", export,
        ]
        if self.mode == "cold" and self.cold_cmd:
            argv += ["--prepare", self.cold_cmd]
        for tool in tools:
            argv += ["--command-name", tool.tool.name, shlex.join(tool.argv(self.tree_path))]
        done = subprocess.run(argv, stdin=subprocess.DEVNULL, capture_output=True, text=True, errors="replace")
        if done.returncode != 0:
            tail = " / ".join((done.stderr or done.stdout).strip().splitlines()[-3:])
            raise MeasureError(f"hyperfine exited {done.returncode}: {tail}")
        runs = read_export(export)
        return {tool.tool.name: runs[tool.tool.name][0] for tool in tools}

    def alone(self, subject: Resolved) -> List[Run]:
        """The subject's runs when no competitor is measured."""
        runs = []
        for r in range(self.runs):
            warmup = self.warmup if (r == 0 and self.mode == "warm") else 0
            got = self._invoke([subject], f"{subject.tool.name}-r{r:02d}", warmup)
            runs.append(got[subject.tool.name])
            self.log(f"{self.tree} {self.mode}, run {r + 1}/{self.runs}: {subject.tool.name} {runs[-1].wall:.3f} s")
        return runs

    def pair(self, subject: Resolved, other: Resolved) -> Tuple[List[Run], List[Run]]:
        """The subject's and the other tool's runs, interleaved in rounds."""
        by_name = {subject.tool.name: subject, other.tool.name: other}
        subject_runs: List[Run] = []
        other_runs: List[Run] = []
        for r in range(self.runs):
            order = pair_order(subject.tool.name, other.tool.name, r)
            warmup = self.warmup if (r == 0 and self.mode == "warm") else 0
            got = self._invoke([by_name[n] for n in order], f"{other.tool.name}-r{r:02d}", warmup)
            subject_runs.append(got[subject.tool.name])
            other_runs.append(got[other.tool.name])
            self.log(
                f"{self.tree} {self.mode}, {subject.tool.name} and {other.tool.name}, run {r + 1}/{self.runs}: "
                + ", ".join(f"{n} {got[n].wall:.3f} s" for n in order)
            )
        return subject_runs, other_runs
