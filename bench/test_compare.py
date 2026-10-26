"""Unit tests of bench/compare.py; duscope/tests/nightly_bench.rs runs them as
`p0_bench_02_compare_unit_tests` (and `p0_bench_01_python_unit_tests` finds them
with the other test_*.py files):

    python3 -B -m unittest discover -s bench -p 'test_compare.py'
"""

from __future__ import annotations

import contextlib
import io
import json
import os
import tempfile
import unittest
from typing import Dict, List, Optional
from unittest import mock

import compare

DIGEST = "a1ddf1f022c8aca72a18b689a9952357"


def result(
    p50: Dict[str, float],
    started_at: str = "2026-10-25T10:00:00Z",
    platform: str = "macos",
    arch: str = "arm64",
    digest: str = DIGEST,
    tool: str = "du",
    subject: str = "du",
    tree: str = "T1",
    cpu: Optional[str] = "Apple M5 Pro",
    cpus: int = 18,
    ratios: Optional[Dict[str, float]] = None,
) -> dict:
    """A result file with `tool`'s p50 on `tree` in each mode of `p50`, and for
    each `mode/competitor` in `ratios` the competitor's entry with that
    `subject_ratio_p50`, trimmed to the fields compare.py reads."""
    pairs = [
        {"tree": tree, "mode": key.split("/")[0], "tool": key.split("/")[1], "paired_with": tool,
         "p50_s": 1.0, "subject_ratio_p50": value}
        for key, value in (ratios or {}).items()
    ]
    return {
        "schema_version": 1,
        "started_at": started_at,
        "platform": platform,
        "subject": subject,
        "host": {"arch": arch, "cpu": cpu, "logical_cpus": cpus},
        "trees": [{"name": tree, "digest": digest}],
        "results": [{"tree": tree, "mode": mode, "tool": tool, "p50_s": value} for mode, value in p50.items()]
        + pairs,
    }


