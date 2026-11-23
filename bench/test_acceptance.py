"""Unit tests of the 1.0 acceptance run (P5-BENCH-01): the targets, the
judgement and the summary (bench/acceptance.py), and the incremental rescans
(bench/rescanbench.py). duscope/tests/acceptance_bench.rs runs them as
`p5_bench_01_python_unit_tests`, and runs `bench/run.sh acceptance` whole
against the binary:

    python3 -B -m unittest discover -s bench -p 'test_acceptance.py'
"""

from __future__ import annotations

import contextlib
import io
import json
import os
import shutil
import stat
import tempfile
import unittest
from typing import Dict, List, Optional, Tuple
from unittest import mock

import acceptance
import harness
import measure
import rescanbench
import tools
import trees

Pair = Tuple[str, str, str]  # tree, mode, competitor


def run_doc(ratios: Dict[Pair, float], subject: str = "duscope", fs: str = "ext4", scale: float = 1.0,
            tools: Optional[List[dict]] = None, cpu: str = "Intel(R) Xeon(R)", cpus: int = 4,
            started_at: str = "2026-11-20T10:00:00Z", digest: str = "d", commit: str = "c0ffee") -> dict:
    """A harness result with each ratio as a competitor's paired entry,
    trimmed to the fields the judge and the cross-check read."""
    return {
        "schema_version": 1,
        "started_at": started_at,
        "platform": "linux",
        "subject": subject,
        "host": {"arch": "x86_64", "cpu": cpu, "logical_cpus": cpus},
        "harness": {"commit": commit},
        "trees": [
            {"name": t, "scale": scale, "digest": f"{digest}-{t}", "filesystem": fs, "cold_filesystem": None}
            for t in acceptance.TREES
        ],
        "tools": tools if tools is not None else [
            {"name": c, "status": "measured", "reason": None} for c in sorted({c for _, _, c in ratios})
        ],
        "results": [
            {"tree": t, "mode": m, "tool": c, "paired_with": subject, "p50_s": 1.0, "subject_ratio_p50": v}
            for (t, m, c), v in ratios.items()
        ],
    }


def doc(platform: str, ratios: Dict[Pair, float], write: Optional[float] = None, **run_kwargs) -> dict:
    """An acceptance file of `platform`."""
    run = run_doc(ratios, fs="ext4" if platform == "linux" else "apfs", **run_kwargs)
    run["platform"] = platform
    index = None
    if write is not None:
        index = {
            "gates_tree": "T2",
            "gates": [{"name": "write_us_per_entry_warm_p50", "value": write, "limit": 2.75}],
            "trees": [{"name": "T2", "write": {"cold": {"share_of_scan": {"p50": 1.22}}}}],
        }
    return {
        "schema_version": 1,
        "kind": "acceptance",
        "date": "2026-11-20",
        "platform": platform,
        "host": {"os": "an OS", "arch": "x86_64" if platform == "linux" else "arm64", "cpu": "a CPU", "logical_cpus": 10},
        "harness": {"commit": "0123456789abcdef", "scale": 1.0},
        "duscope": {"version": "duscope 1.0.0"},
        "run": run,
        "index": index,
        "rescan": None,
        "failures": [],
    }


def all_pairs(value: float) -> Dict[str, Dict[Pair, float]]:
    """Every pair each platform's targets read, at `value`."""
    out: Dict[str, Dict[Pair, float]] = {"linux": {}, "macos": {}}
    for t in acceptance.TARGETS:
        for tree in t.trees:
            for c in t.against:
                out[t.platform][(tree, t.mode, c)] = value
    return out


class Targets(unittest.TestCase):
    def test_p5_bench_01_the_targets_are_the_plans_goals_1_and_2(self):
        # plan/PROPOSAL.md, goals 1 and 2 and the 1.0 acceptance targets (1),
        # (2) and (3), with DEV-107's write limit.
        got = [(t.goal, t.platform, t.mode, t.against, t.limit, t.trees) for t in acceptance.TARGETS]
        every = ("T1", "T2", "T3")
        self.assertEqual(got, [
            (1, "linux", "warm", ("diskus",), 1.2, every),
            (1, "linux", "warm", ("pdu", "fdu"), 1.15, every),
            (1, "linux", "cold", ("diskus",), 1.15, every),
            (1, "macos", "warm", ("diskus",), 1.0, every),
            (1, "macos", "warm", ("dua",), 1.2, every),
            (1, "macos", "warm", ("dumac",), 1.3, every),
            (1, "macos", "cold", ("dua",), 1.2, every),
            (2, "linux", "warm", (), 2.75, ("T2",)),
        ])
        for t in acceptance.TARGETS:
            self.assertIn(f"plan/PROPOSAL.md goal {t.goal} and 1.0 acceptance target (", t.source)
            self.assertIn(f"{t.limit:g}", t.what)
        self.assertIn("DEV-107", acceptance.TARGETS[-1].source)
        self.assertIn("DEV-52", acceptance.TARGETS[3].source)


