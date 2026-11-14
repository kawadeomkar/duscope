"""Unit tests of bench/redraw.py (P4-TEST-01): the classes of frame, the first-time
frames, the summaries and the two gates. duscope/tests/pty/nav_redraw.rs runs the
whole subcommand against the binary, on a pseudo-terminal."""

from __future__ import annotations

import os
import stat
import sys
import tempfile
import unittest
from typing import List, Optional, Sequence, Tuple

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import redraw  # noqa: E402
from redraw import Press  # noqa: E402

# What the program standing in for the viewer waits between erasing the screen
# and writing the frame, as the viewer lays out a resized map, seconds. Above
# the 50 ms of silence that ends a frame.
LAYOUT_S = 0.25


class Scripted:
    """Stands in for a Viewer: each press answers the next of `answers`, a
    frame's time in seconds and size, or None for a press that drew nothing."""

    def __init__(self, answers: Sequence[Optional[Tuple[float, int]]]):
        self.answers = list(answers)
        self.pressed: List[bytes] = []

    def press(self, key: bytes) -> Tuple[Optional[float], int]:
        self.pressed.append(key)
        answer = self.answers.pop(0)
        return (None, 0) if answer is None else answer


def frames(*keys: str, ms: float = 1.0, first: bool = False) -> List[Press]:
    return [Press(key, ms, 100, first) for key in keys]


class Numbers(unittest.TestCase):
    def test_p4_test_01_a_row_summarises_its_samples(self):
        presses = [Press("l", float(ms), size, False) for ms, size in zip((1, 2, 3, 4, 5), (100, 200, 100, 200, 400))]
        row = redraw.row("T2", "move", "navigation", presses + [Press("l", None, 0, False)] * 2)
        self.assertEqual(row["class"], "navigation")
        self.assertEqual(row["limit_ms"], 16.0)
        self.assertEqual(row["samples"], 5)
        self.assertEqual(row["no_frame"], 2)
        self.assertEqual(row["first"], 0)
        self.assertEqual(row["p50_ms"], 3.0)
        self.assertEqual(row["p95_ms"], 4.8)
        self.assertEqual(row["max_ms"], 5.0)
        self.assertEqual(row["mean_bytes"], 200)
        self.assertEqual(row["keys"], ["l"] * 5)

    def test_p4_test_01_a_kind_without_a_frame_has_no_numbers(self):
        row = redraw.row("T2", "move", "navigation", [Press("l", None, 0, False)] * 4)
        self.assertEqual(row["samples"], 0)
        self.assertEqual(row["no_frame"], 4)
        self.assertNotIn("p95_ms", row)

    def test_p4_test_01_a_row_counts_the_frames_over_its_limit(self):
        navigation = redraw.row("T2", "views", "navigation", frames("2", ms=16.0) + frames("3", ms=16.5))
        self.assertEqual(navigation["over_limit"], 1)
        recompute = redraw.row("T2", "repaint", "recompute", frames("c", ms=1000.0) + frames("p", ms=1000.5))
        self.assertEqual(recompute["limit_ms"], 1000.0)
        self.assertEqual(recompute["over_limit"], 1)


