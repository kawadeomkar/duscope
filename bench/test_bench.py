"""Unit tests of the harness modules; duscope/tests/bench_harness.rs runs them as
`p0_bench_01_python_unit_tests`:

    python3 -B -m unittest discover -s bench -p 'test_*.py'
"""

from __future__ import annotations

import contextlib
import hashlib
import io
import json
import math
import os
import re
import shlex
import statistics
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

import harness
import measure
import sweep
import tools
import trees

HERE = os.path.dirname(os.path.abspath(__file__))
EXPORT = os.path.join(HERE, "testdata", "hyperfine-2.0.0-export.json")
STUB = os.path.join(HERE, "testdata", "hyperfine_stub.py")


class Percentiles(unittest.TestCase):
    def test_p0_bench_01_percentiles_interpolate_between_ranks(self):
        values = [5.0, 1.0, 4.0, 2.0, 3.0]
        self.assertEqual(measure.percentile(values, 0.5), 3.0)
        self.assertAlmostEqual(measure.percentile(values, 0.95), 4.8)
        self.assertEqual(measure.percentile(values, 0.0), 1.0)
        self.assertEqual(measure.percentile(values, 1.0), 5.0)
        self.assertEqual(measure.percentile([2.0, 4.0], 0.5), 3.0)
        self.assertEqual(measure.percentile([7.0], 0.95), 7.0)
        # Ten runs, the harness default: p95 lies 55% of the way from the 9th to the 10th.
        ten = [float(i) for i in range(1, 11)]
        self.assertAlmostEqual(measure.percentile(ten, 0.95), 9.55)
        self.assertAlmostEqual(measure.percentile(ten, 0.5), 5.5)


class HyperfineExport(unittest.TestCase):
    def test_p0_bench_01_reads_a_hyperfine_2_export(self):
        runs = measure.read_export(EXPORT)
        with open(EXPORT, encoding="utf-8") as f:
            raw = json.load(f)
        self.assertEqual(sorted(runs), ["du", "ls"])
        for result in raw["results"]:
            got = runs[result["name"]]
            want = [m["time_wall_clock"]["value"] for m in result["measurements"]]
            self.assertEqual([r.wall for r in got], want)
            self.assertEqual([r.rss for r in got], [int(m["memory_peak_resident"]["value"]) for m in result["measurements"]])
            self.assertEqual([r.user for r in got], [m["time_user"]["value"] for m in result["measurements"]])

        row = measure.row("T1", "warm", "du", "du -sk {tree}", runs["du"])
        walls = sorted(r.wall for r in runs["du"])
        self.assertEqual(row["run_count"], 5)
        self.assertAlmostEqual(row["p50_s"], walls[2], places=9)
        self.assertAlmostEqual(row["p95_s"], walls[3] + 0.8 * (walls[4] - walls[3]), places=9)
        self.assertEqual(row["min_s"], round(walls[0], 9))
        self.assertEqual(row["max_s"], round(walls[-1], 9))
        self.assertEqual(row["peak_rss_bytes"], max(r.rss for r in runs["du"]))
        self.assertIsNone(row["paired_with"])
        self.assertIsNone(row["subject_ratio_p50"])

    def test_p0_bench_01_rejects_other_export_versions(self):
        with tempfile.TemporaryDirectory() as scratch:
            path = os.path.join(scratch, "old.json")
            with open(path, "w", encoding="utf-8") as f:
                json.dump({"results": [{"command": "du", "times": [1.0]}]}, f)
            with self.assertRaisesRegex(measure.MeasureError, "schema_version"):
                measure.read_export(path)

    def test_p0_bench_01_stub_writes_the_real_export_shape(self):
        """The stub's export has every key the harness reads, where the real one has it."""
        with tempfile.TemporaryDirectory() as scratch:
            out = os.path.join(scratch, "stub.json")
            subprocess.run(
                [sys.executable, STUB, "-N", "--runs", "2", "--warmup", "0", "--style", "none",
                 "--metrics", measure.METRICS, "--export-json", out,
                 "--command-name", "true", "true"],
                check=True,
            )
            with open(out, encoding="utf-8") as f:
                stub = json.load(f)
            self.assertEqual(len(measure.read_export(out)["true"]), 2)
        with open(EXPORT, encoding="utf-8") as f:
            real = json.load(f)
        self.assertEqual(stub["schema_version"], real["schema_version"])
        stub_m = stub["results"][0]["measurements"][0]
        real_m = real["results"][0]["measurements"][0]
        self.assertTrue(set(stub["results"][0]) <= set(real["results"][0]))
        for key in stub_m:
            self.assertIn(key, real_m)
            if isinstance(real_m[key], dict):
                self.assertEqual(set(stub_m[key]), set(real_m[key]), key)