class Judge(unittest.TestCase):
    def verdict(self, d: Optional[dict], index: int, tree: str = "T1") -> acceptance.Verdict:
        return acceptance.judge_one(acceptance.TARGETS[index], tree, d)

    def test_p5_bench_01_a_ratio_at_its_limit_is_met_and_above_it_missed(self):
        for value, status in ((1.2, "met"), (0.35, "met"), (1.2001, "MISSED")):
            v = self.verdict(doc("linux", {("T1", "warm", "diskus"): value}), 0)
            self.assertEqual((v.value, v.status), (value, status))
        # The cold target reads the cold pair only.
        v = self.verdict(doc("linux", {("T1", "warm", "diskus"): 0.3, ("T1", "cold", "diskus"): 1.16}), 2)
        self.assertEqual((v.value, v.status), (1.16, "MISSED"))

    def test_p5_bench_01_the_faster_of_pdu_and_fdu_gives_the_larger_ratio(self):
        v = self.verdict(doc("linux", {("T1", "warm", "pdu"): 0.22, ("T1", "warm", "fdu"): 1.096}), 1)
        self.assertEqual((v.value, v.status, v.note), (1.096, "met", "against fdu, the faster"))
        v = self.verdict(doc("linux", {("T3", "warm", "pdu"): 0.297, ("T3", "warm", "fdu"): 0.194}), 1, "T3")
        self.assertEqual((v.value, v.note), (0.297, "against pdu, the faster"))
        # One of the two not measured: the other is judged, and the note says why.
        tools = [{"name": "pdu", "status": "measured", "reason": None},
                 {"name": "fdu", "status": "failed", "reason": "cargo install failed"}]
        v = self.verdict(doc("linux", {("T1", "warm", "pdu"): 1.2}, tools=tools), 1)
        self.assertEqual((v.value, v.status), (1.2, "MISSED"))
        self.assertIn("fdu failed: cargo install failed", v.note)

    def test_p5_bench_01_what_was_not_measured_says_why(self):
        self.assertEqual(self.verdict(None, 0).note, "no linux result file")
        self.assertEqual(self.verdict(None, 4).note, "no macos result file")
        d = doc("macos", {})
        d["run"]["tools"] = [{"name": "dumac", "status": "failed", "reason": "the build failed"}]
        v = self.verdict(d, 5)
        self.assertEqual((v.value, v.status, v.note), (None, "not measured", "dumac failed: the build failed"))
        self.assertEqual(self.verdict(d, 4).note, "dua was not among the run's competitors")
        d["run"]["tools"] = [{"name": "dua", "status": "measured", "reason": None}]
        self.assertEqual(self.verdict(d, 6).note, "dua was not paired with duscope on T1 cold")
        d["run"]["subject"] = "du"
        self.assertEqual(self.verdict(d, 4).note, "the subject is du, not duscope")
        d["run"] = None
        self.assertEqual(self.verdict(d, 4).note, "the harness part did not run")

    def test_p5_bench_01_goal_2_reads_t2s_warm_write_from_the_index_part(self):
        for write, status in ((2.54, "met"), (2.75, "met"), (2.8, "MISSED")):
            v = self.verdict(doc("linux", {}, write=write), 7, "T2")
            self.assertEqual((v.value, v.status), (write, status))
        d = doc("linux", {}, write=2.5)
        d["index"]["gates_tree"] = None
        self.assertEqual(self.verdict(d, 7, "T2").status, "not measured")
        d["index"] = None
        self.assertEqual(self.verdict(d, 7, "T2").status, "not measured")

    def test_p5_bench_01_the_report_counts_every_target_in_plain_text(self):
        pairs = all_pairs(0.5)
        docs = {"linux": doc("linux", pairs["linux"], write=2.5), "macos": doc("macos", pairs["macos"])}
        names = {"linux": "linux.json", "macos": "macos.json"}
        lines, met = acceptance.report(docs, names)
        self.assertTrue(met)
        self.assertEqual(lines[-1], "22 of 22 targets met, 0 missed, 0 not measured")
        self.assertTrue(all(line.isascii() for line in lines), lines)
        self.assertIn("goal 1: Linux warm p50 <= 1.2x diskus (plan/PROPOSAL.md goal 1 and 1.0 acceptance target (1); Gate 1 -> 2)", lines)
        self.assertIn(f"goal 2: the write starts only after the treemap or JSON is out: held by the test "
                      f"{acceptance.WRITE_AFTER_OUTPUT_TEST} on every platform (DEV-108), not by these files", lines)
        self.assertIn("goal 2: the write's share of a cold T2 scan on Linux, recorded and no target (DEV-108): 1.220", lines)
        docs["macos"]["run"]["results"][0]["subject_ratio_p50"] = 9.0
        lines, met = acceptance.report(docs, names)
        self.assertFalse(met)
        self.assertEqual(lines[-1], "21 of 22 targets met, 1 missed, 0 not measured")
        # One platform's lines alone, as the acceptance run prints them.
        lines, met = acceptance.report(docs, names, ("linux",))
        self.assertTrue(met)
        self.assertEqual(lines[-1], "10 of 10 targets met, 0 missed, 0 not measured")
        self.assertFalse(any("macOS" in line for line in lines), lines)

    def test_p5_bench_01_the_conditions_the_targets_name_are_checked(self):
        d = doc("linux", {}, scale=0.001)
        d["run"]["trees"][1]["filesystem"] = "xfs"
        self.assertEqual(acceptance.conditions(d), [
            "a smoke run at scale 0.001: not the acceptance run",
            "T2's copy is on xfs, where the target names ext4",
        ])
        m = doc("macos", {})
        m["host"]["arch"] = "x86_64"
        m["run"]["trees"][0]["cold_filesystem"] = "hfs"
        self.assertEqual(acceptance.conditions(m), [
            "T1's cold copy is on hfs, where the target names apfs",
            "measured on x86_64, where the target names Apple silicon",
        ])
        self.assertEqual(acceptance.conditions(doc("linux", {})), [])

    def test_p5_bench_01_judge_exits_0_only_when_every_target_is_met(self):
        pairs = all_pairs(0.5)
        with tempfile.TemporaryDirectory() as tmp:
            def write(name: str, data) -> str:
                path = os.path.join(tmp, name)
                with open(path, "w", encoding="utf-8") as f:
                    f.write(data if isinstance(data, str) else json.dumps(data))
                return path

            write("linux.json", doc("linux", pairs["linux"], write=2.5))
            write("macos.json", doc("macos", pairs["macos"]))

            def judge(*argv: str) -> Tuple[int, str]:
                out = io.StringIO()
                with contextlib.redirect_stdout(out), contextlib.redirect_stderr(io.StringIO()):
                    status = harness.main(["judge", *argv])
                return status, out.getvalue()

            status, out = judge("--dir", tmp, "--summary", os.path.join(tmp, "README.md"))
            self.assertEqual(status, 0, out)
            with open(os.path.join(tmp, "README.md"), encoding="utf-8") as f:
                self.assertEqual(f.read(), acceptance.summary(
                    {"linux": acceptance.load(os.path.join(tmp, "linux.json")),
                     "macos": acceptance.load(os.path.join(tmp, "macos.json"))},
                    {"linux": "linux.json", "macos": "macos.json"}))
            # A platform without its file: its targets are not measured.
            status, out = judge(os.path.join(tmp, "linux.json"))
            self.assertEqual(status, 1)
            self.assertIn("macos: no result file", out)
            self.assertIn("10 of 22 targets met, 0 missed, 12 not measured", out)
            missed = doc("linux", pairs["linux"], write=2.9)
            status, out = judge(write("missed.json", missed), os.path.join(tmp, "macos.json"))
            self.assertEqual(status, 1)
            self.assertIn("  T2: 2.900 MISSED", out)
            # Not an acceptance file, or two of one platform: nothing is judged.
            self.assertEqual(judge(write("bad.json", "{}"))[0], 2)
            self.assertEqual(judge(write("run.json", run_doc({})))[0], 2)
            self.assertEqual(judge(os.path.join(tmp, "linux.json"), os.path.join(tmp, "missed.json"))[0], 2)


