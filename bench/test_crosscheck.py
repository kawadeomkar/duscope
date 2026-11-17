"""Unit tests of bench/crosscheck.py, the off-Proxmox cross-check of the Linux
ratios (P5-BENCH-01, DEV-157); duscope/tests/acceptance_bench.rs runs them as
`p5_bench_01_python_unit_tests`:

    python3 -B -m unittest discover -s bench -p 'test_crosscheck.py'
"""

from __future__ import annotations

import contextlib
import io
import json
import os
import shutil
import tempfile
import unittest
from typing import Dict, Tuple
from unittest import mock

import crosscheck
import harness
from test_acceptance import Pair, run_doc

def nightly(ratios: Dict[Pair, float], started_at: str, cpu: str = "Intel(R) Core(TM) i9-10900K CPU @ 3.70GHz",
            cpus: int = 10, commit: str = "c0ffee") -> dict:
    return run_doc(ratios, cpu=cpu, cpus=cpus, started_at=started_at, commit=commit)


class Crosscheck(unittest.TestCase):
    def setUp(self) -> None:
        self.dir = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, self.dir)

    def write(self, name: str, data: dict) -> str:
        path = os.path.join(self.dir, name)
        with open(path, "w", encoding="utf-8") as f:
            json.dump(data, f)
        return path

    def check(self, *argv: str, github: bool = False) -> Tuple[int, str]:
        out = io.StringIO()
        env = {"GITHUB_ACTIONS": "true" if github else "false"}
        with mock.patch.dict(os.environ, env), contextlib.redirect_stderr(io.StringIO()):
            status = crosscheck.main(list(argv), out)
        return status, out.getvalue()

    def test_p5_bench_01_the_latest_bench_linux_nightly_is_the_baseline(self):
        old = nightly({}, "2026-11-07T17:00:00Z")
        new = nightly({}, "2026-11-08T17:00:00Z")
        other = nightly({}, "2026-11-09T17:00:00Z", cpu="another", cpus=4)
        self.assertEqual(crosscheck.pick([old, new], 1), [new])
        self.assertEqual(crosscheck.pick([old, new], 5), [new, old])
        # --last takes earlier results from the latest one's machine alone.
        self.assertEqual(crosscheck.pick([old, new, other], 5), [other])
        self.assertEqual(crosscheck.pick([], 1), [])
        # Index, redraw and acceptance files, macOS results and another subject
        # are no nightly harness result.
        self.assertTrue(crosscheck.is_nightly(new, "duscope"))
        for kind in ("index", "redraw", "acceptance"):
            self.assertFalse(crosscheck.is_nightly(dict(new, kind=kind), "duscope"), kind)
        self.assertFalse(crosscheck.is_nightly(dict(new, platform="macos"), "duscope"))
        self.assertFalse(crosscheck.is_nightly(new, "du"))

    def test_p5_bench_01_each_ratio_lies_within_15_percent_either_way_or_outside(self):
        base = nightly({("T1", "warm", "diskus"): 0.354, ("T1", "warm", "fdu"): 1.096, ("T1", "cold", "diskus"): 1.087,
                        ("T2", "warm", "dua"): 0.155}, "2026-11-08T17:00:00Z")
        here = run_doc({("T1", "warm", "diskus"): 0.40, ("T1", "warm", "fdu"): 1.25, ("T1", "cold", "diskus"): 0.90,
                        ("T2", "warm", "dua"): 0.16, ("T3", "warm", "dua"): 0.05})
        out, outside, compared = crosscheck.lines(here, [base], 0.15)
        self.assertEqual(compared, 4)
        self.assertEqual(out, [
            "T1 warm duscope/diskus: 0.400 here, 0.354 on bench-linux (+13.0%): within 15%; meets the target 1.2 here",
            "T1 warm duscope/fdu: 1.250 here, 1.096 on bench-linux (+14.1%): within 15%; over the target 1.15 here",
            "T1 cold duscope/diskus: 0.900 here, 1.087 on bench-linux (-17.2%): OUTSIDE 15%, lower here; meets the target 1.15 here",
            "T2 warm duscope/dua: 0.160 here, 0.155 on bench-linux (+3.2%): within 15%",
            "T3 warm duscope/dua: 0.050 here, no bench-linux ratio on the same tree to compare",
        ])
        self.assertEqual(outside, [out[2]])

    def test_p5_bench_01_the_exit_status_says_whether_every_ratio_lies_within(self):
        base = self.write("2026-11-08-linux.json", nightly({("T1", "warm", "diskus"): 0.354}, "2026-11-08T17:00:00Z"))
        self.write("2026-11-07-linux.json", nightly({("T1", "warm", "diskus"): 0.9}, "2026-11-07T17:00:00Z"))
        self.write("2026-11-08-linux-index.json", dict(nightly({}, "2026-11-08T18:00:00Z"), kind="index"))
        # The current file itself, in the directory too, is no baseline.
        within = self.write("here.json", run_doc({("T1", "warm", "diskus"): 0.36}, commit="beef", started_at="2026-11-21T00:00:00Z"))
        status, out = self.check(within, self.dir)
        self.assertEqual(status, 0, out)
        self.assertIn("bench-linux: 2026-11-08-linux.json, 2026-11-08T17:00:00Z, Intel(R) Core(TM) i9-10900K CPU @ 3.70GHz, 10 CPUs, commit c0ffee", out)
        self.assertIn("note: the two measured different commits", out)
        self.assertTrue(out.endswith("1 of 1 ratios within 15% of bench-linux's, 0 outside\n"), out)
        self.assertNotIn("::", out)
        # The median of the last two moves the baseline.
        status, out = self.check(within, self.dir, "--last", "2")
        self.assertEqual(status, 1, out)
        outside = self.write("outside.json", run_doc({("T1", "warm", "diskus"): 0.6}))
        status, out = self.check(outside, base, github=True)
        self.assertEqual(status, 1, out)
        self.assertIn("::warning title=linux-crosscheck::T1 warm duscope/diskus: 0.600 here, 0.354 on bench-linux (+69.5%): OUTSIDE 15%, higher here; meets the target 1.2 here", out)
        # Nothing to compare: another tree (a smoke run's), no nightly, or a
        # current file that is not a Linux harness result.
        scaled = self.write("scaled.json", run_doc({("T1", "warm", "diskus"): 0.3}, digest="scaled"))
        self.assertEqual(self.check(scaled, base)[0], 2)
        self.assertEqual(self.check(outside, self.write("mac.json", dict(nightly({}, "2026-11-09T00:00:00Z"), platform="macos")))[0], 2)
        self.assertEqual(self.check(self.write("index.json", dict(run_doc({}), kind="index")), base)[0], 2)

    def test_p5_bench_01_the_harness_hands_crosscheck_its_command_line(self):
        base = self.write("base.json", nightly({("T2", "cold", "diskus"): 1.0}, "2026-11-08T17:00:00Z"))
        here = self.write("here.json", run_doc({("T2", "cold", "diskus"): 1.1}))
        out = io.StringIO()
        with contextlib.redirect_stdout(out), mock.patch.dict(os.environ, {"GITHUB_ACTIONS": "false"}):
            status = harness.main(["crosscheck", "--tolerance", "0.05", here, base])
        self.assertEqual(status, 1, out.getvalue())
        self.assertIn("(+10.0%): OUTSIDE 5%", out.getvalue())


if __name__ == "__main__":
    unittest.main()
