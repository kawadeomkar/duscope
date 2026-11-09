"""Unit tests of bench/redraw.py (P4-TEST-01): the kinds, the summaries and the
gate. duscope/tests/pty/nav_redraw.rs runs the whole subcommand against the
binary, on a pseudo-terminal."""

from __future__ import annotations

import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import redraw  # noqa: E402


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


if __name__ == "__main__":
    unittest.main()
