"""The redraw budget (P4-TEST-01): how long the viewer takes to redraw at 120x40
on T2. The phase-4 gate holds the frames of navigation to 16 ms and, apart, the
frames that recompute the map or build something for the first time to 1 s.

    bench/run.sh redraw --duscope target/release/duscope [--trees T2] [--rounds 30]

The viewer runs on a pseudo-terminal of 120 columns by 40 rows, on a saved scan
of the tree, as the nightly job's other measurements read it. Each kind of
redraw (a cursor move, a descent and an ascent, a zoom in and out, the colour
mode, the tables of the views, a resize) is a cycle of keys, or for the resize
a change of the terminal's size, the viewer answers in a fresh session; the
time of one redraw is from the key's write to the last byte of the frame it
caused, read from the pseudo-terminal and not parsed, so the terminal's own
drawing is not in it (bench/README.md, "The redraw budget"). A key or size that
changes nothing on the screen writes no frame and yields no sample: the count
says how many.

Each frame has a class. `navigation` is what the budget of a frame is for: a
move, a descent, a focus change, a table already built, a zoom into a folder
already laid out. `recompute` is the rest: a colour or pack change and a resize,
which redo the whole map each time, and the first-time frames, a folder's first
zoom and a table's first frame, which build what later frames reuse.

The result file (`<date>-<platform>-redraw.json` in `--results-dir`) holds the
usual header, one row per tree, kind and class, `sessions` (every press in order,
those that drew nothing too), and `gates`, the two lines of plan/SPEC.md's Gate
4 → 5, read from T2 (the first tree when T2 was not measured): the worst 95th
percentile of the navigation rows against the 16 ms, and the slowest recompute
frame against the 1 s, a maximum so that no percentile can hide a frame that
happens a few times a session. A gate failing is no failure of the run; a part
that cannot be measured is recorded in `failures`, the rest is still measured,
and the command then exits 1.

`--tree-path` measures a tree that is already there, read-only, and neither
built nor verified: the one case a run on a checkout's bench/.trees wants when
it must not risk rebuilding it. Nothing else needs it.

T3, one folder of a million empty files, has kinds of its own (`TREE_KEYS`):
its map draws no tile, so the cursor moves in the siblings pane, through the
folder's million entries, and there is no folder under the root to zoom into;
the resize is measured as on every tree. The gates are read from T2 all the
same.

Standard library only; runs on Python 3.9 and later, on Linux and macOS.
"""

from __future__ import annotations

import fcntl
import os
import pty
import select
import shutil
import signal
import struct
import tempfile
import termios
import time
from typing import Any, Dict, List, NamedTuple, Optional, Sequence, Tuple

import indexbench
import measure
import ptyrun
import tools
import trees

# The terminal the budget names.
COLUMNS, ROWS = 120, 40
# The frame budget a navigation frame is held to, at the 95th percentile of its
# kind: 60 Hz, milliseconds.
BUDGET_MS = 16.0
# The ceiling of a frame that recomputes the map or builds something for the
# first time, held by the slowest one: the published limit for a response that
# keeps the user's flow of thought with no sign of progress, and the viewer shows
# none (bench/README.md, "The redraw budget"), milliseconds.
CEILING_MS = 1000.0
# How long a press waits for the first byte of a frame before the key is taken
# to have changed nothing (`--frame-wait`). A full repaint of T2 took about 0.8 s
# on an Apple M5 Pro: a frame slower than this wait would be read by the next
# press and counted against it, so the default leaves room for a slower machine.
FRAME_WAIT_S = 5.0
# Silence that ends a frame: the viewer writes a frame without a pause, so
# this much silence after its first byte means the frame is over.
SETTLE_S = 0.05
# Erases the screen. When the terminal's size changes ratatui writes and flushes
# this by itself, then lays the frame out and writes it: bytes that do no more
# than this are not a frame, and the silence after them is the viewer working.
ERASE = b"\x1b[2J"
# How long a viewer may take to open a million-entry saved scan and draw.
OPEN_WAIT_S = 120.0