class Classes(unittest.TestCase):
    def test_p4_test_01_a_folders_first_zoom_is_first_time_and_a_kept_one_navigation(self):
        viewer = Scripted(
            [
                (0.124, 900),  # z: the first folder, first time
                (0.001, 900),  # Z
                (0.0003, 90),  # l: on to the next folder
                (0.155, 900),  # z: that folder, first time
                (0.001, 900),  # Z
                None,  # l: at the edge, the cursor stays
                (0.002, 900),  # z: the same folder, kept
                (0.002, 900),  # Z
                None,  # l
            ]
        )
        presses = redraw.measure_keys(viewer, "zoom", (), (b"z", b"Z", b"l"), 3)
        self.assertEqual([p.key for p in presses], ["z", "Z", "l"] * 3)
        self.assertEqual([p.first for p in presses], [True, False, False, True, False, False, False, False, False])
        rows = redraw.rows_of("T2", "zoom", presses)
        self.assertEqual([r["class"] for r in rows], ["navigation", "recompute"])
        self.assertEqual(rows[0]["samples"], 5)
        self.assertEqual(rows[0]["no_frame"], 2)
        self.assertEqual(rows[0]["first"], 0)
        self.assertEqual(rows[1]["keys"], ["z", "z"])
        self.assertEqual(rows[1]["first"], 2)
        self.assertEqual(rows[1]["max_ms"], 155.0)

    def test_p4_test_01_a_zoom_that_drew_nothing_leaves_the_folder_first_time(self):
        viewer = Scripted([None, (0.1, 900)])
        presses = redraw.measure_keys(viewer, "zoom", (), (b"z",), 2)
        self.assertEqual([p.first for p in presses], [False, True])

    def test_p4_test_01_a_tables_first_frame_is_first_time_and_the_map_never_is(self):
        viewer = Scripted([(0.001, 100)] * 12)
        presses = redraw.measure_keys(viewer, "views", (), (b"2", b"1", b"3", b"1", b"4", b"1"), 2)
        self.assertEqual([p.first for p in presses], [True, False, True, False, True, False] + [False] * 6)
        rows = redraw.rows_of("T2", "views", presses)
        self.assertEqual([(r["class"], r["samples"]) for r in rows], [("navigation", 9), ("recompute", 3)])
        self.assertEqual(rows[1]["keys"], ["2", "3", "4"])

    def test_p4_test_01_a_setup_key_is_pressed_and_never_measured(self):
        viewer = Scripted([(0.5, 100), (0.001, 100)])
        presses = redraw.measure_keys(viewer, "move", (b"\x7f",), (b"l",), 1)
        self.assertEqual(viewer.pressed, [b"\x7f", b"l"])
        self.assertEqual([p.key for p in presses], ["l"])

    def test_p4_tui_09_t3_moves_in_the_siblings_pane_and_keeps_the_resize(self):
        # T3's map draws no tile: its kinds move in the siblings pane,
        # focused first and never measured; every other tree keeps the kinds
        # the gates are read from.
        self.assertEqual(redraw.kinds("T2"), redraw.KEYS)
        self.assertEqual(redraw.kinds("T1"), redraw.KEYS)
        names = [kind for kind, _, _ in redraw.kinds("T3")]
        self.assertEqual(names, ["move", "page"])
        for kind, setup, keys in redraw.kinds("T3"):
            self.assertEqual(setup, (b"\t",), kind)
            viewer = Scripted([(0.001, 100)] * (len(setup) + len(keys)))
            presses = redraw.measure_keys(viewer, kind, setup, keys, 1)
            self.assertEqual(viewer.pressed, list(setup) + list(keys), kind)
            self.assertEqual(len(presses), len(keys), kind)
            self.assertEqual([r["class"] for r in redraw.rows_of("T3", kind, presses)], ["navigation"], kind)
        self.assertEqual([p.key for p in redraw.measure_keys(Scripted([(0.001, 100)] * 2), "page", (), (b"\x1b[6~", b"\x1b[5~"), 1)], ["pagedown", "pageup"])

    def test_p4_test_01_keys_are_named_as_the_key_line_reads(self):
        viewer = Scripted([(0.001, 100)] * 3)
        presses = redraw.measure_keys(viewer, "descend", (), (b"\x7f", b"\r", b"\t"), 1)
        self.assertEqual([p.key for p in presses], ["backspace", "enter", "tab"])

    def test_p4_test_01_a_repaint_and_a_resize_are_whole_recomputes_each_time(self):
        for kind in ("repaint", "resize"):
            rows = redraw.rows_of("T2", kind, frames("c", "p", ms=700.0))
            self.assertEqual([r["class"] for r in rows], ["recompute"], kind)
            self.assertEqual(rows[0]["first"], 0, kind)

    def test_p4_test_01_each_kind_is_navigation_unless_a_frame_is_first_time(self):
        for kind in ("move", "descend", "focus"):
            rows = redraw.rows_of("T2", kind, frames("x"))
            self.assertEqual([r["class"] for r in rows], ["navigation"], kind)

    def test_p4_test_01_a_resize_names_the_size_it_lands_on(self):
        class Resizes:
            def resize(self, columns: int, rows: int) -> Tuple[Optional[float], int]:
                return 0.3, 1000

        presses = redraw.measure_resizes(Resizes(), 1)
        self.assertEqual([p.key for p in presses], ["100x30", "120x40"])