class Summary(unittest.TestCase):
    def test_p5_bench_01_the_summary_is_generated_from_the_files(self):
        pairs = all_pairs(0.5)
        pairs["macos"][("T2", "warm", "diskus")] = 1.07
        linux = doc("linux", pairs["linux"], write=2.54)
        linux["rescan"] = {"trees": [
            {"name": t, "status": "not measured", "reason": "this platform keeps no change history"} for t in ("T1", "T2")
        ]}
        macos = doc("macos", pairs["macos"])
        macos["rescan"] = {"trees": [
            {"name": "T1", "status": "measured", "reason": None, "changed_dirs": 100, "dir_count": 100001,
             "rescanned_dirs": [100, 102], "incremental_p50_s": 0.8, "full_p50_s": 1.6, "incremental_over_full_p50": 0.5},
            {"name": "T3", "status": "not measured", "reason": "too_many_changes"},
        ]}
        text = acceptance.summary({"linux": linux, "macos": macos}, {"linux": "linux.json", "macos": "macos.json"})
        lines = text.splitlines()
        self.assertEqual(lines[0], "# The 1.0 acceptance run")
        self.assertIn("| linux | `linux.json` | 2026-11-20 | a CPU, 10 CPUs, an OS | duscope 1.0.0 at `0123456789ab` |", lines)
        self.assertIn(
            "| macOS warm p50 ≤ 1.0× diskus | 0.50× met | 1.07× **missed** | 0.50× met | "
            "plan/PROPOSAL.md goal 1 and 1.0 acceptance target (2); Gate 1 → 2, deferred to 1.0 by DEV-52 |", lines)
        self.assertIn(
            "| Linux warm index write ≤ 2.75 µs per entry |  | 2.54 µs met |  | "
            "plan/PROPOSAL.md goal 2 and 1.0 acceptance target (3); DEV-107; Gate 2 → 3 |", lines)
        self.assertIn("| macos | T1 | 100 of 100001 | 100–102 | 0.800 s | 1.600 s | 0.50 |", lines)
        self.assertIn("| macos | T3 | not measured | | | | |", lines)
        self.assertIn("- linux T1, T2: this platform keeps no change history", lines)
        self.assertIn("21 of 22 targets met.", lines)
        self.assertEqual(text, acceptance.summary({"linux": linux, "macos": macos}, {"linux": "linux.json", "macos": "macos.json"}))

    def test_p5_bench_01_the_committed_summary_is_the_committed_files(self):
        found = {p: os.path.join(acceptance.OUT_DIR, f"{p}.json") for p in acceptance.PLATFORMS}
        found = {p: path for p, path in found.items() if os.path.exists(path)}
        if not found:
            self.skipTest("no 1.0 result file is committed yet")
        docs = {p: (acceptance.load(found[p]) if p in found else None) for p in acceptance.PLATFORMS}
        names = {p: os.path.basename(path) for p, path in found.items()}
        with open(os.path.join(acceptance.OUT_DIR, "README.md"), encoding="utf-8") as f:
            self.assertEqual(f.read(), acceptance.summary(docs, names),
                             "bench/results/1.0/README.md: run `bench/run.sh judge --summary bench/results/1.0/README.md`")


class FakeDuscope:
    """Stands in for rescanbench.Duscope: each scan an id in order, each rescan
    the given number of directories read, and a refusal where asked."""

    def __init__(self, read: int = 3, refuse_at: Optional[int] = None):
        self.calls: List[Tuple[str, ...]] = []
        self.read = read
        self.refuse_at = refuse_at
        self.count = 0

    def save(self, root: str) -> str:
        self.calls.append(("save",))
        return "P0"

    def rescan(self, root: str, parent: str, which: str):
        self.count += 1
        self.calls.append((which, parent))
        if self.refuse_at == self.count:
            raise rescanbench.Refused("not possible (too_many_changes)")
        wall = 0.5 if which == rescanbench.INCREMENTAL else 2.0
        scan = {"id": f"S{self.count}", "rescanned_dirs": self.read if which == rescanbench.INCREMENTAL else None,
                "dir_count": 26}
        return measure.Run(wall, 0.1, 0.1, 1000), scan, 0.25

    def keep(self, scan: Optional[str]) -> None:
        self.calls.append(("keep", scan))