# To the top row of the map, from the file the viewer opens on: up to the map's
# root (nine presses are past T2's depth; at the root one does nothing), and ⏎
# onto the largest top-level folder.
TO_TOP: Sequence[bytes] = (b"\x7f",) * 9 + (b"\r",)

# Each kind: keys to press first, and its keys, pressed in turn and repeated
# for `--rounds` cycles, in a session of their own. Keys that change nothing
# (a move at a block's edge) yield no sample and are counted.
KEYS: Sequence[Tuple[str, Sequence[bytes], Sequence[bytes]]] = (
    # A cursor move on the map: the outline and the panes, a small frame.
    ("move", TO_TOP, (b"l", b"j", b"h", b"k")),
    # Up the path and back down: the panes and breadcrumbs.
    ("descend", (), (b"\x7f", b"\r")),
    # A zoom into a top-level folder and back, then the next folder: a map
    # for the new root the first time it is seen (a first-time frame), kept
    # after (navigation).
    ("zoom", TO_TOP, (b"z", b"Z", b"l")),
    # The colour mode and the theme pack: the legend and the map's every cell
    # worked out again, each time.
    ("repaint", (), (b"c", b"p")),
    # Focus cycling: the borders of the panes.
    ("focus", (), (b"\t",)),
    # The three tables in place of the map, and the map again: a table's
    # first frame builds its rows (a first-time frame), later ones reuse them.
    ("views", (), (b"2", b"1", b"3", b"1", b"4", b"1")),
)

# The kinds of a tree whose shape the ones above do not fit, by its name. T3 is
# one folder of a million empty files: the map draws no tile, so the arrows move
# in the siblings pane, focused first, along the folder's whole list (a page at
# a time too); there is no folder below the root to zoom into or descend to.
TREE_KEYS: Dict[str, Sequence[Tuple[str, Sequence[bytes], Sequence[bytes]]]] = {
    "T3": (
        ("move", (b"\t",), (b"j", b"k")),
        ("page", (b"\t",), (b"\x1b[6~", b"\x1b[5~")),
    ),
}

# A resize: the two sizes the terminal alternates between.
RESIZES: Sequence[Tuple[int, int]] = ((100, 30), (COLUMNS, ROWS))

# The kinds whose every frame works the whole map out again.
RECOMPUTE_KINDS = ("repaint", "resize")
# The classes of frame, in the order of a kind's rows, and each one's limit.
LIMITS_MS: Dict[str, float] = {"navigation": BUDGET_MS, "recompute": CEILING_MS}
# The keys that show a table.
TABLES: Sequence[bytes] = (b"2", b"3", b"4")
# The keys a key line names in words.
KEY_NAMES: Dict[bytes, str] = {b"\x7f": "backspace", b"\r": "enter", b"\t": "tab", b"\x1b[6~": "pagedown", b"\x1b[5~": "pageup"}


class Press(NamedTuple):
    """One press of a key, or one change of the terminal's size: the key's
    name (a resize's is the size it lands on), the frame's time in milliseconds
    and size in bytes (None and 0 when none came), and whether the frame was a
    first-time one."""

    key: str
    ms: Optional[float]
    size: int
    first: bool


class Firsts:
    """Tells which frames are first-time ones: those that build what the
    viewer then keeps for the session. A table's first frame, and a zoom into a
    folder not zoomed before: the zoom kind only moves right along the map's
    top row, so the moves that drew a frame count which folder the cursor is
    on, and a place is never reached twice."""

    def __init__(self) -> None:
        self.built: set = set()
        self.place = 0

    def press(self, key: bytes, drew: bool) -> bool:
        """Takes a press in turn; whether its frame is a first-time one."""
        if not drew:
            return False
        if key == b"l":
            self.place += 1
            return False
        if key in TABLES:
            built: object = key
        elif key == b"z":
            built = ("zoom", self.place)
        else:
            return False
        first = built not in self.built
        self.built.add(built)
        return first


