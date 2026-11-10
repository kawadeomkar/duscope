"""Unit tests of bench/redraw.py (P4-TEST-01): the kinds, the summaries and the
gate. duscope/tests/pty/nav_redraw.rs runs the whole subcommand against the
binary, on a pseudo-terminal."""

from __future__ import annotations

import os
import stat
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import redraw  # noqa: E402

# What the program standing in for the viewer waits between erasing the screen
# and writing the frame, as the viewer lays out a resized map, seconds. Above
# the 50 ms of silence that ends a frame.
LAYOUT_S = 0.25


class Numbers(unittest.TestCase):
    def test_p4_test_01_a_row_summarises_its_samples(self):
        row = redraw.row("T2", "move", [1.0, 2.0, 3.0, 4.0, 5.0], [100, 200, 100, 200, 400], 2)
        self.assertEqual(row["samples"], 5)
        self.assertEqual(row["no_frame"], 2)
        self.assertEqual(row["p50_ms"], 3.0)
        self.assertEqual(row["p95_ms"], 4.8)
        self.assertEqual(row["max_ms"], 5.0)
        self.assertEqual(row["mean_bytes"], 200)

    def test_p4_test_01_a_kind_without_a_frame_has_no_numbers(self):
        row = redraw.row("T2", "move", [], [], 4)
        self.assertEqual(row["samples"], 0)
        self.assertNotIn("p95_ms", row)

    def test_p4_test_01_the_gate_is_the_worst_kind_at_120x40(self):
        rows = [
            {"kind": "move", "p95_ms": 1.0},
            {"kind": "repaint", "p95_ms": 22.5},
            {"kind": "resize", "p95_ms": 300.0},
        ]
        self.assertEqual(redraw.worst_p95(rows), 22.5)
        self.assertEqual(redraw.worst_p95([{"kind": "move"}]), None)

    def test_p4_test_01_the_gate_holds_at_the_budget_and_fails_above_it(self):
        self.assertTrue(redraw.gates(16.0)[0]["holds"])
        self.assertFalse(redraw.gates(16.001)[0]["holds"])
        self.assertIsNone(redraw.gates(None)[0]["holds"])
        self.assertEqual(redraw.gates(1.0)[0]["limit"], 16.0)


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