class Rescans(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, self.tmp)
        self.t = trees.tree("T2", 0.0002)
        self.stamp = trees.build(self.t, self.tmp, 1)
        self.root = trees.tree_root(self.t, self.tmp)

    def test_p5_bench_01_the_change_touches_at_most_1_percent_spread_over_the_tree(self):
        for name in ("T1", "T2"):
            t = trees.tree(name)
            got = rescanbench.changed_dirs(t)
            self.assertEqual(len(got), 100, name)
            self.assertEqual(len(set(got)), 100, name)
            every = trees.directory_paths(t)
            self.assertTrue(set(got) <= set(every[1:]), name)
            # Spread over the tree: the first and the last hundredth both hold one.
            self.assertEqual(got[0], every[1])
            self.assertGreater(every.index(got[-1]), len(every) * 0.98)
        self.assertEqual(rescanbench.changed_dirs(trees.tree("T1"), 10)[1], trees.directory_paths(trees.tree("T1"))[10_001])
        # A small tree: one directory; T3 has none below its root.
        self.assertEqual(len(rescanbench.changed_dirs(trees.tree("T1", 0.0002))), 1)
        self.assertEqual(rescanbench.changed_dirs(trees.tree("T3")), ["."])
        self.assertEqual(rescanbench.changed_dirs(trees.tree("T3", 0.0002)), ["."])

    def test_p5_bench_01_the_change_is_undone_to_the_trees_stamp(self):
        relatives = rescanbench.changed_dirs(self.t) + ["."]
        saved = rescanbench.change(self.root, relatives)
        self.assertNotEqual(trees.walk_digest(self.root), self.stamp["digest"])
        for relative in relatives:
            path = os.path.join(self.root, relative, rescanbench.CHANGE_NAME)
            self.assertEqual(os.path.getsize(path), rescanbench.CHANGE_BYTES)
        rescanbench.undo(saved)
        self.assertEqual(trees.walk_digest(self.root), self.stamp["digest"])
        # Undone twice, nothing is left to remove.
        rescanbench.undo(saved)
        self.assertEqual(trees.walk_digest(self.root), self.stamp["digest"])

    def test_p5_bench_01_rounds_alternate_and_start_from_the_last_full_rescan(self):
        fake = FakeDuscope(read=3)
        slept: List[float] = []
        found, rows = rescanbench.measure_tree(fake, self.t, self.root, 3, 1.5, 100, lambda _: None, slept.append)
        inc, full = rescanbench.INCREMENTAL, rescanbench.FULL
        self.assertEqual(fake.calls, [
            ("save",), (inc, "P0"), ("keep", "P0"),
            (inc, "P0"), (full, "P0"), ("keep", "S3"),
            (full, "S3"), (inc, "S3"), ("keep", "S4"),
            (inc, "S4"), (full, "S4"), ("keep", "S7"),
            ("keep", None),
        ])
        self.assertEqual(slept, [1.5, 1.5, 1.5])
        self.assertEqual(found["status"], "measured")
        self.assertEqual((found["rounds"], found["rescanned_dirs"], found["short_runs"]), (3, [3, 3, 3], 0))
        self.assertEqual((found["dir_count"], found["changed_dirs"]), (26, 1))
        self.assertEqual((found["incremental_p50_s"], found["full_p50_s"], found["incremental_over_full_p50"]), (0.5, 2.0, 0.25))
        self.assertEqual(found["write_p50_s"], {inc: 0.25, full: 0.25})
        self.assertEqual([(r["tool"], r["paired_with"], r["run_count"]) for r in rows], [(inc, None, 3), (full, inc, 3)])
        self.assertEqual(rows[1]["subject_ratio_p50"], 0.25)
        self.assertEqual(trees.walk_digest(self.root), self.stamp["digest"])

    def test_p5_bench_01_a_refused_incremental_rescan_is_recorded_with_the_binarys_reason(self):
        for refuse_at, rounds in ((1, 0), (3, 1)):
            fake = FakeDuscope(refuse_at=refuse_at)
            found, rows = rescanbench.measure_tree(fake, self.t, self.root, 3, 0, 100, lambda _: None, lambda _: None)
            self.assertEqual((found["status"], found["reason"], found["rounds"], rows),
                             ("not measured", "not possible (too_many_changes)", rounds, []), refuse_at)
            self.assertEqual(fake.calls[-1], ("keep", None))
            self.assertEqual(trees.walk_digest(self.root), self.stamp["digest"])

    def test_p5_bench_01_a_round_that_read_less_than_the_change_is_a_failure(self):
        fake = FakeDuscope(read=0)
        logs: List[str] = []
        with mock.patch.object(rescanbench, "Duscope", lambda path, index_dir: fake), \
                mock.patch.object(rescanbench.time, "sleep", lambda _: None):
            part, failures = rescanbench.measure_rescans("duscope", [self.t], self.tmp, 2, 0, 100, logs.append)
        [found] = part["trees"]
        self.assertEqual((found["status"], found["short_runs"]), ("measured", 2))
        self.assertIn("raise --settle", found["reason"])
        self.assertEqual(failures, [f"rescan T2: {found['reason']}"])
        self.assertEqual(len(part["results"]), 2)

    def test_p5_bench_01_a_rescan_reads_the_answer_and_a_refusal(self):
        stub = os.path.join(self.tmp, "duscope")
        with open(stub, "w", encoding="utf-8") as f:
            f.write(
                "#!/bin/sh\n"
                "case \"$1 $5\" in\n"
                "  'scan '*) echo '{\"scan\": {\"id\": \"abc\"}}' ;;\n"
                "  *--full) echo 'duscope: write: 26 entries in 0.004 s: 150.0 µs/entry' >&2\n"
                "           echo '{\"scan\": {\"id\": \"def\", \"incremental\": false}}' ;;\n"
                "  *--incremental) echo '{\"error\": {\"code\": \"not_available\", \"message\": \"no history\"}}'; exit 2 ;;\n"
                "esac\n"
            )
        os.chmod(stub, os.stat(stub).st_mode | stat.S_IXUSR)
        d = rescanbench.Duscope(stub, os.path.join(self.tmp, "index"))
        self.assertEqual(d.save(self.root), "abc")
        run, scan, write = d.rescan(self.root, "abc", rescanbench.FULL)
        self.assertEqual((scan["id"], write), ("def", 0.004))
        self.assertGreater(run.wall, 0)
        with self.assertRaises(rescanbench.Refused) as refused:
            d.rescan(self.root, "abc", rescanbench.INCREMENTAL)
        self.assertEqual(str(refused.exception), "no history")


