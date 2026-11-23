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

`bench/run.sh rescan` (P5-BENCH-04) times the rescan by phase at several
shares of the directories changed, 0, 0.1%, 1% and 10% by default: loading
the parent, the replay, the volatile set, the walk, the read-back, the
post-check, the roll-up and the write, each read from the binary's own log
lines and the time each reached the harness (`measure_levels`). On Linux every
rescan is a full one, and its phases are loading the parent, the walk, the
roll-up and the write.

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
import tempfile
import time
from typing import Callable, Dict, List, Optional, Sequence, Tuple

import indexbench
import measure
import tools
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


def clear(saved: Saved) -> None:
    """Removes what `change` created and leaves each directory's times as the
    removal left them: putting a directory's times back is, in FSEvents'
    history, a change to its parent, which the next replay would report."""
    for directory, _, _ in saved:
        try:
            os.remove(os.path.join(directory, CHANGE_NAME))
        except FileNotFoundError:
            pass


# Runs a command once, timed around the process (bench/measure.py).
timed = measure.timed


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

    def phased(self, root: str, parent: str, which: str) -> Tuple[measure.Run, Dict[str, object], Dict[str, Optional[float]], List[str]]:
        """`rescan` with the log lines its phases are read from (PHASE_LOG):
        the run, the answer's scan block, each phase's seconds, and what could
        not be told apart. A refused incremental rescan raises Refused."""
        flag = "--incremental" if which == INCREMENTAL else "--full"
        argv = [self.path, "rescan", root, "--scan", parent, flag, "--json", "--index-dir", self.index_dir]
        status, run, stdout, lines = measure.timed_lines(argv, dict(os.environ, DUSCOPE_LOG=PHASE_LOG))
        stderr = "\n".join(line for _, line in lines)
        if status != 0:
            try:
                error = json.loads(stdout)["error"]
            except (ValueError, KeyError, TypeError):
                error = None
            if which == INCREMENTAL and isinstance(error, dict) and error.get("code") == "not_available":
                raise Refused(str(error.get("message")))
            raise measure.MeasureError(f"`{shlex.join(argv)}` exited {status}: {_tail(stderr or stdout)}")
        found, notes = phases(lines, run.wall, which)
        return run, json.loads(stdout)["scan"], found, notes

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


# --- The rescan by phase (P5-BENCH-04) -----------------------------------------------

# The share of a tree's directories each level's change touches, in percent, in
# the order the levels run: ascending, so that the directories an earlier level
# changed, which an incremental rescan reads again while their change is recent
# (the volatile set's 4-hour window, DEV-227), are fewer than a later level's own.
LEVELS = (0.0, 0.1, 1.0, 10.0)

# The rescans' log filter: `-v`'s lines (info), and the FSEvents replay's debug
# lines alone. `--debug` would also turn on the walk's log of each directory's
# errors, which costs every directory a rescan reads.
PHASE_LOG = "info,duscope_walk::platform::macos::fsevents=debug"

# The lines the phases are read from (duscope/src/verbs/rescan.rs and scan.rs,
# crates/duscope-core/src/rescan/incremental.rs, and
# crates/duscope-walk/src/platform/macos/fsevents.rs, to which
# duscope/tests/main_path_bench.rs holds them). The replay first asks whether
# the volume logs at all; where the volume's .fseventsd is closed to the user,
# as the Data volume's is to all but root, it says on a debug line that it
# cannot tell, and that line marks where the replay starts.
REPLAY_START = re.compile(r"cannot tell whether FSEvents logs ")
REPLAY_END = re.compile(r"FSEvents replay of .* from [0-9]+: ")
PLAN_LINE = re.compile(r"incremental rescan: ([0-9,]+) of the saved scan's ([0-9,]+) directories changed")
INCREMENTAL_WALK = re.compile(r"incremental walk: ([0-9,]+) directories read, ([0-9,]+) subtrees taken from the saved scan in ([0-9.]+) s")
MERGE_LINE = re.compile(r"incremental merge: ([0-9,]+) entries taken from the saved scan in ([0-9.]+) s")
POST_CHECK_LINE = re.compile(r"post-check: .* in ([0-9.]+) s")

# A rescan's phases, in the order they run, and what is left of its time.
# `plan_s` is an incremental rescan's time before its walk, loading the
# parent, the replay and the volatile set together, which it always has even
# where the three cannot be told apart.
PHASES = ("parent_s", "replay_s", "volatile_s", "plan_s", "walk_s", "read_back_s", "post_check_s", "rollup_s", "write_s", "rest_s")