class Viewer:
    """A running `duscope view` on a pseudo-terminal: what it writes is read
    and dropped."""

    def __init__(self, duscope: str, index_dir: str, tree: str, frame_wait: float = FRAME_WAIT_S, columns: int = COLUMNS, rows: int = ROWS):
        argv = [duscope, "view", tree, "--index-dir", index_dir, "--theme", "mocha"]
        self.frame_wait = frame_wait
        self.pending = b""
        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            try:
                # A terminal's environment, nothing else: a variable an agent
                # or CI sets would make the viewer refuse to draw.
                env = {"TERM": "xterm-256color", "COLORTERM": "truecolor", "LC_ALL": "C.UTF-8"}
                env["PATH"] = os.environ.get("PATH", "/usr/bin:/bin")
                env["HOME"] = os.environ.get("HOME", "/")
                fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack("HHHH", rows, columns, 0, 0))
                os.execve(argv[0], argv, env)
            except OSError as error:
                os.write(2, f"redraw: {argv[0]}: {error.strerror}\n".encode())
            os._exit(127)

    def settle(self, first_wait: float, settle: float = SETTLE_S, keep: Optional[bytearray] = None) -> Tuple[Optional[float], int]:
        """Reads until `settle` of silence after the first byte, which is
        waited for at most `first_wait`. Returns when the last byte arrived
        (None if none did) and the bytes read; with `keep`, appends them."""
        deadline = time.perf_counter() + first_wait
        last: Optional[float] = None
        count = 0
        while True:
            # Before the first byte, up to the deadline; after it, until the
            # settling silence.
            timeout = max(deadline - time.perf_counter(), 0.0) if last is None else settle
            ready, _, _ = select.select([self.fd], [], [], timeout)
            if not ready:
                return last, count
            try:
                chunk = os.read(self.fd, 65536)
            except OSError:  # EIO, on Linux, once the viewer has closed the terminal
                chunk = b""
            if not chunk:
                return last, count
            count += len(chunk)
            last = time.perf_counter()
            if keep is not None:
                keep += chunk
            # ratatui asks where the cursor is when the viewer opens, as a
            # terminal it must be answered.
            found, self.pending = ptyrun.answers(self.pending + chunk)
            for answer in found:
                os.write(self.fd, answer)

    def first_frame(self) -> None:
        """Waits for the first frame, the map's title in it, and for the
        terminal to fall silent after it."""
        seen = bytearray()
        deadline = time.perf_counter() + OPEN_WAIT_S
        while b"treemap" not in seen:
            arrived, _ = self.settle(max(deadline - time.perf_counter(), 0.0), 0.05, seen)
            if arrived is None:
                raise measure.MeasureError(f"no first frame in {OPEN_WAIT_S:.0f} s; the viewer wrote: {bytes(seen[-200:])!r}")
        self.settle(SETTLE_S, 0.1)

    def frame(self, start: float) -> Tuple[Optional[float], int]:
        """Reads the frame of what happened at `start`: the time to its last
        byte and its size in bytes, `(None, 0)` when no frame came. What only
        erases the screen ([`ERASE`]) is read past, for the frame after it."""
        seen = bytearray()
        deadline = start + self.frame_wait
        while True:
            last, _ = self.settle(max(deadline - time.perf_counter(), 0.0), keep=seen)
            if last is None:
                return None, 0
            if seen.replace(ERASE, b""):
                return last - start, len(seen)

    def press(self, key: bytes) -> Tuple[Optional[float], int]:
        """Presses `key`; the time to the frame's last byte and its size in
        bytes, `(None, 0)` when no frame came."""
        start = time.perf_counter()
        os.write(self.fd, key)
        return self.frame(start)

    def resize(self, columns: int, rows: int) -> Tuple[Optional[float], int]:
        """Resizes the terminal; the time to the frame's last byte and its
        size in bytes, `(None, 0)` when no frame came."""
        start = time.perf_counter()
        fcntl.ioctl(self.fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, columns, 0, 0))
        return self.frame(start)

    def close(self) -> None:
        """Quits the viewer and reaps it."""
        try:
            os.write(self.fd, b"q")
        except OSError:
            pass
        try:
            os.close(self.fd)
        except OSError:
            pass
        try:
            for _ in range(200):
                done, _ = os.waitpid(self.pid, os.WNOHANG)
                if done:
                    return
                time.sleep(0.01)
            os.kill(self.pid, signal.SIGKILL)
            os.waitpid(self.pid, 0)
        except (OSError, ChildProcessError):
            pass