def with_diskus(d: dict, ratios: Dict[Tuple[str, str, int], float], threads: Tuple[int, ...] = (10, 20)) -> dict:
    """`d` with an acceptance file's `diskus` part: each ratio duscope's to
    diskus at `-j n` on a tree in a mode."""
    d["diskus"] = {
        "threads": list(threads), "default_threads": 30, "modes": ["warm", "cold"], "version": "diskus 0.9.0",
        "run": run_doc({(t, m, acceptance.diskus_name(n)): v for (t, m, n), v in ratios.items()}), "reason": None,
    }
    return d


class DiskusAtItsBest(unittest.TestCase):
    """P5-BENCH-04: `judge` holds duscope to diskus at the best of -j = the
    logical CPUs, twice them, and its default (DEV-213)."""

    def verdict(self, d: Optional[dict], index: int, tree: str = "T1") -> acceptance.Verdict:
        return acceptance.judge_one(acceptance.TARGETS[index], tree, d)

    def test_p5_bench_04_the_judge_takes_the_fastest_of_diskus_three(self):
        d = with_diskus(doc("linux", {("T1", "warm", "diskus"): 0.40}), {("T1", "warm", 10): 0.55, ("T1", "warm", 20): 0.62})
        v = self.verdict(d, 0)
        self.assertEqual((v.value, v.status, v.note), (0.62, "met", "diskus at -j 20, the fastest of its default, -j 10 and -j 20"))
        # A diskus faster at -j 10 than duscope's limit allows is a miss its default would hide.
        d = with_diskus(doc("linux", {("T1", "cold", "diskus"): 1.1}), {("T1", "cold", 10): 1.3, ("T1", "cold", 20): 1.2})
        v = self.verdict(d, 2)
        self.assertEqual((v.value, v.status, v.note), (1.3, "MISSED", "diskus at -j 10, the fastest of its default, -j 10 and -j 20"))
        d = with_diskus(doc("macos", {("T2", "warm", "diskus"): 0.9}), {("T2", "warm", 18): 0.7, ("T2", "warm", 36): 0.8}, (18, 36))
        v = self.verdict(d, 3, "T2")
        self.assertEqual((v.value, v.note), (0.9, "diskus at its default, the fastest of its default, -j 18 and -j 36"))
        # The other targets read their own competitors alone.
        self.assertEqual(self.verdict(with_diskus(doc("macos", {("T1", "warm", "dua"): 0.5}), {("T1", "warm", 10): 2.0}), 4).value, 0.5)

    def test_p5_bench_04_without_the_other_counts_diskus_is_judged_at_its_default_and_says_so(self):
        v = self.verdict(doc("linux", {("T1", "warm", "diskus"): 0.4}), 0)
        self.assertEqual((v.value, v.note), (0.4, "diskus at its default alone: the file measured diskus at no other -j"))
        d = doc("linux", {("T1", "warm", "diskus"): 0.4})
        d["diskus"] = {"threads": [10, 20], "run": None, "reason": "diskus failed: not installed"}
        self.assertEqual(self.verdict(d, 0).note, "diskus at its default alone: diskus failed: not installed")
        d = with_diskus(doc("linux", {("T1", "cold", "diskus"): 0.4}), {("T1", "warm", 10): 0.5})
        self.assertEqual(self.verdict(d, 2).note,
                         "diskus at its default alone: diskus at -j 10 and -j 20 was not paired with duscope on T1 cold")
        # Its default not measured, the other counts are judged and the note says why.
        tools_ = [{"name": "diskus", "status": "failed", "reason": "the download failed"}]
        d = with_diskus(doc("linux", {}, tools=tools_), {("T1", "warm", 10): 0.5, ("T1", "warm", 20): 0.45})
        v = self.verdict(d, 0)
        self.assertEqual((v.value, v.status), (0.5, "met"))
        self.assertEqual(v.note, "diskus at -j 10, the fastest of -j 10 and -j 20; diskus failed: the download failed")
        v = self.verdict(doc("linux", {}, tools=tools_), 0)
        self.assertEqual((v.value, v.status), (None, "not measured"))
        self.assertIn("diskus failed: the download failed", v.note)

    def test_p5_bench_04_diskus_runs_at_its_other_counts_where_a_diskus_target_binds(self):
        both = ["warm", "cold"]
        self.assertEqual(acceptance.diskus_modes("linux", both, True), both)
        self.assertEqual(acceptance.diskus_modes("linux", both, False), ["warm"])
        self.assertEqual(acceptance.diskus_modes("macos", both, True), ["warm"], "no macOS target reads diskus cold")
        self.assertEqual(acceptance.diskus_modes("macos", ["cold"], True), [])
        self.assertEqual(acceptance.DISKUS_THREADS, (1, 2))

    def test_p5_bench_04_the_diskus_part_pairs_duscope_with_diskus_at_the_cpus_and_twice_them(self):
        with tempfile.TemporaryDirectory() as tmp:
            args = harness.parser().parse_args(["acceptance", "--duscope", "d", "--trees", "T1", "--runs", "3"])
            common = ["--scale", "1", "--jobs", "1", "--mode", "both", "--cold-cmd", "true", "--warmup", "2",
                      "--trees-dir", tmp, "--tools-dir", tmp, "--hyperfine", "hyperfine"]
            seen = []

            def run(run_args) -> int:
                seen.append(run_args)
                with open(run_args.out, "w", encoding="utf-8") as f:
                    json.dump({"subject": "duscope", "results": []}, f)
                return 0

            ready = tools.Resolved(tools.PINS["diskus"], "ready", "release", "/opt/a dir/diskus", "diskus 0.9.0")
            failures: List[str] = []
            with mock.patch.object(harness, "cmd_run", run), mock.patch.object(tools, "resolve", lambda *a: ready), \
                    mock.patch.object(acceptance.os, "cpu_count", lambda: 10), contextlib.redirect_stderr(io.StringIO()):
                part = acceptance._diskus_part(args, harness, "macos", "macos-arm64", "/bin/duscope", {}, common, tmp, failures)
            [got] = seen
            self.assertEqual((part["threads"], part["default_threads"], part["modes"], part["version"]), ([10, 20], 30, ["warm"], "diskus 0.9.0"))
            self.assertEqual((got.subject, got.competitors, got.mode, got.runs, got.trees), ("duscope", "none", "warm", 3, "T1"))
            self.assertEqual(got.command, ["diskus-j10='/opt/a dir/diskus' {tree} -j 10", "diskus-j20='/opt/a dir/diskus' {tree} -j 20"])
            self.assertEqual((part["run"], failures), ({"subject": "duscope", "results": []}, []))
            # diskus not installed: the part says so and nothing runs.
            gone = tools.Resolved(tools.PINS["diskus"], "failed", "release", reason="not installed")
            with mock.patch.object(harness, "cmd_run", run), mock.patch.object(tools, "resolve", lambda *a: gone):
                part = acceptance._diskus_part(args, harness, "linux", "linux-x86_64", "/bin/duscope", {}, common, tmp, failures)
            self.assertEqual((part["run"], part["reason"], len(seen)), (None, "diskus failed: not installed", 1))