def _found(lines: Sequence[measure.Line], pattern, last: bool = False):
    """The (time, match) of the first line `pattern` matches, or the last."""
    hits = [(at, m) for at, line in lines for m in [pattern.search(line)] if m]
    if not hits:
        return None
    return hits[-1] if last else hits[0]


def _s(value: float) -> float:
    return round(value, 6)


def phases(lines: Sequence[measure.Line], wall: float, which: str) -> Tuple[Dict[str, Optional[float]], List[str]]:
    """Each phase's seconds in one rescan of kind `which`, from its stderr
    lines and the time each arrived, `wall` its whole time; and what could not
    be told apart. Where the binary prints a phase's time, that is taken; the
    others are the gaps between lines:

    - `parent_s`: from the start to the replay's start (incremental), or to
      the walk's (full): the process's start, the saved scan opened and its
      options checked, and for an incremental rescan its directories loaded
      and the files held open for writing listed;
    - `replay_s`: the FSEvents replay, from its first line to its last;
    - `volatile_s`: from the replay's end to the plan's line: the saved scan's
      recent entries read out of it;
    - `walk_s`: the walk, of the changed directories alone for an incremental
      rescan; `read_back_s`: the saved scan's other rows read back out of it;
      `post_check_s`: a sample of those directories stamped again;
    - `rollup_s` and `write_s`, as `-v` prints them;
    - `rest_s`: the rest of `wall`: the output, the process's exit, and the
      gaps between the phases."""
    out: Dict[str, Optional[float]] = dict.fromkeys(PHASES)
    notes: List[str] = []
    tail = " / ".join(line for _, line in lines[-3:])
    rollup = _found(lines, indexbench.ROLLUP_LINE)
    write = _found(lines, indexbench.WRITE_LINE)
    if rollup is None or write is None:
        raise measure.MeasureError(f"`{which}` printed no roll-up and write lines: {tail}")
    out["rollup_s"] = float(rollup[1].group(1))
    out["write_s"] = float(write[1].group(2))
    if which == INCREMENTAL:
        plan = _found(lines, PLAN_LINE)
        walk = _found(lines, INCREMENTAL_WALK)
        merge = _found(lines, MERGE_LINE)
        post = _found(lines, POST_CHECK_LINE)
        if plan is None or walk is None or merge is None or post is None:
            raise measure.MeasureError(f"`{which}` printed no plan, incremental walk, merge and post-check lines: {tail}")
        out["plan_s"] = _s(plan[0])
        out["walk_s"] = float(walk[1].group(3))
        out["read_back_s"] = float(merge[1].group(2))
        out["post_check_s"] = float(post[1].group(1))
        start = _found(lines, REPLAY_START)
        end = _found(lines, REPLAY_END, last=True)
        if end is None or end[0] > plan[0]:
            notes.append("no FSEvents replay line came before the plan's: loading the parent, the replay and "
                         "the volatile set are plan_s alone")
        else:
            out["volatile_s"] = _s(plan[0] - end[0])
            if start is None or start[0] > end[0]:
                notes.append("the replay printed no line as it started (the volume's .fseventsd is open to this "
                             "user): loading the parent and the replay are plan_s less volatile_s")
            else:
                out["parent_s"] = _s(start[0])
                out["replay_s"] = _s(end[0] - start[0])
        spent = [out["plan_s"], out["walk_s"], out["read_back_s"], out["post_check_s"]]
    else:
        whole = indexbench.WALK_LINE
        listing = _found(lines, indexbench.LISTING_LINE)
        stat = _found(lines, indexbench.STAT_LINE)
        walked = _found(lines, whole)
        if walked is not None:
            out["walk_s"], printed = float(walked[1].group(1)), walked[0]
        elif listing is not None and stat is not None:
            out["walk_s"] = round(float(listing[1].group(1)) + float(stat[1].group(1)), 3)
            printed = listing[0]
        else:
            raise measure.MeasureError(f"`{which}` printed no walk line: {tail}")
        # The walk's and the roll-up's lines are printed once the roll-up ends.
        out["parent_s"] = _s(max(0.0, printed - out["walk_s"] - out["rollup_s"]))
        spent = [out["parent_s"], out["walk_s"]]
    out["rest_s"] = _s(wall - sum(v for v in [*spent, out["rollup_s"], out["write_s"]] if v is not None))
    return out, notes


def no_history(message: str) -> bool:
    """Whether a refused incremental rescan's message says the platform keeps
    no change history (Linux), which no later rescan changes, as against a
    check this one rescan failed (`not possible (<reason>)`)."""
    return "keeps no change history" in message


