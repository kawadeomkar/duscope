"""Unit tests of bench/indexbench.py (P2-BENCH-01): reading `scan -v`, the
gates, the filtered shapes (P2-QRY-10), and the pseudo-terminal dua runs on
(bench/ptyrun.py). The whole
subcommand runs in duscope/tests/index_bench.rs, against the binary."""

from __future__ import annotations

import calendar
import os
import subprocess
import sys
import time
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import indexbench  # noqa: E402
import measure  # noqa: E402
import ptyrun  # noqa: E402
import trees  # noqa: E402


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

    def test_p5_walk_04_linux_listing_and_stat_lines_are_the_walk(self):
        stderr = self.STDERR.replace(
            "duscope: walk: 1,000,001 entries, 125,001 directories, 3.4 GB in 1.234 s: 810,374 entries/s\n",
            "duscope: listing: 125,001 directories in 0.412 s: 303,400 directories/s\n"
            "duscope: stat: 1,000,001 entries, 3,400 MB in 0.822 s: 1,216,546 entries/s, 4,136 MB/s\n",
        )
        self.assertEqual(indexbench.parse_verbose(stderr)["walk_s"], 1.234)

    def test_p2_bench_01_a_scan_without_the_lines_is_an_error(self):
        with self.assertRaises(measure.MeasureError):
            indexbench.parse_verbose("duscope: walk: 1 entries, 1 directories, 0 B in 0.001 s: 1 entries/s\n")


class Gates(unittest.TestCase):
    def test_p2_bench_01_each_gate_holds_fails_or_was_not_measured(self):
        got = {
            g["name"]: g
            for g in indexbench.gates({"top_p50_ms": 3.0, "top_p95_ms": 60.0, "dua_speedup_p50": 40.0}, "linux")
        }
        self.assertEqual([g[0] for g in indexbench.GATES], list(got))
        self.assertTrue(got["top_p50_ms"]["holds"])
        self.assertFalse(got["top_p95_ms"]["holds"])
        self.assertTrue(got["dua_speedup_p50"]["holds"])
        self.assertIsNone(got["fdu_speedup_p50"]["holds"])
        self.assertIsNone(got["fdu_speedup_p50"]["value"])

    def test_p2_bench_01_the_gates_are_plan_specs(self):
        limits = {name: (op, limit) for name, op, limit, _ in indexbench.GATES}
        self.assertEqual(limits["write_us_per_entry_warm_p50"], ("<=", 2.75))
        # Recorded, not gated (DEV-108).
        self.assertNotIn("write_share_of_cold_scan_p50", limits)
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


    def test_p2_bench_01_the_warm_write_binds_linux_alone(self):
        values = {"write_us_per_entry_warm_p50": 3.1, "write_share_of_cold_scan_p50": 0.2}
        for platform, holds in (("linux", False), ("macos", None)):
            got = {g["name"]: g for g in indexbench.gates(values, platform)}
            self.assertIs(got["write_us_per_entry_warm_p50"]["holds"], holds, platform)
            self.assertEqual(got["write_us_per_entry_warm_p50"]["value"], 3.1)
            self.assertNotIn("write_share_of_cold_scan_p50", got, platform)

    def test_p2_bench_01_a_subtree_gets_50_ms_per_100k_files(self):
        def subtree(files, ms):
            [gate] = [g for g in indexbench.gates({"by_type_subtree_files": files, "by_type_subtree_p50_ms": ms}, "linux")
                      if g["name"] == "by_type_subtree_p50_ms"]
            return gate["limit"], gate["holds"]

        self.assertEqual(subtree(40_000, 49.0), (50.0, True))
        self.assertEqual(subtree(300_000, 148.0), (150.0, True))
        self.assertEqual(subtree(300_000, 151.0), (150.0, False))