class Compare(unittest.TestCase):
    def setUp(self) -> None:
        self.dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.dir.cleanup)
        self.count = 0

    def write(self, data: dict, name: Optional[str] = None) -> str:
        self.count += 1
        path = os.path.join(self.dir.name, name or f"r{self.count:02d}.json")
        with open(path, "w", encoding="utf-8") as f:
            json.dump(data, f)
        return path

    def baselines(self, values: List[float], **kwargs) -> List[str]:
        """One baseline per value, a day apart, oldest first."""
        return [
            self.write(result({"warm": v}, started_at=f"2026-10-{10 + i:02d}T10:00:00Z", **kwargs))
            for i, v in enumerate(values)
        ]

    def run_compare(self, args: List[str], github: bool = False) -> "tuple[int, str]":
        out = io.StringIO()
        env = {"GITHUB_ACTIONS": "true"} if github else {}
        with mock.patch.dict(os.environ, env, clear=False), contextlib.redirect_stdout(out), contextlib.redirect_stderr(out):
            if not github:
                os.environ.pop("GITHUB_ACTIONS", None)
            code = compare.main(args)
        return code, out.getvalue()

    def test_p0_bench_02_a_regression_beyond_the_tolerance_fails(self):
        base = self.baselines([1.0, 1.0, 1.0, 1.0, 1.0])
        for p50, want in [(1.10, 0), (1.15, 0), (1.16, 1), (2.0, 1), (0.5, 0)]:
            current = self.write(result({"warm": p50}, started_at="2026-10-25T10:00:00Z"))
            code, out = self.run_compare([current, *base])
            self.assertEqual(code, want, f"p50 {p50}: {out}")
            self.assertIn("over 5 results", out)
            self.assertIn("regression" if want else "ok", out)

    def test_p0_bench_02_the_baseline_is_the_median_of_the_last_five(self):
        # The two oldest are much faster and fall outside the last five; the
        # median of the last five ignores the one slow outlier among them.
        base = self.baselines([0.2, 0.2, 1.0, 1.0, 3.0, 1.0, 1.0])
        current = self.write(result({"warm": 1.1}, started_at="2026-10-25T10:00:00Z"))
        code, out = self.run_compare([current, *base])
        self.assertEqual(code, 0, out)
        self.assertIn("median of 1.000 s over 5 results", out)
        code, out = self.run_compare([current, *base, "--last", "7"])
        self.assertEqual(code, 0, out)
        self.assertIn("over 7 results", out)

    def test_p0_bench_02_only_the_same_machine_and_tree_count(self):
        others = (
            self.baselines([0.5] * 3, platform="linux")
            + self.baselines([0.5] * 3, arch="x86_64")
            + self.baselines([0.5] * 3, digest="ffffffffffffffffffffffffffffffff")
        )
        current = self.write(result({"warm": 1.0}, started_at="2026-10-25T10:00:00Z"))
        code, out = self.run_compare([current, *others])
        self.assertEqual(code, 0, out)
        self.assertIn("no baseline", out)
        same = self.baselines([0.5] * 2)
        code, out = self.run_compare([current, *others, *same])
        self.assertEqual(code, 1, out)
        self.assertIn("over 2 results", out)

    def test_p0_bench_02_absolute_baselines_need_the_same_cpu(self):
        others = self.baselines([0.5] * 3, cpu="Apple M4") + self.baselines([0.5] * 3, cpus=10)
        current = self.write(result({"warm": 1.0}, started_at="2026-10-25T10:00:00Z"))
        code, out = self.run_compare([current, *others])
        self.assertEqual(code, 0, out)
        self.assertIn("no baseline", out)

    def test_p0_bench_02_ratios_compare_across_cpus_of_a_kind(self):
        # Five earlier runs on another CPU, du at 1.0x diskus: their absolute
        # times do not count here, their ratios do.
        base = self.baselines([0.5] * 5, cpu="Intel Xeon", cpus=4, ratios={"warm/diskus": 1.0})
        for ratio, want in [(1.10, 0), (1.30, 1)]:
            current = self.write(
                result({"warm": 1.0}, started_at="2026-10-25T10:00:00Z", ratios={"warm/diskus": ratio})
            )
            code, out = self.run_compare([current, *base])
            self.assertEqual(code, want, out)
            self.assertIn("T1 warm du: no baseline", out)
            self.assertIn(f"T1 warm du/diskus: ratio {ratio:.3f} against a median of 1.000 over 5 results", out)
        # Another platform's ratios do not count.
        linux = self.baselines([0.5] * 5, platform="linux", ratios={"warm/diskus": 0.5})
        current = self.write(result({"warm": 1.0}, started_at="2026-10-25T10:00:00Z", ratios={"warm/diskus": 1.3}))
        code, out = self.run_compare([current, *linux])
        self.assertEqual(code, 0, out)
        self.assertIn("T1 warm du/diskus: no baseline", out)

    def test_p0_bench_02_each_mode_is_checked(self):
        base = [
            self.write(result({"warm": 1.0, "cold": 2.0}, started_at=f"2026-10-1{i}T10:00:00Z")) for i in range(5)
        ]
        current = self.write(result({"warm": 1.05, "cold": 2.6}, started_at="2026-10-25T10:00:00Z"))
        code, out = self.run_compare([current, *base])
        self.assertEqual(code, 1, out)
        self.assertRegex(out, r"T1 warm du: .* ok")
        self.assertRegex(out, r"T1 cold du: .* regression beyond 15%")
        code, out = self.run_compare([current, *base, "--tolerance", "0.5"])
        self.assertEqual(code, 0, out)

    def test_p0_bench_02_the_tool_defaults_to_the_subject(self):
        base = [
            self.write(result({"warm": 1.0}, tool="diskus", started_at=f"2026-10-1{i}T10:00:00Z")) for i in range(3)
        ]
        current = self.write(result({"warm": 9.0}, tool="diskus", started_at="2026-10-25T10:00:00Z"))
        # The subject (du) has no result on T1 in these files.
        code, out = self.run_compare([current, *base])
        self.assertEqual(code, 2, out)
        self.assertIn("no du result on T1", out)
        code, out = self.run_compare([current, *base, "--tool", "diskus"])
        self.assertEqual(code, 1, out)
        self.assertIn("T1 warm diskus", out)

    def test_p0_bench_02_a_directory_of_baselines_skips_the_current_file(self):
        self.baselines([1.0] * 5)
        current = self.write(result({"warm": 1.0}, started_at="2026-10-25T10:00:00Z"), name="2026-10-25-macos.json")
        code, out = self.run_compare([current, self.dir.name])
        self.assertEqual(code, 0, out)
        self.assertIn("over 5 results", out)

    def test_p0_bench_02_unusable_input_exits_2(self):
        bad = os.path.join(self.dir.name, "bad.json")
        with open(bad, "w", encoding="utf-8") as f:
            f.write("{not json")
        old = self.write({"schema_version": 99})
        code, out = self.run_compare([bad])
        self.assertEqual(code, 2, out)
        code, out = self.run_compare([old])
        self.assertEqual(code, 2, out)
        self.assertIn("schema_version", out)
        # An unreadable baseline is reported and left out, not fatal.
        current = self.write(result({"warm": 1.0}, started_at="2026-10-25T10:00:00Z"))
        base = self.baselines([1.0] * 2)
        code, out = self.run_compare([current, bad, *base])
        self.assertEqual(code, 0, out)
        self.assertIn("bad.json", out)
        self.assertIn("over 2 results", out)
        for args in ([current, "--tolerance", "-0.1"], [current, "--last", "0"]):
            with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as raised:
                compare.main(args)
            self.assertEqual(raised.exception.code, 2)

    def test_p0_bench_02_annotations_in_github_actions(self):
        base = self.baselines([1.0] * 5)
        current = self.write(result({"warm": 2.0}, started_at="2026-10-25T10:00:00Z"))
        code, out = self.run_compare([current, *base], github=True)
        self.assertEqual(code, 1, out)
        self.assertIn("::error title=bench-regression::T1 warm du", out)
        lone = self.write(result({"warm": 2.0}, platform="linux", started_at="2026-10-25T10:00:00Z"))
        code, out = self.run_compare([lone, *base], github=True)
        self.assertEqual(code, 0, out)
        self.assertIn("::notice title=bench-regression::T1 warm du: no baseline", out)


if __name__ == "__main__":
    unittest.main()
