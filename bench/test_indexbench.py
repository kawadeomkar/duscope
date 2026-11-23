"""Unit tests of bench/indexbench.py (P2-BENCH-01): reading `scan -v`, the
gates, the filtered shapes (P2-QRY-10), and the pseudo-terminal dua runs on
(bench/ptyrun.py). The whole
subcommand runs in duscope/tests/index_bench.rs, against the binary."""

from __future__ import annotations

import calendar
import os
import shutil
import stat
import subprocess
import sys
import tempfile
import time
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import indexbench  # noqa: E402
import measure  # noqa: E402
import ptyrun  # noqa: E402
import trees  # noqa: E402


class Save(unittest.TestCase):
    """P5-BENCH-04: `scan --save` end to end, and the write per entry on every
    tree, T4's through `import`."""

    WRITE = "duscope: write: 50,001 entries in 0.043 s: 0.87 µs/entry\n"

    def setUp(self) -> None:
        self.tmp = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, self.tmp)

    def test_p5_bench_04_save_runs_the_harness_command_with_save_and_t4_through_import(self):
        t2, t4 = trees.tree("T2", 0.001), trees.tree("T4", 0.001, exports=True)
        self.assertEqual(
            indexbench.save_argv("duscope", t2, "/trees/T2", "/idx"),
            ["duscope", "scan", "--json", "--treemap", "never", "--save", "-v", "--index-dir", "/idx", "/trees/T2"],
        )
        self.assertEqual(indexbench.save_argv("duscope", t4, "/trees/T4.ncdu.json", "/idx"),
                         ["duscope", "import", "/trees/T4.ncdu.json", "--json", "-v", "--index-dir", "/idx"])
        self.assertEqual(trees.built_path(t4, "/trees"), "/trees/T4-x0.001.ncdu.json")
        self.assertEqual(indexbench.SAVE_TREES, "T1,T2,T3,T4")

    def test_p5_bench_04_an_import_prints_the_write_alone(self):
        self.assertEqual(indexbench.parse_write(self.WRITE, export=True),
                         {"entries": 50_001, "write_s": 0.043, "us_per_entry": 0.87, "walk_s": None, "rollup_s": None})
        self.assertEqual(indexbench.parse_write(Verbose.STDERR, export=False)["walk_s"], 1.234)
        with self.assertRaises(measure.MeasureError):
            indexbench.parse_write("duscope: roll-up: 0.001 s\n", export=True)

    def test_p5_bench_04_saves_are_timed_whole_and_each_index_removed(self):
        """A stand-in saves an index and prints the write line; each run is
        timed around the process, the index removed after it, the caches
        dropped before each cold one, and the warm-up runs left out."""
        stub = os.path.join(self.tmp, "duscope")
        drops = os.path.join(self.tmp, "drops")
        with open(stub, "w", encoding="utf-8") as f:
            f.write(
                "#!/bin/sh\n"
                "while [ $# -gt 0 ]; do [ \"$1\" = --index-dir ] && dir=$2; shift; done\n"
                "mkdir -p \"$dir/scans\" && : > \"$dir/scans/abc.db\"\n"
                "echo 'duscope: write: 26 entries in 0.004 s: 150.00 µs/entry' >&2\n"
                "echo '{\"ok\": true}'\n"
            )
        os.chmod(stub, os.stat(stub).st_mode | stat.S_IXUSR)
        index = os.path.join(self.tmp, "index")
        logs = []
        argv = [stub, "import", "T4.ncdu.json", "--json", "-v", "--index-dir", index]
        timed, printed = indexbench.save_runs(argv, index, True, 3, 2, f"echo >> {drops}", logs.append, "T4 cold import")
        self.assertEqual(len(timed), 3)
        self.assertTrue(all(r.wall > 0 and r.rss for r in timed))
        self.assertEqual([p["us_per_entry"] for p in printed], [150.0] * 3)
        self.assertEqual(os.listdir(os.path.join(index, "scans")), [])
        with open(drops, encoding="utf-8") as f:
            self.assertEqual(len(f.read().splitlines()), 5, "a drop before every run, the warm-up ones too")
        self.assertEqual(len(logs), 3)
        os.chmod(stub, 0o755)
        with open(stub, "a", encoding="utf-8") as f:
            f.write("exit 3\n")
        with self.assertRaisesRegex(measure.MeasureError, "exited 3"):
            indexbench.save_runs(argv, index, True, 1, 0, None, logs.append, "T4 warm import")
        self.assertEqual(os.listdir(os.path.join(index, "scans")), [], "a failed run's index is removed too")

    def test_p5_bench_04_a_save_row_holds_the_runs_and_the_figures_read_it(self):
        runs = [measure.Run(w, 0.1, 0.2, 1000) for w in (2.0, 1.0, 3.0)]
        printed = [{"entries": 1_000_001, "write_s": s, "us_per_entry": s * 1.0, "walk_s": 0.3, "rollup_s": 0.05}
                   for s in (1.6, 1.7, 1.65)]
        row = indexbench.save_row(trees.tree("T2"), "warm", runs, printed)
        self.assertEqual((row["tool"], row["p50_s"], row["run_count"], row["entries"]), ("scan --save", 2.0, 3, 1_000_001))
        self.assertEqual((row["write_us_per_entry_p50"], row["write_s_p50"], row["walk_s_p50"]), (1.65, 1.65, 0.3))
        self.assertEqual(row["command"], indexbench.SAVE_COMMANDS["scan --save"])
        imported = indexbench.save_row(trees.tree("T4", exports=True), "cold", runs,
                                       [dict(p, walk_s=None, rollup_s=None) for p in printed])
        self.assertEqual((imported["tool"], imported["walk_s_p50"], imported["rollup_s_p50"]), ("import", None, None))
        self.assertEqual(indexbench.figures([row, imported]), {
            "T2": {"save_p50_s": {"warm": 2.0}, "write_us_per_entry_p50": {"warm": 1.65}},
            "T4": {"save_p50_s": {"cold": None}, "write_us_per_entry_p50": {"cold": 1.65}},
        })


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


    def test_p2_bench_01_the_gates_are_judged_on_t2_alone(self):
        def tree(name, top_p50_ms):
            return {"files": 1, "gate_values": {"top_p50_ms": top_p50_ms}}

        for names in (["T2"], ["T3", "T2"], ["T1", "T2", "T3"]):
            per_tree = [tree(name, {"T1": 30.0, "T2": 3.0, "T3": 0.4}[name]) for name in names]
            judged, values = indexbench.judged(names, per_tree)
            self.assertEqual(judged, "T2")
            self.assertEqual(values, {"top_p50_ms": 3.0})
            # Taken out of every tree's figures, T2's or not.
            self.assertTrue(all("gate_values" not in found for found in per_tree), names)
            [line] = [said for said in indexbench.verdicts(judged, indexbench.gates(values, "linux"))
                      if said.startswith("gate top_p50_ms ")]
            self.assertEqual(line, "gate top_p50_ms <= 20: 3 (holds)")
        # T1's figures, or T3's one folder of a million files, are not the
        # targets': a run without T2 judges nothing, rather than holding or
        # failing on them.
        for names in (["T3"], ["T1"], ["T1", "T3"], []):
            per_tree = [tree(name, 0.4) for name in names]
            judged, values = indexbench.judged(names, per_tree)
            self.assertIsNone(judged, names)
            self.assertEqual(values, {}, names)
            self.assertTrue(all("gate_values" not in found for found in per_tree), names)
            lines = indexbench.gates(values, "linux")
            self.assertTrue(all(gate["holds"] is None for gate in lines), names)
            said = indexbench.verdicts(judged, lines)
            self.assertEqual(len(said), len(indexbench.GATES))
            self.assertTrue(all(line.endswith(": not judged, T2 was not measured") for line in said), said)
        # T2 measured: a gate it did not measure, one that binds another
        # platform, one that fails.
        judged, values = indexbench.judged(["T2"], [{"gate_values": {"top_p95_ms": 60.0, "write_us_per_entry_warm_p50": 3.1}}])
        said = {line.split(":")[0]: line for line in indexbench.verdicts(judged, indexbench.gates(values, "macos"))}
        self.assertEqual(said["gate top_p95_ms <= 50"], "gate top_p95_ms <= 50: 60 (FAILS)")
        self.assertEqual(said["gate top_p50_ms <= 20"], "gate top_p50_ms <= 20: not measured (-)")
        self.assertEqual(
            said["gate write_us_per_entry_warm_p50 <= 2.75"],
            "gate write_us_per_entry_warm_p50 <= 2.75: 3.1 (binds linux only)",
        )


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