def level_dirs(t: trees.Tree, percent: float) -> List[str]:
    """The directories a change of `percent` of the tree's touches, relative to
    the root: that share of its directories below the root, at least one,
    spread evenly in creation order; none at 0; the root when it has none
    below it (T3)."""
    if percent <= 0:
        return []
    below = trees.directory_paths(t)[1:] or ["."]
    count = min(len(below), max(1, round(len(below) * percent / 100)))
    return [below[i * len(below) // count] for i in range(count)]


def _p50(values: Sequence[Optional[float]]) -> Optional[float]:
    present = [v for v in values if v is not None]
    return _s(statistics.median(present)) if present else None


def _kind(status: str, reason: Optional[str] = None) -> Dict[str, object]:
    """One kind of rescan at one level, before anything is measured."""
    return {"status": status, "reason": reason, "wall_p50_s": None, "phases_p50_s": dict.fromkeys(PHASES), "notes": [], "runs": []}


def _round(
    duscope: Duscope,
    root: str,
    parent: str,
    r: int,
    kinds: Dict[str, Dict[str, object]],
    log_round: Callable[[str], None],
) -> Tuple[Dict[str, Dict[str, object]], Dict[str, measure.Run], Dict[str, Tuple[Dict[str, Optional[float]], List[str]]]]:
    """The rescans of round `r` from `parent`, an incremental one and a full
    one in the order measure.pair_order gives, each kind that is still
    measured: their scan blocks, runs and phases. A refused incremental
    rescan marks its kind not measured with the binary's reason."""
    scans: Dict[str, Dict[str, object]] = {}
    runs: Dict[str, measure.Run] = {}
    spent: Dict[str, Tuple[Dict[str, Optional[float]], List[str]]] = {}
    for which in measure.pair_order(INCREMENTAL, FULL, r):
        if kinds[which]["status"] != "measured":
            continue
        try:
            run, scan, found, notes = duscope.phased(root, parent, which)
        except Refused as e:
            kinds[which].update(status="not measured", reason=str(e))
            log_round(f"{which} refused: {e}")
            continue
        scans[which], runs[which], spent[which] = scan, run, (found, notes)
    return scans, runs, spent


def measure_levels(
    duscope: Duscope,
    t: trees.Tree,
    root: str,
    levels: Sequence[float],
    runs: int,
    warmup: int,
    settle_s: float,
    log: Callable[[str], None],
    sleep: Callable[[float], None] = time.sleep,
) -> Tuple[Dict[str, object], List[Dict[str, object]], List[str]]:
    """One tree at each level: its entry of the file's `trees`, its result
    rows, and what could not be measured. Each level runs `warmup` unrecorded
    rounds and then `runs` recorded ones; a round changes the level's
    directories, waits `settle_s` for the history where there is one, runs an
    incremental rescan and a full one from the same parent in alternating
    order (the full one alone where the incremental one is refused), takes the
    full one as the next round's parent, and removes the change. The
    directories' times are put back once, after the level's last round: put
    back after every round, they would add each changed directory's parent to
    the next round's replay, which at 10% of T2 is past the 15% of its
    directories an incremental rescan reads at most."""
    found: Dict[str, object] = {"name": t.name, "dir_count": None, "incremental": "measured", "reason": None, "levels": []}
    rows: List[Dict[str, object]] = []
    failures: List[str] = []
    parent = duscope.save(root)
    # One incremental rescan before any change: unrecorded, it warms the path,
    # and a platform with no history to read refuses it at once, for good.
    # Another refusal (too many changes, a history that dropped events) is
    # this rescan's alone, and each level tries again.
    available = True
    try:
        duscope.phased(root, parent, INCREMENTAL)
    except Refused as e:
        if no_history(str(e)):
            available = False
            found.update(incremental="not measured", reason=str(e))
        log(f"{t.name}: {INCREMENTAL} " + ("refused before the levels" if available else "not measured") + f": {e}")
    duscope.keep(parent)
    for percent in levels:
        relatives = level_dirs(t, percent)
        kinds: Dict[str, Dict[str, object]] = {
            INCREMENTAL: _kind("measured") if available else _kind("not measured", str(found["reason"])),
            FULL: _kind("measured"),
        }
        level: Dict[str, object] = {"percent": percent, "changed_dirs": len(relatives), "rounds": 0,
                                    "rescanned_dirs": [], "volatile_count": [], "short_runs": 0, **kinds}
        timed_runs: Dict[str, List[measure.Run]] = {INCREMENTAL: [], FULL: []}
        # The directories' times before the level's first change.
        originals: Optional[Saved] = None
        try:
            for r in range(warmup + runs):
                recorded = r >= warmup
                at = f"{t.name} at {percent:g}%, " + (f"run {r - warmup + 1}/{runs}" if recorded else f"warm-up {r + 1}/{warmup}")
                saved = change(root, relatives)
                if originals is None:
                    originals = saved
                try:
                    if kinds[INCREMENTAL]["status"] == "measured":
                        sleep(settle_s)
                    scans, ran, spent = _round(duscope, root, parent, r, kinds, lambda m: log(f"{at}: {m}"))
                finally:
                    clear(saved)
                if FULL not in scans:
                    raise measure.MeasureError(f"{t.name} at {percent:g}%: no {FULL} gave the next round its parent")
                found["dir_count"] = scans[FULL].get("dir_count")
                parent = str(scans[FULL]["id"])
                duscope.keep(parent)
                if not recorded:
                    log(f"{at}: done")
                    continue
                level["rounds"] = r - warmup + 1
                for which, run in ran.items():
                    phases_of, notes = spent[which]
                    kinds[which]["runs"].append({"wall_s": _s(run.wall), "phases": phases_of})  # type: ignore[union-attr]
                    kinds[which]["notes"] = sorted(set(kinds[which]["notes"]) | set(notes))  # type: ignore[arg-type]
                    timed_runs[which].append(run)
                if INCREMENTAL in scans:
                    read = int(scans[INCREMENTAL].get("rescanned_dirs") or 0)
                    level["rescanned_dirs"].append(read)  # type: ignore[union-attr]
                    level["volatile_count"].append(scans[INCREMENTAL].get("volatile_count"))  # type: ignore[union-attr]
                    level["short_runs"] += int(read < len(relatives))  # type: ignore[operator]
                log(f"{at}: " + ", ".join(f"{w} {run.wall:.3f} s" for w, run in ran.items()))
        finally:
            if originals is not None:
                undo(originals)
        for which in (INCREMENTAL, FULL):
            kind = kinds[which]
            measured = kind["runs"]
            if not measured:
                if kind["status"] == "measured":
                    kind.update(status="not measured", reason="no round was recorded")
                continue
            if kind["status"] != "measured":
                # Refused after some of the level's rounds: those stand.
                rounds = f"{len(measured)} recorded round" + ("" if len(measured) == 1 else "s")  # type: ignore[arg-type]
                kind.update(status="measured", reason=f"refused after {rounds}: {kind['reason']}")
            kind["wall_p50_s"] = _s(statistics.median(r["wall_s"] for r in measured))  # type: ignore[union-attr]
            kind["phases_p50_s"] = {p: _p50([r["phases"][p] for r in measured]) for p in PHASES}  # type: ignore[index]
            tool = f"{which} at {percent:g}%"
            if which == FULL and timed_runs[INCREMENTAL] and len(timed_runs[INCREMENTAL]) == len(timed_runs[FULL]):
                rows.append(measure.row(t.name, "warm", tool, COMMANDS[which], timed_runs[FULL],
                                        f"{INCREMENTAL} at {percent:g}%", timed_runs[INCREMENTAL]))
            else:
                rows.append(measure.row(t.name, "warm", tool, COMMANDS[which], timed_runs[which]))
        if level["short_runs"] and percent > 0:
            failures.append(
                f"rescan {t.name} at {percent:g}%: {level['short_runs']} of {level['rounds']} incremental rescans read "
                f"fewer directories than the {len(relatives)} the change touched: the history had not recorded it yet; "
                "raise --settle"
            )
        found["levels"].append(level)  # type: ignore[union-attr]
        log(f"{t.name} at {percent:g}% ({len(relatives)} directories): " + (", ".join(
            f"{w} p50 {kinds[w]['wall_p50_s']:.3f} s" for w in (INCREMENTAL, FULL) if kinds[w]["wall_p50_s"] is not None
        ) or "not measured"))
    duscope.keep(None)
    # The tree's word on incremental rescans: measured at some level, or why not.
    levels_done = found["levels"]  # type: ignore[assignment]
    if available and not any(lv[INCREMENTAL]["status"] == "measured" for lv in levels_done):  # type: ignore[union-attr,index]
        reasons = [lv[INCREMENTAL]["reason"] for lv in levels_done if lv[INCREMENTAL]["reason"]]  # type: ignore[union-attr,index]
        found.update(incremental="not measured", reason=reasons[0] if reasons else None)
    return found, rows, failures


def measure_by_phase(
    duscope_path: str,
    tree_list: Sequence[trees.Tree],
    trees_dir: str,
    levels: Sequence[float],
    runs: int,
    warmup: int,
    settle_s: float,
    log: Callable[[str], None],
) -> Tuple[List[Dict[str, object]], List[Dict[str, object]], List[str]]:
    """Each tree's entry and rows, and what could not be measured. The trees
    are built and stamped; each must match its stamp again after its rounds."""
    entries: List[Dict[str, object]] = []
    rows: List[Dict[str, object]] = []
    failures: List[str] = []
    for t in tree_list:
        root = trees.tree_root(t, trees_dir)
        index_dir = tempfile.mkdtemp(prefix="duscope-rescan-")
        try:
            found, more, lost = measure_levels(Duscope(duscope_path, index_dir), t, root, levels, runs, warmup, settle_s, log)
            rows += more
            failures += lost
        except (measure.MeasureError, OSError, KeyError, ValueError) as e:
            failures.append(f"rescan {t.name}: {e}")
            log(f"{t.name}: rescans not measured: {e}")
            found = {"name": t.name, "dir_count": None, "incremental": "failed", "reason": str(e), "levels": []}
        finally:
            shutil.rmtree(index_dir, ignore_errors=True)
        entries.append(found)
        stamp = trees.read_stamp(t, trees_dir)
        if stamp is not None and trees.walk_digest(root) != stamp["digest"]:
            failures.append(f"rescan {t.name}: the tree no longer matches its stamp; the next run rebuilds it")
    return entries, rows, failures


def parse_levels(text: str) -> List[float]:
    """`--levels`: percentages from 0 to 100, comma-separated, run in ascending order."""
    try:
        levels = sorted({float(part) for part in text.split(",") if part.strip()})
    except ValueError:
        raise ValueError(f"--levels takes percentages, comma-separated, not {text!r}") from None
    if not levels or not all(0 <= p <= 100 for p in levels):
        raise ValueError(f"--levels takes percentages from 0 to 100, not {text!r}")
    return levels


def cmd_rescan(args, harness) -> int:
    """`bench/run.sh rescan`: `harness` is the harness module, for its helpers."""
    if args.runs < 1 or args.warmup < 0 or args.settle < 0:
        raise harness.UsageError("--runs must be at least 1, --warmup and --settle at least 0")
    try:
        levels = parse_levels(args.levels)
    except ValueError as e:
        raise harness.UsageError(str(e)) from None
    key = tools.platform_key()
    plat = key.split("-")[0]
    tree_list = harness._trees(args)
    duscope = tools.resolve(tools.PINS["duscope"], key, "", False, args.duscope, harness.log)
    if duscope.status != "ready" or duscope.path is None:
        raise measure.MeasureError(f"duscope at {args.duscope} is unavailable: {duscope.reason}")
    started = time.gmtime()
    stamps = [trees.build(t, args.trees_dir, args.jobs, log=harness.log) for t in tree_list]
    entries, rows, failures = measure_by_phase(
        duscope.path, tree_list, args.trees_dir, levels, args.runs, args.warmup, args.settle, harness.log
    )
    finished = time.gmtime()
    scaled = any(float(t.scale) != 1.0 for t in tree_list)
    result = {
        "schema_version": harness.SCHEMA_VERSION,
        "kind": "rescan",
        "date": time.strftime("%Y-%m-%d", started),
        "started_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", started),
        "finished_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", finished),
        "platform": plat,
        "host": harness.host(),
        "harness": {
            "commit": harness.git_commit(),
            "runs": args.runs,
            "warmup": args.warmup,
            "settle_s": args.settle,
            "levels": levels,
            "log_filter": PHASE_LOG,
            "change": f"one {CHANGE_BYTES}-byte file, {CHANGE_NAME}, in each changed directory",
        },
        "duscope": {"path": duscope.path, "version": duscope.version},
        "trees": [
            {
                "scale": float(t.scale),
                "dirs": t.dirs,
                "files": t.files,
                "digest": stamp["digest"],
                "filesystem": harness.filesystem(trees.tree_root(t, args.trees_dir)),
                **found,
            }
            for t, stamp, found in zip(tree_list, stamps, entries)
        ],
        "failures": failures,
        "results": rows,
    }
    name = f"{result['date']}-{plat}-rescan" + (f"-x{float(args.scale):g}" if scaled else "")
    out = args.out or os.path.join(args.results_dir, f"{name}.json")
    harness.write_result(result, out)
    for failure in failures:
        harness.log(f"not measured: {failure}")
    print(out)
    return 1 if failures else 0