def _summary(values_ms: Sequence[float]) -> Dict[str, float]:
    """The distribution of the samples, milliseconds."""
    ordered = sorted(values_ms)
    return {
        "p50_ms": round(measure.percentile(ordered, 0.5), 3),
        "p95_ms": round(measure.percentile(ordered, 0.95), 3),
        "mean_ms": round(sum(ordered) / len(ordered), 3),
        "min_ms": round(ordered[0], 3),
        "max_ms": round(ordered[-1], 3),
    }


def key_name(key: bytes) -> str:
    """How a result file names `key`."""
    return KEY_NAMES.get(key, key.decode())


def measure_keys(viewer: Viewer, kind: str, setup: Sequence[bytes], keys: Sequence[bytes], rounds: int) -> List[Press]:
    """`setup`, then `rounds` cycles of `keys`: every press of the cycles, in
    order."""
    for key in setup:
        viewer.press(key)
    firsts = Firsts()
    presses: List[Press] = []
    for _ in range(rounds):
        for key in keys:
            elapsed, size = viewer.press(key)
            drew = elapsed is not None
            presses.append(Press(key_name(key), elapsed * 1000 if elapsed is not None else None, size, firsts.press(key, drew)))
    return presses


def measure_resizes(viewer: Viewer, rounds: int) -> List[Press]:
    """`rounds` cycles of the two sizes in [`RESIZES`]."""
    presses: List[Press] = []
    for _ in range(rounds):
        for columns, rows in RESIZES:
            elapsed, size = viewer.resize(columns, rows)
            presses.append(Press(f"{columns}x{rows}", elapsed * 1000 if elapsed is not None else None, size, False))
    return presses


def kinds(tree: str) -> Sequence[Tuple[str, Sequence[bytes], Sequence[bytes]]]:
    """The kinds measured on the tree named `tree`, the resize apart."""
    return TREE_KEYS.get(tree, KEYS)


def frame_class(kind: str, first: bool) -> str:
    """The class of a frame of `kind`."""
    return "recompute" if first or kind in RECOMPUTE_KINDS else "navigation"


def row(tree: str, kind: str, klass: str, presses: Sequence[Press]) -> Dict[str, Any]:
    """A row of the result file: one class of frame of one kind on one tree."""
    frames = [p for p in presses if p.ms is not None]
    limit = LIMITS_MS[klass]
    out: Dict[str, Any] = {
        "tree": tree,
        "kind": kind,
        "class": klass,
        "limit_ms": limit,
        "samples": len(frames),
        "no_frame": len(presses) - len(frames),
        "first": sum(1 for p in frames if p.first),
        "over_limit": sum(1 for p in frames if p.ms > limit),
    }
    if frames:
        times = [p.ms for p in frames]
        out.update(_summary(times))
        out["times_ms"] = [round(t, 3) for t in times]
        out["keys"] = [p.key for p in frames]
        out["mean_bytes"] = round(sum(p.size for p in frames) / len(frames))
    return out


def rows_of(tree: str, kind: str, presses: Sequence[Press]) -> List[Dict[str, Any]]:
    """The rows of a kind's presses, one for each class they fall in."""
    out = []
    for klass in LIMITS_MS:
        mine = [p for p in presses if frame_class(kind, p.first) == klass]
        if mine:
            out.append(row(tree, kind, klass, mine))
    return out