class Gates(unittest.TestCase):
    def test_p4_test_01_the_budget_reads_the_worst_navigation_kind(self):
        rows = [
            {"kind": "move", "class": "navigation", "p95_ms": 1.0, "max_ms": 2.0},
            {"kind": "views", "class": "navigation", "p95_ms": 2.5, "max_ms": 3.0},
            {"kind": "repaint", "class": "recompute", "p95_ms": 722.0, "max_ms": 723.0},
            {"kind": "zoom", "class": "recompute", "p95_ms": 150.0, "max_ms": 155.0},
        ]
        self.assertEqual(redraw.navigation_p95(rows), 2.5)
        self.assertEqual(redraw.recompute_max(rows), 723.0)

    def test_p4_test_01_a_gate_without_its_frames_has_no_value(self):
        self.assertIsNone(redraw.navigation_p95([{"kind": "move", "class": "navigation"}]))
        self.assertIsNone(redraw.recompute_max([]))

    def test_p4_test_01_the_gates_hold_at_their_limits_and_fail_above_them(self):
        names = [gate["name"] for gate in redraw.gates(1.0, 1.0)]
        self.assertEqual(names, ["redraw_p95_ms_at_120x40", "redraw_recompute_max_ms"])
        self.assertEqual([gate["limit"] for gate in redraw.gates(1.0, 1.0)], [16.0, 1000.0])
        self.assertTrue(redraw.gates(16.0, 1000.0)[0]["holds"])
        self.assertTrue(redraw.gates(16.0, 1000.0)[1]["holds"])
        self.assertFalse(redraw.gates(16.001, 1.0)[0]["holds"])
        self.assertTrue(redraw.gates(16.001, 1.0)[1]["holds"])
        self.assertFalse(redraw.gates(1.0, 1000.5)[1]["holds"])
        self.assertIsNone(redraw.gates(None, None)[0]["holds"])
        self.assertIsNone(redraw.gates(None, None)[1]["holds"])

    def test_p4_test_01_the_gates_are_judged_on_t2_alone(self):
        t1 = {"name": "T1", "navigation_p95_ms": 30.0, "recompute_max_ms": 2000.0}
        t2 = {"name": "T2", "navigation_p95_ms": 3.0, "recompute_max_ms": 700.0}
        t3 = {"name": "T3", "navigation_p95_ms": 0.3, "recompute_max_ms": 409.0}
        for measured in ([t2], [t3, t2], [t1, t2, t3]):
            tree, lines = redraw.judged(measured)
            self.assertEqual(tree, "T2")
            self.assertEqual([gate["value"] for gate in lines], [3.0, 700.0])
            self.assertEqual(
                redraw.verdicts(tree, lines),
                ["gate redraw_p95_ms_at_120x40 <= 16: 3.0 (holds)", "gate redraw_recompute_max_ms <= 1000: 700.0 (holds)"],
            )
        # T3's sub-millisecond moves, or T1's frames, are not the budget's: a
        # run without T2 judges nothing, rather than holding or failing.
        for measured in ([t3], [t1], [t1, t3], []):
            tree, lines = redraw.judged(measured)
            self.assertIsNone(tree)
            self.assertEqual([gate["value"] for gate in lines], [None, None])
            self.assertEqual([gate["holds"] for gate in lines], [None, None])
            self.assertEqual(
                redraw.verdicts(tree, lines),
                [
                    "gate redraw_p95_ms_at_120x40 <= 16: not judged, T2 was not measured",
                    "gate redraw_recompute_max_ms <= 1000: not judged, T2 was not measured",
                ],
            )
        # T2 measured, a gate without its frames.
        tree, lines = redraw.judged([{"name": "T2", "navigation_p95_ms": None, "recompute_max_ms": 1200.0}])
        self.assertEqual(
            redraw.verdicts(tree, lines),
            ["gate redraw_p95_ms_at_120x40 <= 16: None (not measured)", "gate redraw_recompute_max_ms <= 1000: 1200.0 (FAILS)"],
        )

    def test_p4_test_01_no_number_of_rounds_hides_a_first_zoom(self):
        # z, Z, l at 30 rounds, as the nightly runs: two first zooms among 61
        # frames. In one row its p95 would be about 2.5 ms.
        answers: List[Optional[Tuple[float, int]]] = [(0.124, 900), (0.002, 900), (0.0003, 90), (0.155, 900), (0.002, 900), None]
        for _ in range(28):
            answers += [(0.0025, 900), (0.0025, 900), None]
        presses = redraw.measure_keys(Scripted(answers), "zoom", (), (b"z", b"Z", b"l"), 30)
        mixed = sorted(p.ms for p in presses if p.ms is not None)
        self.assertLess(redraw.measure.percentile(mixed, 0.95), 16.0)
        rows = redraw.rows_of("T2", "zoom", presses)
        self.assertLess(redraw.navigation_p95(rows), 16.0)
        self.assertEqual(redraw.recompute_max(rows), 155.0)
        self.assertEqual(rows[1]["times_ms"], [124.0, 155.0])

    def test_p4_test_01_a_trees_summary_counts_what_went_over(self):
        rows = redraw.rows_of("T2", "views", frames("2", ms=21.7, first=True) + frames("1", "1", ms=2.0) + frames("3", ms=17.0))
        rows += redraw.rows_of("T2", "repaint", frames("c", ms=1200.0) + frames("p", ms=700.0))
        summary = redraw.tree_summary(rows)
        self.assertEqual(summary["navigation_over_budget"], 1)
        self.assertEqual(summary["recompute_over_ceiling"], 1)
        self.assertEqual(summary["recompute_max_ms"], 1200.0)
        self.assertEqual(summary["first_frames"], 1)
        self.assertEqual(summary["first_max_ms"], 21.7)


