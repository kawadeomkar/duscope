"""The redraw budget (P4-TEST-01): how long the viewer takes to redraw at 120x40
on T2, which the phase-4 gate holds to 16 ms.

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

The result file (`<date>-<platform>-redraw.json` in `--results-dir`) holds the
usual header, one row per tree and kind, and `gates`: the line of plan/SPEC.md's
Gate 4 → 5, the worst 95th percentile of the kinds at 120x40 (the resize, which
lands on two sizes, stands beside them) against the 16 ms, read from T2 (the
first tree when T2 was not measured). A gate failing is no failure of the run; a
part that cannot be measured is recorded in `failures`, the rest is still
measured, and the command then exits 1.

`--tree-path` measures a tree that is already there, read-only, and neither
built nor verified: the one case a run on a checkout's bench/.trees wants when
it must not risk rebuilding it. Nothing else needs it.

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
from typing import Dict, List, Optional, Sequence, Tuple

import indexbench
import measure
import ptyrun
import tools
import trees

# The terminal the budget names.
COLUMNS, ROWS = 120, 40
# What the gate holds: the worst kind's p95, milliseconds.
BUDGET_MS = 16.0
# How long a press waits for the first byte of a frame before the key is taken
# to have changed nothing (`--frame-wait`). A full repaint of T2 took about 0.8 s
# on an Apple M5 Pro: a frame slower than this wait would be read by the next
# press and counted against it, so the default leaves room for a slower machine.
FRAME_WAIT_S = 5.0
# Silence that ends a frame: the viewer writes a frame without a pause, so
# this much silence after its first byte means the frame is over.
SETTLE_S = 0.05
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
    # for the new root the first time it is seen, kept after.
    ("zoom", TO_TOP, (b"z", b"Z", b"l")),
    # The colour mode and the theme pack: every cell of the map repainted,
    # the full-repaint case the design gives its 16 ms for.
    ("repaint", (), (b"c", b"p")),
    # Focus cycling: the borders of the panes.
    ("focus", (), (b"\t",)),
    # The three tables in place of the map, and the map again.
    ("views", (), (b"2", b"1", b"3", b"1", b"4", b"1")),
)

# A resize: the two sizes the terminal alternates between.
RESIZES: Sequence[Tuple[int, int]] = ((100, 30), (COLUMNS, ROWS))


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

    def press(self, key: bytes) -> Tuple[Optional[float], int]:
        """Presses `key`; the time to the frame's last byte and its size in
        bytes, `(None, 0)` when no frame came."""
        start = time.perf_counter()
        os.write(self.fd, key)
        last, count = self.settle(self.frame_wait)
        return (None if last is None else last - start), count

    def resize(self, columns: int, rows: int) -> Tuple[Optional[float], int]:
        """Resizes the terminal; the time to the frame's last byte and its
        size in bytes, `(None, 0)` when no frame came."""
        start = time.perf_counter()
        fcntl.ioctl(self.fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, columns, 0, 0))
        last, count = self.settle(self.frame_wait)
        return (None if last is None else last - start), count

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


def measure_keys(viewer: Viewer, setup: Sequence[bytes], keys: Sequence[bytes], rounds: int) -> Tuple[List[float], List[int], int]:
    """`setup`, then `rounds` cycles of `keys`: the frames' times in
    milliseconds and their sizes in bytes, and the number of presses that
    changed nothing."""
    for key in setup:
        viewer.press(key)
    times: List[float] = []
    sizes: List[int] = []
    no_frame = 0
    for _ in range(rounds):
        for key in keys:
            elapsed, size = viewer.press(key)
            if elapsed is None:
                no_frame += 1
            else:
                times.append(elapsed * 1000)
                sizes.append(size)
    return times, sizes, no_frame


def measure_resizes(viewer: Viewer, rounds: int) -> Tuple[List[float], List[int], int]:
    """`rounds` cycles of the two sizes in [`RESIZES`]."""
    times: List[float] = []
    sizes: List[int] = []
    no_frame = 0
    for _ in range(rounds):
        for columns, rows in RESIZES:
            elapsed, size = viewer.resize(columns, rows)
            if elapsed is None:
                no_frame += 1
            else:
                times.append(elapsed * 1000)
                sizes.append(size)
    return times, sizes, no_frame


def row(tree: str, kind: str, times: Sequence[float], sizes: Sequence[int], no_frame: int) -> Dict[str, object]:
    """A row of the result file for one kind of redraw on one tree."""
    out: Dict[str, object] = {"tree": tree, "kind": kind, "samples": len(times), "no_frame": no_frame}
    if times:
        out.update(_summary(times))
        out["times_ms"] = [round(t, 3) for t in times]
        out["mean_bytes"] = round(sum(sizes) / len(sizes))
    return out


def gates(worst_p95_ms: Optional[float]) -> List[Dict[str, object]]:
    """The gate line the numbers decide."""
    return [
        {
            "name": "redraw_p95_ms_at_120x40",
            "op": "<=",
            "limit": BUDGET_MS,
            "value": worst_p95_ms,
            "holds": None if worst_p95_ms is None else worst_p95_ms <= BUDGET_MS,
        }
    ]


def measure_tree(duscope: str, index_dir: str, tree: str, name: str, rounds: int, frame_wait: float, log, failures: List[str]) -> List[Dict[str, object]]:
    """Measures each kind in a fresh session of the viewer on `tree`. A kind
    that fails is recorded and the rest still measured."""
    rows: List[Dict[str, object]] = []

    def session(kind: str, measured) -> None:
        log(f"{name}: {kind}")
        viewer = Viewer(duscope, index_dir, tree, frame_wait)
        try:
            viewer.first_frame()
            times, sizes, no_frame = measured(viewer)
            rows.append(row(name, kind, times, sizes, no_frame))
            if not times:
                failures.append(f"{name} {kind}: no redraw was measured")
        except (OSError, measure.MeasureError) as error:
            failures.append(f"{name} {kind}: {error}")
        finally:
            viewer.close()

    for kind, setup, keys in KEYS:
        session(kind, lambda v, s=setup, k=keys: measure_keys(v, s, k, rounds))
    session("resize", lambda v: measure_resizes(v, rounds))
    return rows


def worst_p95(rows: Sequence[Dict[str, object]]) -> Optional[float]:
    """The largest p95 of the kinds measured at 120x40; `resize` lands on two
    sizes and is reported beside them."""
    found = [r["p95_ms"] for r in rows if "p95_ms" in r and r["kind"] != "resize"]
    return max(found) if found else None  # type: ignore[type-var,return-value]


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

    rows: List[Dict[str, object]] = []
    failures: List[str] = []
    per_tree: List[Dict[str, object]] = []
    index_dir = tempfile.mkdtemp(prefix="duscope-redraw-")
    try:
        for t, stamp in zip(tree_list, built):
            path = args.tree_path or trees.tree_root(t, args.trees_dir)
            harness.log(f"{t.name}: saving a scan of {path}")
            indexbench.Duscope(duscope.path, index_dir).save(path, "--treemap", "never", "--attrs", "basic")
            found = measure_tree(duscope.path, index_dir, path, t.name, args.rounds, args.frame_wait, harness.log, failures)
            rows.extend(found)
            per_tree.append(
                {
                    "name": t.name,
                    "scale": float(t.scale),
                    "dirs": t.dirs,
                    "files": t.files,
                    "digest": stamp["digest"],
                    "filesystem": harness.filesystem(path),
                    "worst_p95_ms": worst_p95(found),
                }
            )
    finally:
        shutil.rmtree(index_dir, ignore_errors=True)
    finished = time.gmtime()

    judged = next((i for i, t in enumerate(tree_list) if t.name == "T2"), 0)
    worst = per_tree[judged]["worst_p95_ms"] if per_tree else None
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
        "gates": gates(worst),
        "failures": failures,
        "results": rows,
    }
    name = f"{result['date']}-{plat}-redraw" + (f"-x{float(args.scale):g}" if any(float(t.scale) != 1.0 for t in tree_list) else "")
    out = args.out or os.path.join(args.results_dir, f"{name}.json")
    harness.write_result(result, out)
    for r in rows:
        shown = f"p50 {r['p50_ms']} ms, p95 {r['p95_ms']} ms, max {r['max_ms']} ms" if "p95_ms" in r else "not measured"
        harness.log(f"{r['tree']} {r['kind']}: {r['samples']} frames, {r['no_frame']} presses without one, {shown}")
    gate = result["gates"][0]
    verdict = {True: "holds", False: "FAILS", None: "not measured"}[gate["holds"]]
    harness.log(f"gate {gate['name']} {gate['op']} {gate['limit']:g}: {gate['value']} ({verdict})")
    for failure in failures:
        harness.log(f"not measured: {failure}")
    print(out)
    return 1 if failures else 0