def navigation_p95(rows: Sequence[Dict[str, Any]]) -> Optional[float]:
    """The largest p95 of the navigation rows."""
    found = [r["p95_ms"] for r in rows if r["class"] == "navigation" and "p95_ms" in r]
    return max(found) if found else None


def recompute_max(rows: Sequence[Dict[str, Any]]) -> Optional[float]:
    """The slowest frame of the recompute rows."""
    found = [r["max_ms"] for r in rows if r["class"] == "recompute" and "max_ms" in r]
    return max(found) if found else None


def tree_summary(rows: Sequence[Dict[str, Any]]) -> Dict[str, Any]:
    """A tree's two figures and the counts beside them: the frames over each
    limit, and the first-time frames, which the recompute rows hold."""
    firsts = [t for r in rows if r["first"] for t in r["times_ms"]]
    return {
        "navigation_p95_ms": navigation_p95(rows),
        "navigation_over_budget": sum(r["over_limit"] for r in rows if r["class"] == "navigation"),
        "recompute_max_ms": recompute_max(rows),
        "recompute_over_ceiling": sum(r["over_limit"] for r in rows if r["class"] == "recompute"),
        "first_frames": len(firsts),
        "first_max_ms": max(firsts) if firsts else None,
    }


def _gate(name: str, limit: float, value: Optional[float]) -> Dict[str, object]:
    """A gate line; `holds` is None when nothing was measured."""
    return {"name": name, "op": "<=", "limit": limit, "value": value, "holds": None if value is None else value <= limit}


def gates(navigation_p95_ms: Optional[float], recompute_max_ms: Optional[float]) -> List[Dict[str, object]]:
    """The gate lines the numbers decide."""
    return [
        _gate("redraw_p95_ms_at_120x40", BUDGET_MS, navigation_p95_ms),
        _gate("redraw_recompute_max_ms", CEILING_MS, recompute_max_ms),
    ]


def measure_tree(duscope: str, index_dir: str, tree: str, name: str, rounds: int, frame_wait: float, log, failures: List[str]) -> Tuple[List[Dict[str, Any]], List[Dict[str, Any]]]:
    """Measures each kind in a fresh session of the viewer on `tree`: its rows
    and its sessions. A kind that fails is recorded and the rest still
    measured."""
    rows: List[Dict[str, Any]] = []
    sessions: List[Dict[str, Any]] = []

    def session(kind: str, measured) -> None:
        log(f"{name}: {kind}")
        viewer = Viewer(duscope, index_dir, tree, frame_wait)
        try:
            viewer.first_frame()
            presses = measured(viewer)
            rows.extend(rows_of(name, kind, presses))
            sessions.append({"tree": name, "kind": kind, "presses": [[p.key, None if p.ms is None else round(p.ms, 3), p.size, p.first] for p in presses]})
            if not any(p.ms is not None for p in presses):
                failures.append(f"{name} {kind}: no redraw was measured")
        except (OSError, measure.MeasureError) as error:
            failures.append(f"{name} {kind}: {error}")
        finally:
            viewer.close()

    for kind, setup, keys in kinds(name):
        session(kind, lambda v, kd=kind, s=setup, k=keys: measure_keys(v, kd, s, k, rounds))
    session("resize", lambda v: measure_resizes(v, rounds))
    return rows, sessions