class Pairs(unittest.TestCase):
    def test_p0_bench_01_pairs_alternate_which_runs_first(self):
        orders = [measure.pair_order("du", "diskus", r) for r in range(4)]
        self.assertEqual(orders, [("du", "diskus"), ("diskus", "du"), ("du", "diskus"), ("diskus", "du")])

    def test_p0_bench_01_ratio_is_the_median_of_paired_ratios(self):
        subject = [measure.Run(w, None, None, None) for w in (2.0, 3.0, 9.0)]
        other = [measure.Run(w, None, None, None) for w in (1.0, 1.0, 1.0)]
        row = measure.row("T1", "warm", "diskus", "diskus {tree}", other, "du", subject)
        self.assertEqual(row["subject_ratio_p50"], 3.0)
        self.assertEqual(row["subject_times_s"], [2.0, 3.0, 9.0])
        self.assertIsNone(row["user_s"])
        self.assertIsNone(row["peak_rss_bytes"])


class Trees(unittest.TestCase):
    def test_p0_bench_01_full_trees_have_the_specified_counts(self):
        t1, t2, t3 = trees.tree("T1"), trees.tree("T2"), trees.tree("T3")
        self.assertEqual((t1.dirs, t1.files), (100_000, 400_000))
        self.assertEqual((t2.dirs, t2.files), (125_000, 875_000))
        self.assertEqual((t3.dirs, t3.files), (0, 1_000_000))
        self.assertEqual([t.dirname for t in (t1, t2, t3)], ["T1", "T2", "T3"])
        self.assertEqual(trees.tree("T1", 0.001).dirname, "T1-x0.001")
        with self.assertRaises(trees.TreeError):
            trees.tree("T1", 0)
        with self.assertRaises(trees.TreeError):
            trees.tree("T4")

    def test_p0_bench_01_t2_follows_fdu_balanced_corpus(self):
        """Values read from a tree fdu 0.5.0's own generator built
        (explorations/benchmarks: `generate create --recipe balanced`)."""
        t2 = trees.tree("T2")
        dirs = trees.directory_paths(trees.tree("T2", 0.002))
        self.assertEqual(dirs[1], "d-0000000-fc371f")
        self.assertEqual(dirs[9], "d-0000000-fc371f/d-0000008-986cf6")
        self.assertEqual(dirs[73], "d-0000000-fc371f/d-0000008-986cf6/d-0000072-17d786")
        self.assertEqual(trees.mtime_ns(t2.spec.seed, dirs[1]), 1_700_001_892_000_000_000)
        self.assertEqual(trees.mtime_ns(t2.spec.seed, dirs[9]), 1_700_015_832_000_000_000)
        self.assertEqual(trees.mtime_ns(t2.spec.seed, dirs[73]), 1_700_000_036_000_000_000)
        # The parent of directory i is directory i // 8, as in fdu's balanced topology.
        full = trees.directory_paths(trees.tree("T2", 0.01))
        for i in (0, 7):
            self.assertNotIn("/", full[i + 1])
        for i in (8, 63, 64, 1000):
            self.assertEqual(full[i + 1].rsplit("/", 1)[0], full[i // 8])

    def test_p0_bench_01_t1_file_sizes_are_sparse(self):
        t = trees.tree("T1", 0.001)
        dirs = trees.directory_paths(t)
        entries = [trees.file_entry(t, dirs, j) for j in range(t.files)]
        self.assertTrue(all(data is None for _, _, data in entries))
        self.assertTrue({size for _, size, _ in entries} <= set(trees.T1_SIZES))
        self.assertGreater(len({size for _, size, _ in entries}), 4)

    def test_p0_bench_01_build_and_verify_round_trip(self):
        with tempfile.TemporaryDirectory() as scratch:
            for name in trees.SPECS:
                t = trees.tree(name, 0.0003)
                stamp = trees.build(t, scratch, jobs=1)
                observed, expected = trees.verify(t, scratch)
                self.assertEqual(observed, expected, name)
                self.assertEqual(stamp["digest"], expected)
                # A matching stamp means no rebuild.
                again = trees.build(t, scratch, jobs=1)
                self.assertEqual(again["built_at"], stamp["built_at"])
                listing = list(trees.walk(trees.tree_root(t, scratch)))
                self.assertEqual(sum(k == "d" for k, *_ in listing), t.dirs + 1)
                self.assertEqual(sum(k == "f" for k, *_ in listing), t.files)
            # A changed mtime changes the digest.
            t = trees.tree("T1", 0.0003)
            root = trees.tree_root(t, scratch)
            victim = next(r for k, _, _, r in trees.walk(root) if k == "f")
            os.utime(os.path.join(root, victim), ns=(0, 0))
            observed, expected = trees.verify(t, scratch)
            self.assertNotEqual(observed, expected)

    def test_p0_bench_01_rebuild_keeps_the_old_tree_where_sparse_files_fail(self):
        """The sparse probe runs before the previous build is removed."""
        with tempfile.TemporaryDirectory() as scratch:
            t = trees.tree("T1", 0.0003)
            trees.build(t, scratch, jobs=1)
            refusal = trees.TreeError("this filesystem does not keep sparse files sparse")
            with mock.patch.object(trees, "_check_sparse", side_effect=refusal):
                with self.assertRaises(trees.TreeError):
                    trees.build(t, scratch, jobs=1, force=True)
            observed, expected = trees.verify(t, scratch)
            self.assertEqual(observed, expected)

    def test_p0_bench_01_parallel_build_equals_serial(self):
        with tempfile.TemporaryDirectory() as scratch:
            t = trees.tree("T3", 0.025)  # 25,000 files: above PARALLEL_MIN_FILES
            serial = trees.build(t, os.path.join(scratch, "a"), jobs=1)
            parallel = trees.build(t, os.path.join(scratch, "b"), jobs=3)
            self.assertEqual(serial["digest"], parallel["digest"])
            self.assertEqual(parallel["jobs"], 3)


class T4(unittest.TestCase):
    """P5-BENCH-04: T4, the generated scan with log-normal sizes."""

    SEED = trees.EXPORTS["T4"].seed

    def test_p5_bench_04_t4_has_t2s_layout_and_is_named_only_where_it_is_read(self):
        t4, t2 = trees.tree("T4", exports=True), trees.tree("T2")
        self.assertEqual((t4.dirs, t4.files, t4.entries), (t2.dirs, t2.files, 1_000_000))
        self.assertTrue(t4.export)
        self.assertFalse(t2.export)
        self.assertNotEqual(t4.spec.seed, t2.spec.seed)
        small = trees.tree("T4", 0.001, exports=True)
        self.assertEqual(small.dirname, "T4-x0.001")
        paths = trees.directory_paths(small)
        self.assertEqual(paths[9], f"{paths[1]}/{paths[9].rsplit('/', 1)[-1]}")  # directory 8's parent is directory 0
        # `all` is the trees on disk; T4 is taken by name, where `exports` allows it.
        with self.assertRaises(trees.TreeError):
            trees.tree("T4")
        args = harness.parser().parse_args(["save", "--duscope", "d", "--trees", "all"])
        self.assertEqual([t.name for t in harness._trees(args, exports=True)], ["T1", "T2", "T3"])
        args = harness.parser().parse_args(["save", "--duscope", "d"])
        self.assertEqual([t.name for t in harness._trees(args, exports=True)], ["T1", "T2", "T3", "T4"])
        with self.assertRaises(harness.UsageError):
            harness._trees(harness.parser().parse_args(["run", "--trees", "T2,T4"]))

    def test_p5_bench_04_t4_sizes_follow_a_log_normal_spread(self):
        # Pinned: `decimal` gives these on every platform and every Python.
        self.assertEqual([trees.lognormal_size(self.SEED, i) for i in range(6)], [4792, 26519, 450292, 1136, 169, 445])
        sizes = [trees.lognormal_size(self.SEED, i) for i in range(5_000)]
        logs = [math.log(s) for s in sizes if s > 0]
        self.assertAlmostEqual(statistics.median(sizes) / trees.T4_MEDIAN, 1, delta=0.1)
        self.assertAlmostEqual(statistics.pstdev(logs), float(trees.T4_SIGMA), delta=0.1)
        allocated = [trees.allocated_size(s) for s in sizes]
        self.assertTrue(all(a % trees.T4_BLOCK == 0 and s <= a < s + trees.T4_BLOCK for s, a in zip(sizes, allocated)))
        self.assertEqual(trees.allocated_size(0), 0)
        # Where T2's allocated sizes take two values, T4's take hundreds even here.
        self.assertGreater(len(set(allocated)), 200)

    def test_p5_bench_04_t4_is_byte_identical_across_two_generations_from_one_seed(self):
        with tempfile.TemporaryDirectory() as scratch:
            t = trees.tree("T4", 0.03, exports=True)  # 26,250 files: above PARALLEL_MIN_FILES
            serial = trees.build(t, os.path.join(scratch, "a"), jobs=1)
            again = trees.build(t, os.path.join(scratch, "b"), jobs=1)
            parallel = trees.build(t, os.path.join(scratch, "c"), jobs=3)
            files = [trees.export_path(t, os.path.join(scratch, d)) for d in "abc"]
            with open(files[0], "rb") as f:
                first = f.read()
            for other in files[1:]:
                with open(other, "rb") as f:
                    self.assertEqual(f.read(), first, other)
            self.assertEqual({serial["digest"], again["digest"], parallel["digest"]}, {trees.file_digest(files[0])})
            self.assertEqual(serial["apparent_bytes"], parallel["apparent_bytes"])
            # The export names no path of the machine that made it.
            self.assertNotIn(scratch.encode(), first)
        # A small one, pinned: duscope/tests/main_path_bench.rs pins it too.
        with tempfile.TemporaryDirectory() as scratch:
            stamp = trees.build(trees.tree("T4", 0.0002, exports=True), scratch, jobs=1)
            self.assertEqual((stamp["digest"], stamp["apparent_bytes"]), ("57095a3efa018889f8593d2855b86c5a", 5999273))

    def test_p5_bench_04_t4_is_an_ncdu_export_of_its_entries(self):
        with tempfile.TemporaryDirectory() as scratch:
            t = trees.tree("T4", 0.001, exports=True)
            stamp = trees.build(t, scratch, jobs=1)
            with open(trees.export_path(t, scratch), encoding="utf-8") as f:
                major, minor, meta, root = json.load(f)
            self.assertEqual((major, minor, meta["timestamp"]), (1, 2, trees.T4_SCANNED_AT_S))
            counts = {"dirs": 0, "files": 0, "apparent": 0}
            stack = [root]
            while stack:
                node = stack.pop()
                info, *children = node
                counts["dirs"] += 1
                for child in children:
                    if isinstance(child, list):
                        stack.append(child)
                    else:
                        self.assertEqual(child["dsize"], trees.allocated_size(child["asize"]))
                        self.assertLessEqual(child["mtime"], trees.T4_SCANNED_AT_S)
                        counts["files"] += 1
                        counts["apparent"] += child["asize"]
            self.assertEqual(root[0]["name"], trees.T4_ROOT)
            self.assertEqual((counts["dirs"], counts["files"], counts["apparent"]), (t.dirs + 1, t.files, stamp["apparent_bytes"]))

    def test_p5_bench_04_t4_is_rebuilt_when_its_file_changes_and_alone(self):
        with tempfile.TemporaryDirectory() as scratch:
            t4 = trees.tree("T4", 0.0002, exports=True)
            t2 = trees.tree("T2", 0.0002)
            stamp, kept = trees.build(t4, scratch, jobs=1), trees.build(t2, scratch, jobs=1)
            self.assertEqual(trees.build(t4, scratch, jobs=1)["built_at"], stamp["built_at"])
            self.assertEqual(trees.verify(t4, scratch), (stamp["digest"], stamp["digest"]))
            with open(trees.export_path(t4, scratch), "a", encoding="utf-8") as f:
                f.write(" ")
            observed, expected = trees.verify(t4, scratch)
            self.assertNotEqual(observed, expected)
            rebuilt = trees.build(t4, scratch, jobs=1)
            self.assertEqual(rebuilt["digest"], stamp["digest"])
            self.assertEqual(trees.read_stamp(t2, scratch), kept)
            # A stamp of another export version is no stamp of this one.
            with mock.patch.object(trees, "EXPORT_VERSION", trees.EXPORT_VERSION + 1):
                self.assertIsNone(trees.read_stamp(t4, scratch))
                self.assertEqual(trees.read_stamp(t2, scratch), kept)
            os.remove(trees.export_path(t4, scratch))
            self.assertIsNone(trees.read_stamp(t4, scratch))


class Tools(unittest.TestCase):
    def test_p0_bench_01_pins_are_well_formed(self):
        self.assertEqual(
            sorted(tools.COMPETITORS),
            sorted(["diskus", "dua", "pdu", "dust", "gdu", "ncdu", "fdu", "dumac"]),
        )
        for tool in tools.PINS.values():
            self.assertIn("{tree}", tool.args, tool.name)
            for key, release in tool.releases.items():
                self.assertIn(key, tools.PLATFORMS)
                self.assertTrue(
                    release.url.startswith(("https://github.com/", "https://dev.yorhel.nl/download/")),
                    release.url,
                )
                self.assertRegex(release.sha256, r"^[0-9a-f]{64}$")
                self.assertIn(str(tool.pinned), release.url)
            if tool.cargo and tool.cargo.git:
                self.assertRegex(str(tool.cargo.rev), r"^[0-9a-f]{40}$")
                self.assertEqual(tool.cargo.rev, tool.pinned)
            elif tool.cargo:
                self.assertEqual(tool.cargo.version, tool.pinned)
            for key in tools.PLATFORMS:
                source, reason = tools.source_for(tool, key)
                self.assertEqual(source == "skipped", reason is not None)

    def test_p0_bench_01_every_platform_has_du_and_diskus(self):
        for key in tools.PLATFORMS:
            self.assertEqual(tools.source_for(tools.PINS["du"], key)[0], "system")
            self.assertEqual(tools.source_for(tools.PINS["diskus"], key)[0], "release")
        self.assertEqual(tools.source_for(tools.PINS["dumac"], "linux-x86_64")[0], "skipped")

    def test_p0_bench_01_downloads_send_no_identity(self):
        self.assertEqual(tools.USER_AGENT, "duscope-bench")
        self.assertNotRegex(tools.USER_AGENT, r"@|https?:")

    def test_p0_bench_01_release_install_checks_the_digest(self):
        with tempfile.TemporaryDirectory() as scratch:
            payload = b"#!/bin/sh\necho fake 1.0\n"
            asset = os.path.join(scratch, "fake")
            with open(asset, "wb") as f:
                f.write(payload)
            good = hashlib.sha256(payload).hexdigest()
            tool = tools.Tool("fake", "1.0", ("fake", "{tree}"),
                              releases={"any": tools.Release("file://" + asset, good)})
            downloads = os.path.join(scratch, "tools", "downloads")
            os.makedirs(downloads)
            # A cached download with the right digest is used without curl.
            with open(os.path.join(downloads, "fake"), "wb") as f:
                f.write(payload)
            path = tools._install_release(tool, tool.releases["any"], os.path.join(scratch, "tools"), lambda m: None)
            self.assertEqual(tools.check_version(tool, path), "fake 1.0")
            bad = tools.Tool("fake", "1.0", ("fake", "{tree}"),
                             releases={"any": tools.Release("file://" + asset, "0" * 64)})
            with self.assertRaisesRegex(tools.ToolError, "not the pinned"):
                tools._install_release(bad, bad.releases["any"], os.path.join(scratch, "tools2"), lambda m: None)


class Harness(unittest.TestCase):
    def test_p0_bench_01_names_and_pairs_parse(self):
        self.assertEqual(harness._names("all", trees.SPECS, "tree"), ["T1", "T2", "T3"])
        self.assertEqual(harness._names("T2,T1,T2", trees.SPECS, "tree"), ["T2", "T1"])
        self.assertEqual(harness._names("none", tools.PINS, "tool", allow_none=True), [])
        with self.assertRaises(harness.UsageError):
            harness._names("T9", trees.SPECS, "tree")
        self.assertEqual(harness._pairs(["a=b c", "d=e=f"], "--command"), {"a": "b c", "d": "e=f"})
        with self.assertRaises(harness.UsageError):
            harness._pairs(["nope"], "--command")

    def test_p0_bench_01_host_facts(self):
        facts = harness.host()
        self.assertIsInstance(facts["logical_cpus"], int)
        self.assertTrue(re.match(r"^(Darwin|Linux) ", str(facts["kernel"])))
        self.assertIn(harness.platform_name(), ("macos", "linux"))

    def test_p0_bench_02_cold_runs_measure_their_own_trees_and_tools(self):
        """--cold-trees-dir and --cold-competitors give the cold runs their own trees and tools."""
        with tempfile.TemporaryDirectory() as scratch:
            argv = ["run", "--trees", "T3", "--scale", "0.0005", "--jobs", "1", "--subject", "du"]
            # Stand-ins for two pinned tools: each reports its pin and exits 0.
            for name in ("diskus", "dua"):
                fake = os.path.join(scratch, name)
                with open(fake, "w", encoding="utf-8") as f:
                    f.write(f"#!/bin/sh\necho '{name} {tools.PINS[name].pinned}'\n")
                os.chmod(fake, 0o755)
                argv += ["--tool-path", f"{name}={fake}"]
            warm, cold, results = (os.path.join(scratch, d) for d in ("warm", "cold", "results"))
            argv += [
                "--competitors", "diskus,dua", "--cold-competitors", "dua",
                "--trees-dir", warm, "--cold-trees-dir", cold,
                "--mode", "both", "--cold-cmd", "true", "--runs", "1", "--warmup", "0",
                "--hyperfine", f"{shlex.quote(sys.executable)} {shlex.quote(STUB)}",
                "--tools-dir", os.path.join(scratch, "tools"), "--results-dir", results,
            ]
            smoked = []
            smoke = measure.smoke

            def record(tool, path):
                smoked.append((tool.tool.name, os.path.dirname(path)))
                smoke(tool, path)

            with mock.patch.object(measure, "smoke", record), \
                    contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(harness.main(argv), 0)
            # Every tool runs on the trees its mode reads, and only there.
            self.assertEqual(
                sorted(smoked),
                [("diskus", warm), ("du", cold), ("du", warm), ("dua", cold), ("dua", warm)],
            )
            for d in (warm, cold):
                self.assertTrue(os.path.isfile(trees.stamp_path(trees.tree("T3", 0.0005), d)), d)
            [name] = os.listdir(results)
            with open(os.path.join(results, name), encoding="utf-8") as f:
                result = json.load(f)
            self.assertEqual(
                sorted((r["mode"], r["tool"]) for r in result["results"]),
                [("cold", "du"), ("cold", "dua"), ("warm", "diskus"), ("warm", "du"), ("warm", "dua")],
            )
            self.assertEqual(result["harness"]["cold_competitors"], ["dua"])
            self.assertEqual(result["trees"][0]["filesystem"], result["trees"][0]["cold_filesystem"])
            self.assertIsNotNone(result["trees"][0]["cold_filesystem"])



class Sweep(unittest.TestCase):
    @staticmethod
    def rows(p50s, tree="T1", mode="warm"):
        return [{"tree": tree, "mode": mode, "threads": n, "p50_s": p50} for n, p50 in p50s.items()]

    def test_p1_bench_01_the_rule_takes_the_smallest_count_near_the_best(self):
        # Best 0.39 s at 16; 4 is within 5% (0.4 <= 0.4095), and is a target.
        rows = self.rows({1: 1.0, 2: 0.6, 4: 0.4, 6: 0.41, 8: 0.405, 16: 0.39})
        self.assertEqual(sweep.default_threads(rows), 4)
        # Rows of other trees and modes do not count.
        rows += self.rows({1: 0.1}, tree="T3") + self.rows({1: 0.1}, mode="cold")
        self.assertEqual(sweep.default_threads(rows), 4)

    def test_p1_bench_01_the_count_rounds_to_4_8_16_or_32(self):
        def chosen(smallest):
            return sweep.default_threads(self.rows({smallest: 1.0, 64: 1.0}))
        self.assertEqual([chosen(n) for n in (1, 5, 6, 7, 12, 13, 24, 25, 48)], [4, 4, 4, 8, 8, 16, 16, 32, 32])

    def test_p1_bench_01_no_warm_t1_rows_give_no_default(self):
        self.assertIsNone(sweep.default_threads(self.rows({4: 1.0}, mode="cold")))

    def test_p1_bench_01_cold_rows_read_their_own_trees(self):
        """--cold-trees-dir gives the cold rows their own trees, as in `run`."""
        with tempfile.TemporaryDirectory() as scratch:
            # A stand-in for duscope: it names itself and exits 0.
            duscope = os.path.join(scratch, "duscope")
            with open(duscope, "w", encoding="utf-8") as f:
                f.write("#!/bin/sh\necho 'duscope 0.0.1'\n")
            os.chmod(duscope, 0o755)
            warm, cold, results = (os.path.join(scratch, d) for d in ("warm", "cold", "results"))
            argv = [
                "sweep", "--duscope", duscope, "--trees", "T3", "--scale", "0.0005", "--jobs", "1",
                "--threads", "1,2", "--trees-dir", warm, "--cold-trees-dir", cold,
                "--mode", "both", "--cold-cmd", "true", "--runs", "1", "--warmup", "0",
                "--hyperfine", f"{shlex.quote(sys.executable)} {shlex.quote(STUB)}",
                "--results-dir", results,
            ]
            smoked = []
            smoke = measure.smoke

            def record(tool, path):
                smoked.append((tool.tool.name, os.path.dirname(path)))
                smoke(tool, path)

            with mock.patch.object(measure, "smoke", record), \
                    contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(harness.main(argv), 0)
            # Each thread count runs on the trees its mode reads, and only there.
            self.assertEqual(
                sorted(smoked),
                [("duscope-t1", cold), ("duscope-t1", warm), ("duscope-t2", cold), ("duscope-t2", warm)],
            )
            for d in (warm, cold):
                self.assertTrue(os.path.isfile(trees.stamp_path(trees.tree("T3", 0.0005), d)), d)
            [name] = os.listdir(results)
            with open(os.path.join(results, name), encoding="utf-8") as f:
                result = json.load(f)
            self.assertEqual(
                sorted((r["mode"], r["threads"]) for r in result["results"]),
                [("cold", 1), ("cold", 2), ("warm", 1), ("warm", 2)],
            )
            self.assertEqual(result["trees"][0]["filesystem"], result["trees"][0]["cold_filesystem"])
            self.assertIsNotNone(result["trees"][0]["cold_filesystem"])


if __name__ == "__main__":
    unittest.main()