class Shapes(unittest.TestCase):
    def test_p2_qry_10_the_filtered_shapes_are_the_audits_and_select_on_t2(self):
        shapes = dict(indexbench.SHAPES)
        self.assertEqual(len(shapes), len(indexbench.SHAPES), "each shape has a name of its own")
        # The three shapes the 2026-11-09 audit timed on T2 (0.37-0.92 s).
        self.assertEqual(shapes["top_files_depth"], ("top", "--files", "--depth", "3"))
        self.assertEqual(shapes["top_files_min_size"], ("top", "--files", "--min-size", "1M"))
        self.assertEqual(shapes["top_files_apparent"], ("top", "--files", "--size", "apparent"))
        self.assertTrue(all(verb in ("top", "stale") for verb, *_ in shapes.values()))
        # T2's files tie densely and none reaches 1 MiB, so a shape selects
        # by time: `--since` falls inside the generator's modification times
        # and splits them.
        since = shapes["top_files_since"]
        stamp = since[since.index("--since") + 1]
        at = calendar.timegm(time.strptime(stamp, "%Y-%m-%dT%H:%M:%SZ"))
        first, last = trees.BASE_MTIME_S, trees.BASE_MTIME_S + 2 * (10_000 - 1)
        self.assertTrue(first < at < last, (first, at, last))
        self.assertGreater((last - at) / (last - first), 0.5)
        # `stale` asks for a `ctime` before the cutoff too, and the change
        # times are the tree's build: a day ago, not the default 180 days,
        # which would keep nothing on a tree built this year.
        stale = shapes["stale_depth"]
        self.assertEqual(stale[0], "stale")
        self.assertEqual(stale[stale.index("--older-than") + 1], "1d")

    def test_p2_qry_10_a_shape_is_recorded_with_the_rows_it_matched(self):
        answer = [sys.executable, "-c", "print('{\"ok\": true, \"total_count\": 594990}')"]
        self.assertEqual(indexbench.matched(answer, "a shape"), 594_990)
        with self.assertRaises(measure.MeasureError):
            indexbench.matched([sys.executable, "-c", "print('{}')"], "a shape")
        with self.assertRaises(measure.MeasureError):
            indexbench.matched([sys.executable, "-c", "import sys; sys.exit(3)"], "a shape")


class Pty(unittest.TestCase):
    def test_p2_bench_01_dua_runs_on_a_terminal_that_answers(self):
        argv = indexbench.under_pty(["dua", "i", "--once", "--export", "/t/s p", "/t/tree"])
        self.assertEqual(argv[:3], [sys.executable, "-I", "-S"])
        self.assertEqual(os.path.basename(argv[3]), "ptyrun.py")
        self.assertTrue(os.path.isfile(argv[3]))
        self.assertEqual(argv[4:], ["dua", "i", "--once", "--export", "/t/s p", "/t/tree"])

    def test_p2_bench_01_each_cursor_query_is_answered_once(self):
        self.assertEqual(ptyrun.answers(b"\x1b[?25l\x1b[6nframe\x1b[6n"), ([b"\x1b[1;1R"] * 2, b""))
        self.assertEqual(ptyrun.answers(b"text \x1b[2J and \x1b[1;1H"), ([], b""))
        # A query split across two reads is answered when its end arrives.
        found, pending = ptyrun.answers(b"frame\x1b[6")
        self.assertEqual((found, pending), ([], b"\x1b[6"))
        self.assertEqual(ptyrun.answers(pending + b"n"), ([b"\x1b[1;1R"], b""))

    def test_p2_bench_01_a_command_that_asks_where_the_cursor_is_gets_an_answer(self):
        # What dua does first: raw mode, the query, and a read that gives up
        # after two seconds.
        asks = (
            "old=$(stty -g); stty raw -echo min 0 time 20; printf '\\033[6n'; "
            "reply=$(dd bs=16 count=1 2>/dev/null); stty \"$old\"; "
            "case $reply in *R) exit 0 ;; *) echo 'Error: The cursor position could not be read'; exit 1 ;; esac"
        )
        ran = subprocess.run(
            [sys.executable, "-I", "-S", ptyrun.__file__, "sh", "-c", asks],
            stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=30,
        )
        self.assertEqual(ran.returncode, 0, ran.stderr)
        failed = subprocess.run(
            [sys.executable, "-I", "-S", ptyrun.__file__, "sh", "-c", "echo 'Error: no snapshot'; exit 3"],
            stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=30,
        )
        self.assertEqual((failed.returncode, failed.stderr.strip()), (3, "Error: no snapshot"))


if __name__ == "__main__":
    unittest.main()
