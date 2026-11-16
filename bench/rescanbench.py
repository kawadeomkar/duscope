"""The incremental rescan's cost against a full one (P5-BENCH-01), part of
`bench/run.sh acceptance`.

On each tree, warm: a saved scan, then rounds of a scripted small change (one
file created in each of a few directories spread over the tree), a pause for
the change history to record it, and `rescan --incremental` paired with
`rescan --full` of the same tree from the same saved scan, the incremental one
first on even rounds and second on odd ones, as the harness pairs tools. The
change is undone after each round and the directories' times put back, so the
tree still matches its stamp. Each rescan writes a whole new index, so both
times include the write.

macOS reads what changed from FSEvents' history. Linux keeps no change
history: every rescan there reads the whole tree, and `rescan --incremental`
is refused (plan/PROPOSAL.md, "Incremental rescans"). The part records the
binary's own refusal as the reason a tree is not measured: on Linux for every
tree, and on macOS for a tree whose change the checks refuse (T3's one
directory is all of its directories, over the 15% an incremental rescan
reads at most).

The rescans are timed by the harness around the process, not by hyperfine:
each one's answer names the scan the next round starts from.

Standard library only; runs on Python 3.9 and later.
"""

from __future__ import annotations

import glob
import json
import os
import shlex
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from typing import Callable, Dict, List, Optional, Sequence, Tuple

import indexbench
import measure
import trees

# The directories the change touches: at most this many, and at most 1% of the
# tree's directories (at least one), far below the 15% over which an
# incremental rescan reads the whole tree.
CHANGED_DIRS = 100
# The file the change creates in each of them: a name no tree holds, and one
# block of bytes.
CHANGE_NAME = ".duscope-bench-rescan"
CHANGE_BYTES = 4096
# How long a round waits after the change for fseventsd to record it.
SETTLE_S = 2.0

INCREMENTAL = "rescan --incremental"
FULL = "rescan --full"
COMMANDS = {
    INCREMENTAL: "duscope rescan {tree} --incremental --json -v",
    FULL: "duscope rescan {tree} --full --json -v",
}


class Refused(Exception):
    """`rescan --incremental` was refused; the message is the binary's."""