def cmd_redraw(args, harness) -> int:
    """`bench/run.sh redraw`: `harness` is the harness module, for its helpers."""
    if args.rounds < 1:
        raise harness.UsageError("--rounds must be at least 1")
    if args.frame_wait <= 0:
        raise harness.UsageError("--frame-wait must be above 0")
    tree_list = harness._trees(args)
    if args.tree_path and len(tree_list) != 1:
        raise harness.UsageError("--tree-path names one tree: give --trees one name")
    key = tools.platform_key()
    plat = key.split("-")[0]
    started = time.gmtime()
    duscope = tools.resolve(tools.PINS["duscope"], key, args.tools_dir, False, args.duscope, harness.log)
    if duscope.status != "ready" or duscope.path is None:
        raise measure.MeasureError(f"duscope at {args.duscope} is unavailable: {duscope.reason}")

    if args.tree_path:
        built = [{"digest": None}]
    else:
        built = [trees.build(t, args.trees_dir, args.jobs, log=harness.log) for t in tree_list]

    rows: List[Dict[str, Any]] = []
    sessions: List[Dict[str, Any]] = []
    failures: List[str] = []
    per_tree: List[Dict[str, Any]] = []
    index_dir = tempfile.mkdtemp(prefix="duscope-redraw-")
    try:
        for t, stamp in zip(tree_list, built):
            path = args.tree_path or trees.tree_root(t, args.trees_dir)
            harness.log(f"{t.name}: saving a scan of {path}")
            indexbench.Duscope(duscope.path, index_dir).save(path, "--treemap", "never", "--attrs", "basic")
            found, played = measure_tree(duscope.path, index_dir, path, t.name, args.rounds, args.frame_wait, harness.log, failures)
            rows.extend(found)
            sessions.extend(played)
            per_tree.append(
                {
                    "name": t.name,
                    "scale": float(t.scale),
                    "dirs": t.dirs,
                    "files": t.files,
                    "digest": stamp["digest"],
                    "filesystem": harness.filesystem(path),
                    **tree_summary(found),
                }
            )
    finally:
        shutil.rmtree(index_dir, ignore_errors=True)
    finished = time.gmtime()

    judged = next((i for i, t in enumerate(tree_list) if t.name == "T2"), 0)
    judged_tree = per_tree[judged] if per_tree else {}
    result = {
        "schema_version": harness.SCHEMA_VERSION,
        "kind": "redraw",
        "date": time.strftime("%Y-%m-%d", started),
        "started_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", started),
        "finished_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", finished),
        "platform": plat,
        "subject": "duscope",
        "host": harness.host(),
        "harness": {
            "commit": harness.git_commit(),
            "rounds": args.rounds,
            "columns": COLUMNS,
            "rows": ROWS,
            "settle_ms": SETTLE_S * 1000,
            "frame_wait_ms": args.frame_wait * 1000,
            "tree_path": args.tree_path or None,
            "note": args.note or None,
        },
        "duscope": {"path": duscope.path, "version": duscope.version},
        "trees": per_tree,
        "gates_tree": tree_list[judged].name if tree_list else None,
        "gates": gates(judged_tree.get("navigation_p95_ms"), judged_tree.get("recompute_max_ms")),
        "failures": failures,
        "results": rows,
        "sessions": sessions,
    }
    name = f"{result['date']}-{plat}-redraw" + (f"-x{float(args.scale):g}" if any(float(t.scale) != 1.0 for t in tree_list) else "")
    out = args.out or os.path.join(args.results_dir, f"{name}.json")
    harness.write_result(result, out)
    for r in rows:
        shown = f"p50 {r['p50_ms']} ms, p95 {r['p95_ms']} ms, max {r['max_ms']} ms" if "p95_ms" in r else "not measured"
        harness.log(f"{r['tree']} {r['kind']} ({r['class']}): {r['samples']} frames, {r['first']} first-time, {r['over_limit']} over {r['limit_ms']:g} ms, {r['no_frame']} presses without one, {shown}")
    for gate in result["gates"]:
        verdict = {True: "holds", False: "FAILS", None: "not measured"}[gate["holds"]]
        harness.log(f"gate {gate['name']} {gate['op']} {gate['limit']:g}: {gate['value']} ({verdict})")
    for failure in failures:
        harness.log(f"not measured: {failure}")
    print(out)
    return 1 if failures else 0