def line_at(at: float, text: str) -> Tuple[float, str]:
    return at, f"duscope: {text}"


class RescanPhases(unittest.TestCase):
    """P5-BENCH-04: the rescan by phase, from the binary's lines and the time
    each reached the harness."""

    DEBUG = "debug: duscope_walk::platform::macos::fsevents: "
    INCREMENTAL = [
        line_at(0.030, DEBUG + "cannot tell whether FSEvents logs /System/Volumes/Data: Permission denied (os error 13)"),
        line_at(1.180, DEBUG + "FSEvents replay of /Users/a/T2 from 5012005366: 125 directories, 0 subtrees, dropped false, wrapped false, done true"),
        line_at(1.300, "incremental rescan: 125 of the saved scan's 125,001 directories changed; 0 volatile entries read again"),
        line_at(1.320, "incremental walk: 125 directories read, 124876 subtrees taken from the saved scan in 0.015 s"),
        line_at(2.300, "incremental merge: 999000 entries taken from the saved scan in 0.980 s"),
        line_at(2.330, "post-check: 1249 of 124876 directories taken from the saved scan read again in 0.028 s"),
        line_at(2.400, "walk: 1,000,001 entries, 125,001 directories, 3,400 MB in 1.030 s: 970,874 entries/s, 3,300 MB/s"),
        line_at(2.400, "roll-up: 0.045 s"),
        line_at(3.300, "write: 1,000,001 entries in 0.800 s: 0.80 µs/entry"),
    ]
    FULL_LINUX = [
        line_at(1.400, "listing: 125,001 directories in 0.412 s: 303,400 directories/s"),
        line_at(1.400, "stat: 1,000,001 entries, 3,400 MB in 0.822 s: 1,216,546 entries/s, 4,136 MB/s"),
        line_at(1.400, "roll-up: 0.045 s"),
        line_at(3.100, "write: 1,000,001 entries in 1.650 s: 1.65 µs/entry"),
    ]

    def setUp(self) -> None:
        self.tmp = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, self.tmp)

    def phases(self, lines, wall: float, which: str):
        got, notes = rescanbench.phases(lines, wall, which)
        return {k: (round(v, 6) if v is not None else None) for k, v in got.items()}, notes

    def test_p5_bench_04_an_incremental_rescans_phases_are_read_from_its_lines(self):
        got, notes = self.phases(self.INCREMENTAL, 3.35, rescanbench.INCREMENTAL)
        self.assertEqual(got, {
            "parent_s": 0.03, "replay_s": 1.15, "volatile_s": 0.12, "plan_s": 1.3, "walk_s": 0.015,
            "read_back_s": 0.98, "post_check_s": 0.028, "rollup_s": 0.045, "write_s": 0.8, "rest_s": 0.182,
        })
        self.assertEqual(notes, [])
        self.assertEqual(tuple(got), rescanbench.PHASES)

    def test_p5_bench_04_without_the_replays_first_line_the_parent_and_the_replay_are_one_figure(self):
        got, notes = self.phases(self.INCREMENTAL[1:], 3.35, rescanbench.INCREMENTAL)
        self.assertEqual((got["parent_s"], got["replay_s"], got["volatile_s"], got["plan_s"]), (None, None, 0.12, 1.3))
        self.assertIn("loading the parent and the replay are plan_s less volatile_s", notes[0])
        got, notes = self.phases(self.INCREMENTAL[2:], 3.35, rescanbench.INCREMENTAL)
        self.assertEqual((got["parent_s"], got["replay_s"], got["volatile_s"], got["plan_s"]), (None, None, None, 1.3))
        self.assertIn("are plan_s alone", notes[0])
        with self.assertRaisesRegex(measure.MeasureError, "no plan, incremental walk"):
            rescanbench.phases(self.INCREMENTAL[:3] + self.INCREMENTAL[6:], 3.35, rescanbench.INCREMENTAL)

    def test_p5_bench_04_a_full_rescans_phases_on_linux_and_macos(self):
        got, notes = self.phases(self.FULL_LINUX, 3.15, rescanbench.FULL)
        self.assertEqual(got, {
            "parent_s": 0.121, "replay_s": None, "volatile_s": None, "plan_s": None, "walk_s": 1.234,
            "read_back_s": None, "post_check_s": None, "rollup_s": 0.045, "write_s": 1.65, "rest_s": 0.1,
        })
        self.assertEqual(notes, [])
        macos = [self.INCREMENTAL[6], self.INCREMENTAL[7], line_at(3.3, "write: 1,000,001 entries in 0.900 s: 0.90 µs/entry")]
        got, _ = self.phases(macos, 3.35, rescanbench.FULL)
        self.assertEqual((got["parent_s"], got["walk_s"], got["write_s"], got["rest_s"]), (1.325, 1.03, 0.9, 0.05))
        with self.assertRaisesRegex(measure.MeasureError, "no roll-up and write lines"):
            rescanbench.phases(self.FULL_LINUX[:2], 3.15, rescanbench.FULL)

    def test_p5_bench_04_each_level_changes_its_share_of_the_directories(self):
        t2, t1 = trees.tree("T2"), trees.tree("T1")
        every = trees.directory_paths(t2)
        for percent, count in ((0, 0), (0.1, 125), (1, 1250), (10, 12_500)):
            got = rescanbench.level_dirs(t2, percent)
            self.assertEqual((len(got), len(set(got))), (count, count), percent)
            self.assertTrue(set(got) <= set(every[1:]))
        self.assertEqual(len(rescanbench.level_dirs(t1, 0.1)), 100)
        spread = rescanbench.level_dirs(t2, 10)
        self.assertEqual((spread[0], spread[1]), (every[1], every[11]))
        self.assertEqual(len(rescanbench.level_dirs(trees.tree("T2", 0.0002), 0.1)), 1, "at least one")
        self.assertEqual((rescanbench.level_dirs(trees.tree("T3"), 1), rescanbench.level_dirs(trees.tree("T3"), 0)), (["."], []))
        self.assertEqual(rescanbench.LEVELS, (0.0, 0.1, 1.0, 10.0))
        self.assertEqual(rescanbench.parse_levels("10,0,0.1,1,1"), [0.0, 0.1, 1.0, 10.0])
        for bad in ("", "x", "-1", "101", "1,,y"):
            with self.assertRaises(ValueError):
                rescanbench.parse_levels(bad)