class Resize(unittest.TestCase):
    def fake(self, directory: str, then_draws: bool) -> str:
        """A program that behaves as the viewer does on a resize: erases the
        screen at once, works in silence, then writes the frame."""
        path = os.path.join(directory, "viewer")
        draw = f"    time.sleep({LAYOUT_S})\n    os.write(1, b'frame' * 100)\n" if then_draws else ""
        with open(path, "w") as out:
            out.write(
                f"#!{sys.executable} -I\n"
                "import os, signal, time, tty\n"
                "tty.setraw(0)\n"
                "def resized(*_):\n"
                "    os.write(1, b'\\x1b[2J')\n"
                f"{draw}"
                "signal.signal(signal.SIGWINCH, resized)\n"
                "os.write(1, b'treemap')\n"
                "while os.read(0, 1) != b'q':\n"
                "    pass\n"
            )
        os.chmod(path, os.stat(path).st_mode | stat.S_IXUSR)
        return path

    def test_p4_test_01_a_resize_is_timed_to_its_frame_not_to_the_erase_before_it(self):
        with tempfile.TemporaryDirectory() as directory:
            viewer = redraw.Viewer(self.fake(directory, True), directory, directory, frame_wait=10.0)
            try:
                viewer.first_frame()
                for columns, rows in ((100, 30), (120, 40), (100, 30)):
                    elapsed, size = viewer.resize(columns, rows)
                    self.assertIsNotNone(elapsed)
                    self.assertGreaterEqual(elapsed, LAYOUT_S, (columns, rows))
                    self.assertGreaterEqual(size, 500, (columns, rows))
            finally:
                viewer.close()

    def test_p4_test_01_a_resize_that_only_erases_draws_no_frame(self):
        with tempfile.TemporaryDirectory() as directory:
            viewer = redraw.Viewer(self.fake(directory, False), directory, directory, frame_wait=0.3)
            try:
                viewer.first_frame()
                self.assertEqual(viewer.resize(100, 30), (None, 0))
            finally:
                viewer.close()


if __name__ == "__main__":
    unittest.main()
