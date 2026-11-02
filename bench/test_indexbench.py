"""Unit tests of bench/indexbench.py (P2-BENCH-01): reading `scan -v`, the
gates, and the pseudo-terminal dua's snapshot is written under. The whole
subcommand runs in duscope/tests/index_bench.rs, against the binary."""

from __future__ import annotations

import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import indexbench  # noqa: E402
import measure  # noqa: E402


class Verbose(unittest.TestCase):
    STDERR = (
        "duscope: walk: 1,000,001 entries, 125,001 directories, 3.4 GB in 1.234 s: 810,374 entries/s\n"
        "duscope: roll-up: 0.045 s\n"
        "duscope: write: 1,000,001 entries in 1.110 s: 1.11 µs/entry\n"
    )

    def test_p2_bench_01_scan_v_gives_the_walk_roll_up_and_write(self):
        got = indexbench.parse_verbose(self.STDERR)
        self.assertEqual(
            got,
            {"entries": 1_000_001, "write_s": 1.11, "us_per_entry": 1.11, "walk_s": 1.234, "rollup_s": 0.045},
        )

    def test_p2_bench_01_a_scan_without_the_lines_is_an_error(self):
        with self.assertRaises(measure.MeasureError):
            indexbench.parse_verbose("duscope: walk: 1 entries, 1 directories, 0 B in 0.001 s: 1 entries/s\n")


class Gates(unittest.TestCase):
    def test_p2_bench_01_each_gate_holds_fails_or_was_not_measured(self):
        got = {g["name"]: g for g in indexbench.gates({"top_p50_ms": 3.0, "top_p95_ms": 60.0, "dua_speedup_p50": 40.0})}
        self.assertEqual([g[0] for g in indexbench.GATES], list(got))
        self.assertTrue(got["top_p50_ms"]["holds"])
        self.assertFalse(got["top_p95_ms"]["holds"])
        self.assertTrue(got["dua_speedup_p50"]["holds"])
        self.assertIsNone(got["fdu_speedup_p50"]["holds"])
        self.assertIsNone(got["fdu_speedup_p50"]["value"])

    def test_p2_bench_01_the_gates_are_plan_specs(self):
        limits = {name: (op, limit) for name, op, limit in indexbench.GATES}
        self.assertEqual(limits["write_us_per_entry_warm_p50"], ("<=", 2.5))
        self.assertEqual(limits["write_share_of_cold_scan_p50"], ("<=", 0.25))
        for verb in ("top", "stale", "explain"):
            self.assertEqual(limits[f"{verb}_p50_ms"], ("<=", 20.0))
            self.assertEqual(limits[f"{verb}_p95_ms"], ("<=", 50.0))
        self.assertEqual(limits["by_type_p50_ms"], ("<=", 50.0))
        self.assertEqual(limits["by_type_subtree_p50_ms"], ("<=", 50.0))
        self.assertEqual(limits["first_query_cold_p50_ms"], ("<=", 250.0))
        self.assertEqual(limits["bytes_per_entry_basic"], ("<=", 110.0))
        self.assertEqual(limits["bytes_per_entry_full"], ("<=", 140.0))
        self.assertEqual(limits["dua_speedup_p50"], (">=", 20.0))
        self.assertEqual(limits["fdu_speedup_p50"], (">=", 20.0))


class Pty(unittest.TestCase):
    def test_p2_bench_01_dua_runs_under_script(self):
        argv = indexbench.under_pty(["dua", "i", "--once", "--export", "/t/s p", "/t/tree"])
        self.assertEqual(argv[0], "script")
        if sys.platform == "darwin":
            self.assertEqual(argv, ["script", "-q", "/dev/null", "dua", "i", "--once", "--export", "/t/s p", "/t/tree"])
        else:
            self.assertEqual(argv, ["script", "-qec", "dua i --once --export '/t/s p' /t/tree", "/dev/null"])


if __name__ == "__main__":
    unittest.main()