class FakePhased:
    """Stands in for rescanbench.Duscope in the rescans by phase: each rescan
    an id in order and the given phases; `refuse` the message of an
    incremental rescan refused at the calls `refuse_at` (all when None)."""

    def __init__(self, refuse: Optional[str] = None, refuse_at: Optional[List[int]] = None):
        self.calls: List[Tuple[str, ...]] = []
        self.refuse = refuse
        self.refuse_at = refuse_at
        self.count = 0

    def save(self, root: str) -> str:
        self.calls.append(("save",))
        return "P0"

    def phased(self, root: str, parent: str, which: str):
        self.count += 1
        self.calls.append((which, parent))
        incremental = which == rescanbench.INCREMENTAL
        if incremental and self.refuse and (self.refuse_at is None or self.count in self.refuse_at):
            raise rescanbench.Refused(self.refuse)
        spent = dict.fromkeys(rescanbench.PHASES)
        spent.update(walk_s=0.1, rollup_s=0.01, write_s=0.2, rest_s=0.01, parent_s=0.02)
        if incremental:
            spent.update(replay_s=0.1, volatile_s=0.01, plan_s=0.13, read_back_s=0.05, post_check_s=0.01)
        scan = {"id": f"S{self.count}", "rescanned_dirs": 3 if incremental else None, "dir_count": 26,
                "volatile_count": 1 if incremental else None}
        return measure.Run(0.5 if incremental else 2.0, 0.1, 0.1, 1000), scan, spent, []

    def keep(self, scan: Optional[str]) -> None:
        self.calls.append(("keep", scan))