def changed_dirs(t: trees.Tree, limit: int = CHANGED_DIRS) -> List[str]:
    """The directories the change touches, relative to the root: spread evenly
    over the tree's directories in creation order, or the root when it has none
    below it (T3)."""
    below = trees.directory_paths(t)[1:] or ["."]
    count = max(1, min(limit, len(below), (len(below) + 1) // 100))
    return [below[i * len(below) // count] for i in range(count)]


Saved = List[Tuple[str, int, int]]


def change(root: str, relatives: Sequence[str]) -> Saved:
    """Creates CHANGE_NAME in each directory; returns each one's path and times
    from before, for `undo`."""
    saved: Saved = []
    try:
        for relative in relatives:
            directory = root if relative == "." else os.path.join(root, relative)
            st = os.stat(directory)
            saved.append((directory, st.st_atime_ns, st.st_mtime_ns))
            with open(os.path.join(directory, CHANGE_NAME), "wb") as f:
                f.write(b"\0" * CHANGE_BYTES)
    except BaseException:
        undo(saved)
        raise
    return saved


def undo(saved: Saved) -> None:
    """Removes what `change` created and puts each directory's times back."""
    for directory, atime, mtime in saved:
        try:
            os.remove(os.path.join(directory, CHANGE_NAME))
        except FileNotFoundError:
            pass
        os.utime(directory, ns=(atime, mtime))


def timed(argv: Sequence[str]) -> Tuple[int, measure.Run, str, str]:
    """Runs `argv` once: its exit status, the run (wall clock, user and system
    CPU time, peak RSS), its stdout and its stderr."""
    with tempfile.TemporaryFile() as out, tempfile.TemporaryFile() as err:
        started = time.perf_counter()
        process = subprocess.Popen(list(argv), stdin=subprocess.DEVNULL, stdout=out, stderr=err)
        _, status, usage = os.wait4(process.pid, 0)
        wall = time.perf_counter() - started
        process.returncode = os.waitstatus_to_exitcode(status)
        out.seek(0)
        err.seek(0)
        stdout = out.read().decode("utf-8", "replace")
        stderr = err.read().decode("utf-8", "replace")
    # ru_maxrss is bytes on macOS and KiB on Linux.
    rss = usage.ru_maxrss if sys.platform == "darwin" else usage.ru_maxrss * 1024
    return process.returncode, measure.Run(wall, usage.ru_utime, usage.ru_stime, int(rss)), stdout, stderr


def _tail(text: str) -> str:
    return " / ".join(text.strip().splitlines()[-3:])


class Duscope:
    """The duscope executable and the index directory its scans go to."""

    def __init__(self, path: str, index_dir: str):
        self.path = path
        self.index_dir = index_dir

    def save(self, root: str) -> str:
        """Saves a scan of `root`; its id."""
        argv = [self.path, "scan", root, "--save", "--json", "--index-dir", self.index_dir]
        status, _, stdout, stderr = timed(argv)
        if status != 0:
            raise measure.MeasureError(f"`{shlex.join(argv)}` exited {status}: {_tail(stderr or stdout)}")
        return str(json.loads(stdout)["scan"]["id"])

    def rescan(self, root: str, parent: str, which: str) -> Tuple[measure.Run, Dict[str, object], Optional[float]]:
        """One rescan of `root` from the saved scan `parent`: the run, the
        answer's scan block, and the write's seconds when `-v` printed them. A
        refused incremental rescan raises Refused with the binary's message."""
        flag = "--incremental" if which == INCREMENTAL else "--full"
        argv = [self.path, "rescan", root, "--scan", parent, flag, "--json", "-v", "--index-dir", self.index_dir]
        status, run, stdout, stderr = timed(argv)
        if status != 0:
            try:
                error = json.loads(stdout)["error"]
            except (ValueError, KeyError, TypeError):
                error = None
            if which == INCREMENTAL and isinstance(error, dict) and error.get("code") == "not_available":
                raise Refused(str(error.get("message")))
            raise measure.MeasureError(f"`{shlex.join(argv)}` exited {status}: {_tail(stderr or stdout)}")
        written = indexbench.WRITE_LINE.search(stderr)
        return run, json.loads(stdout)["scan"], float(written.group(2)) if written else None

    def keep(self, scan: Optional[str]) -> None:
        """Removes every saved scan but `scan`, or all of them for None: each
        is a whole index."""
        for path in glob.glob(os.path.join(self.index_dir, "scans", "*")):
            if scan is None or not os.path.basename(path).startswith(scan):
                os.remove(path)


def entry(t: trees.Tree, changed: int) -> Dict[str, object]:
    """A tree's entry of the part's `trees`, before anything is measured."""
    return {
        "name": t.name,
        "status": "not measured",
        "reason": None,
        "changed_dirs": changed,
        "dir_count": None,
        "rounds": 0,
        "rescanned_dirs": [],
        "short_runs": 0,
        "incremental_p50_s": None,
        "full_p50_s": None,
        "incremental_over_full_p50": None,
        "write_p50_s": {INCREMENTAL: None, FULL: None},
    }


def measure_tree(
    duscope: Duscope,
    t: trees.Tree,
    root: str,
    runs: int,
    settle_s: float,
    limit: int,
    log: Callable[[str], None],
    sleep: Callable[[float], None] = time.sleep,
) -> Tuple[Dict[str, object], List[Dict[str, object]]]:
    """One tree: its entry of the part's `trees`, and its two result rows (none
    when it was not measured)."""
    relatives = changed_dirs(t, limit)
    found = entry(t, len(relatives))

    def refused(e: Refused) -> Tuple[Dict[str, object], List[Dict[str, object]]]:
        found["reason"] = str(e)
        log(f"{t.name}: {INCREMENTAL} not measured: {e}")
        duscope.keep(None)
        return found, []

    parent = duscope.save(root)
    # An incremental rescan of the saved scan before any change: unrecorded,
    # it warms the path, and a platform or a volume with no history to read
    # refuses it at once.
    try:
        duscope.rescan(root, parent, INCREMENTAL)
    except Refused as e:
        return refused(e)
    duscope.keep(parent)
    times: Dict[str, List[measure.Run]] = {INCREMENTAL: [], FULL: []}
    writes: Dict[str, List[float]] = {INCREMENTAL: [], FULL: []}
    read_again: List[int] = []
    for r in range(runs):
        found["rounds"] = r + 1
        saved = change(root, relatives)
        try:
            sleep(settle_s)
            scans: Dict[str, Dict[str, object]] = {}
            for which in measure.pair_order(INCREMENTAL, FULL, r):
                try:
                    run, scan, write = duscope.rescan(root, parent, which)
                except Refused as e:
                    return refused(e)
                times[which].append(run)
                if write is not None:
                    writes[which].append(write)
                scans[which] = scan
            read_again.append(int(scans[INCREMENTAL].get("rescanned_dirs") or 0))
            found["dir_count"] = scans[FULL].get("dir_count")
            # The next round starts from the full rescan: it was read whole, and
            # its history mark is older than this round's undo and the next change.
            parent = str(scans[FULL]["id"])
            duscope.keep(parent)
            log(
                f"{t.name} rescans, run {r + 1}/{runs}: incremental {times[INCREMENTAL][-1].wall:.3f} s "
                f"({read_again[-1]} of {found['dir_count']} directories read), full {times[FULL][-1].wall:.3f} s"
            )
        finally:
            undo(saved)
    duscope.keep(None)
    rows = [
        measure.row(t.name, "warm", INCREMENTAL, COMMANDS[INCREMENTAL], times[INCREMENTAL]),
        measure.row(t.name, "warm", FULL, COMMANDS[FULL], times[FULL], INCREMENTAL, times[INCREMENTAL]),
    ]
    short = sum(1 for n in read_again if n < len(relatives))
    found.update(
        status="measured",
        rescanned_dirs=read_again,
        short_runs=short,
        incremental_p50_s=rows[0]["p50_s"],
        full_p50_s=rows[1]["p50_s"],
        incremental_over_full_p50=rows[1]["subject_ratio_p50"],
        write_p50_s={w: (round(statistics.median(v), 6) if v else None) for w, v in writes.items()},
    )
    if short:
        found["reason"] = (
            f"{short} of {runs} incremental rescans read fewer directories than the {len(relatives)} "
            "the change touched: the history had not recorded it yet; raise --settle"
        )
    log(
        f"{t.name}: {INCREMENTAL} p50 {rows[0]['p50_s']:.3f} s, {FULL} p50 {rows[1]['p50_s']:.3f} s, "
        f"incremental/full {rows[1]['subject_ratio_p50']:.3f}"
    )
    return found, rows


def measure_rescans(
    duscope_path: str,
    tree_list: Sequence[trees.Tree],
    trees_dir: str,
    runs: int,
    settle_s: float,
    limit: int,
    log: Callable[[str], None],
) -> Tuple[Dict[str, object], List[str]]:
    """The part's object for the acceptance file, and what could not be
    measured. The trees are the warm runs' copies, built and stamped; a tree a
    round changed must match its stamp again after."""
    part: Dict[str, object] = {
        "mode": "warm",
        "runs": runs,
        "settle_s": settle_s,
        "changed_dirs_limit": limit,
        "change": f"one {CHANGE_BYTES}-byte file, {CHANGE_NAME}, in each changed directory",
        "trees": [],
        "results": [],
    }
    failures: List[str] = []
    for t in tree_list:
        root = trees.tree_root(t, trees_dir)
        index_dir = tempfile.mkdtemp(prefix="duscope-rescan-")
        try:
            found, rows = measure_tree(Duscope(duscope_path, index_dir), t, root, runs, settle_s, limit, log)
        except (measure.MeasureError, OSError, KeyError, ValueError) as e:
            failures.append(f"rescan {t.name}: {e}")
            log(f"{t.name}: rescans not measured: {e}")
            found, rows = entry(t, len(changed_dirs(t, limit))), []
            found.update(status="failed", reason=str(e), rounds=None)
        finally:
            shutil.rmtree(index_dir, ignore_errors=True)
        if found["short_runs"]:
            failures.append(f"rescan {t.name}: {found['reason']}")
        part["trees"].append(found)  # type: ignore[union-attr]
        part["results"].extend(rows)  # type: ignore[union-attr]
        stamp = trees.read_stamp(t, trees_dir)
        if found["rounds"] != 0 and stamp is not None and trees.walk_digest(root) != stamp["digest"]:
            failures.append(f"rescan {t.name}: the tree no longer matches its stamp; the next run rebuilds it")
    return part, failures