class RescanLevels(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, self.tmp)
        self.t = trees.tree("T2", 0.0002)
        self.stamp = trees.build(self.t, self.tmp, 1)
        self.root = trees.tree_root(self.t, self.tmp)

    def levels(self, fake: FakePhased, levels=(0.0, 10.0), runs: int = 2, warmup: int = 1):
        slept: List[float] = []
        cleared, undone = mock.Mock(wraps=rescanbench.clear), mock.Mock(wraps=rescanbench.undo)
        with mock.patch.object(rescanbench, "clear", cleared), mock.patch.object(rescanbench, "undo", undone):
            found, rows, failures = rescanbench.measure_levels(fake, self.t, self.root, levels, runs, warmup, 1.5,
                                                               lambda _: None, slept.append)
        self.assertEqual(trees.walk_digest(self.root), self.stamp["digest"], "each level puts the times back")
        return found, rows, failures, slept, cleared.call_count, undone.call_count

    def test_p5_bench_04_rounds_alternate_from_the_last_full_rescan_and_times_are_put_back_once_a_level(self):
        fake = FakePhased()
        found, rows, failures, slept, cleared, undone = self.levels(fake)
        inc, full = rescanbench.INCREMENTAL, rescanbench.FULL
        self.assertEqual(fake.calls[:9], [
            ("save",), (inc, "P0"), ("keep", "P0"),
            (inc, "P0"), (full, "P0"), ("keep", "S3"),    # level 0, the warm-up round
            (full, "S3"), (inc, "S3"), ("keep", "S4"),   # its first recorded round, from the full rescan
        ])
        self.assertEqual(fake.calls[-1], ("keep", None))
        self.assertEqual(slept, [1.5] * 6)
        self.assertEqual((cleared, undone), (6, 2), "the change removed every round, the times put back once a level")
        self.assertEqual((found["incremental"], found["reason"], found["dir_count"]), ("measured", None, 26))
        zero, ten = found["levels"]
        self.assertEqual((zero["percent"], zero["changed_dirs"], ten["changed_dirs"]), (0.0, 0, 2))
        self.assertEqual((ten["rounds"], ten["rescanned_dirs"], ten["volatile_count"], ten["short_runs"]), (2, [3, 3], [1, 1], 0))
        self.assertEqual(ten[inc]["phases_p50_s"]["read_back_s"], 0.05)
        self.assertEqual((ten[inc]["wall_p50_s"], ten[full]["wall_p50_s"], ten[full]["phases_p50_s"]["read_back_s"]), (0.5, 2.0, None))
        self.assertEqual(len(ten[inc]["runs"]), 2)
        self.assertEqual([(r["tool"], r["paired_with"], r["run_count"]) for r in rows], [
            (f"{inc} at 0%", None, 2), (f"{full} at 0%", f"{inc} at 0%", 2),
            (f"{inc} at 10%", None, 2), (f"{full} at 10%", f"{inc} at 10%", 2),
        ])
        self.assertEqual(rows[3]["subject_ratio_p50"], 0.25)
        self.assertEqual(failures, [])

    def test_p5_bench_04_linux_measures_the_full_rescan_alone_at_every_level(self):
        fake = FakePhased(refuse="this platform keeps no change history to read only what changed: every rescan reads the whole tree")
        found, rows, failures, slept, _, _ = self.levels(fake)
        self.assertEqual((found["incremental"], slept), ("not measured", []), "no history to wait for")
        self.assertIn("keeps no change history", found["reason"])
        for level in found["levels"]:
            self.assertEqual((level[rescanbench.INCREMENTAL]["status"], level[rescanbench.FULL]["status"]), ("not measured", "measured"))
            self.assertEqual(level["rescanned_dirs"], [])
        self.assertEqual([c[0] for c in fake.calls[3:]].count(rescanbench.INCREMENTAL), 0)
        self.assertEqual([(r["tool"], r["paired_with"]) for r in rows], [(f"{rescanbench.FULL} at 0%", None), (f"{rescanbench.FULL} at 10%", None)])
        self.assertEqual(failures, [])

    def test_p5_bench_04_a_refusal_ends_the_incremental_rescans_of_its_level_alone(self):
        too_many = "an incremental rescan of saved scan P0 is not possible (too_many_changes): 9 of 26 directories changed"
        # Refused before the levels: each level tries again.
        found, _, _, _, _, _ = self.levels(FakePhased(refuse=too_many, refuse_at=[1]))
        self.assertEqual((found["incremental"], [lv[rescanbench.INCREMENTAL]["status"] for lv in found["levels"]]),
                         ("measured", ["measured", "measured"]))
        # Refused in the first level's warm-up round: that level's are not measured, the next one's are.
        found, rows, _, _, _, _ = self.levels(FakePhased(refuse=too_many, refuse_at=[2]))
        zero, ten = found["levels"]
        self.assertEqual((zero[rescanbench.INCREMENTAL]["status"], zero[rescanbench.INCREMENTAL]["reason"]), ("not measured", too_many))
        self.assertEqual(ten[rescanbench.INCREMENTAL]["status"], "measured")
        self.assertEqual(found["incremental"], "measured")
        # Refused everywhere: the tree says why.
        found, _, _, _, _, _ = self.levels(FakePhased(refuse=too_many))
        self.assertEqual((found["incremental"], found["reason"]), ("not measured", too_many))

    def test_p5_bench_04_a_round_that_read_less_than_its_change_is_a_failure(self):
        fake = FakePhased()
        with mock.patch.object(rescanbench, "level_dirs", lambda t, p: rescanbench.trees.directory_paths(t)[1:5] if p else []):
            _, _, failures, _, _, _ = self.levels(fake)
        self.assertEqual(len(failures), 1)
        self.assertIn("rescan T2 at 10%: 2 of 2 incremental rescans read fewer directories than the 4", failures[0])
        self.assertIn("raise --settle", failures[0])

    def test_p5_bench_04_rescan_refuses_bad_options(self):
        for bad in (["--levels", "x"], ["--levels", "200"], ["--runs", "0"], ["--warmup", "-1"], ["--settle", "-1"]):
            err = io.StringIO()
            with contextlib.redirect_stderr(err), contextlib.redirect_stdout(io.StringIO()):
                status = harness.main(["rescan", "--duscope", "duscope", *bad])
            self.assertEqual(status, 2, (bad, err.getvalue()))
        args = harness.parser().parse_args(["rescan", "--duscope", "d"])
        self.assertEqual((args.trees, args.levels, args.runs, args.warmup, args.settle), ("T1,T2", "0,0.1,1,10", 10, 1, rescanbench.SETTLE_S))


class Command(unittest.TestCase):
    def run_main(self, *argv: str) -> Tuple[int, str]:
        err = io.StringIO()
        with contextlib.redirect_stderr(err), contextlib.redirect_stdout(io.StringIO()):
            status = harness.main(list(argv))
        return status, err.getvalue()

    def test_p5_bench_01_acceptance_refuses_what_would_write_the_wrong_file(self):
        here = harness.platform_name()
        other = "macos" if here == "linux" else "linux"
        status, err = self.run_main("acceptance", "--duscope", "duscope", "--platform", other)
        self.assertEqual(status, 2)
        self.assertIn(f"--platform {other}, but this machine is {here}", err)
        status, err = self.run_main("acceptance", "--duscope", "duscope", "--tool-path", "duscope=/bin/true")
        self.assertEqual(status, 2)
        self.assertIn("acceptance measures the --duscope it is given", err)
        for bad in (["--runs", "0"], ["--changed-dirs", "0"], ["--settle", "-1"], ["--tool-path", "dsu=/bin/true"]):
            status, err = self.run_main("acceptance", "--duscope", "duscope", *bad)
            self.assertEqual(status, 2, bad)

    def test_p5_bench_01_acceptance_writes_its_file_under_the_10_results(self):
        args = harness.parser().parse_args(["acceptance", "--duscope", "d"])
        self.assertEqual(args.out_dir, os.path.join(harness.BENCH, "results", "1.0"))
        self.assertEqual((args.trees, args.competitors, args.mode, args.cold_competitors), ("all", "all", "both", None))
        self.assertEqual((args.runs, args.warmup, args.index_runs), (10, 2, 20))
        self.assertEqual((args.settle, args.changed_dirs), (rescanbench.SETTLE_S, rescanbench.CHANGED_DIRS))


if __name__ == "__main__":
    unittest.main()
