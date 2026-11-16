"""Unit tests of the agent eval under bench/eval/ (P3-AGT-03), run with the
other bench tests by `duscope/tests/bench_harness.rs`:

    python3 -B -m unittest discover -s bench -p 'test_*.py'

Nothing here calls a model or Claude Code. The end-to-end tests run the
harness with its stub agent (`--fake right|wrong`) and need a duscope binary:
DUSCOPE_EVAL_TEST_BIN, else the newest of target/{debug,release}/duscope
(`cargo test` builds the debug one before it runs the bench tests); without
one they skip.
"""

from __future__ import annotations

import contextlib
import io
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
EVAL = os.path.join(HERE, "eval")
REPO = os.path.dirname(HERE)
if EVAL not in sys.path:
    sys.path.insert(0, EVAL)

import agent_eval  # noqa: E402
import evalchecks  # noqa: E402
import evaltranscript  # noqa: E402
import evaltree  # noqa: E402
import minitoml  # noqa: E402

M = evaltree.M

try:
    import tomllib
except ImportError:  # Python before 3.11
    tomllib = None


def duscope_binary():
    explicit = os.environ.get("DUSCOPE_EVAL_TEST_BIN")
    if explicit:
        return explicit
    target = os.environ.get("CARGO_TARGET_DIR") or os.path.join(REPO, "target")
    found = [os.path.join(target, p, "duscope") for p in ("debug", "release")]
    found = [p for p in found if os.access(p, os.X_OK)]
    return max(found, key=os.path.getmtime) if found else None


def scratch():
    return tempfile.mkdtemp(prefix="duscope-eval-test-")


class Toml(unittest.TestCase):
    def test_p3_agt_03_the_reader_reads_the_eval_files_as_tomllib_does(self):
        for name in ("tasks.toml", "config.toml"):
            path = os.path.join(EVAL, name)
            with open(path, encoding="utf-8") as f:
                ours = minitoml.loads(f.read())
            self.assertTrue(ours, name)
            if tomllib is not None:
                with open(path, "rb") as f:
                    self.assertEqual(ours, tomllib.load(f), name)

    def test_p3_agt_03_the_reader_covers_the_toml_the_files_may_use(self):
        text = (
            "# comment\n"
            'a = "tab\\tquote\\" \\u00e9"\n'
            "b = 'C:\\path'\n"
            'c = """\nline one \\\n   joined"""\n'
            "d = '''\nraw \\n'''\n"
            "e = [1, 2_000, -3,\n  4.5, # inside\n]\n"
            "f = { g = true, h.i = 'x' }\n"
            "[t.u]\n"
            "v = 0x1F\n"
            "[[w]]\nk = 1\n[[w]]\nk = 2\n"
        )
        want = {
            "a": 'tab\tquote" \u00e9',
            "b": "C:\\path",
            "c": "line one joined",
            "d": "raw \\n",
            "e": [1, 2000, -3, 4.5],
            "f": {"g": True, "h": {"i": "x"}},
            "t": {"u": {"v": 31}},
            "w": [{"k": 1}, {"k": 2}],
        }
        self.assertEqual(minitoml.loads(text), want)
        if tomllib is not None:
            self.assertEqual(tomllib.loads(text), want)
        for bad in ("a = 1\na = 2\n", "[t]\n[t]\n", "a = 1979-05-27\n", 'a = "open\n', "a = 1 b\n"):
            with self.assertRaises(minitoml.TomlError, msg=bad):
                minitoml.loads(bad)


class Files(unittest.TestCase):
    # The row's ten tasks, in its order (plan/SPEC.md, P3-AGT-03).
    ROW = [
        "find the largest directory under a root",
        "name the three largest files",
        "report how much the regenerable caches would free",
        "list files unused for a year above 10 MiB",
        "total what the tar.gz archives take",
        "explain why a given directory is large",
        "compare two scans and name the growth",
        "find same-named large files across the tree",
        "produce a cleanup plan without deleting anything",
        "answer a question that needs query --sql",
    ]

    def test_p3_agt_03_the_tasks_are_the_rows_ten(self):
        meta, tasks = agent_eval.load_tasks()
        self.assertEqual([t["row"] for t in tasks], self.ROW)
        self.assertEqual(len({t["id"] for t in tasks}), 10)
        for t in tasks:
            prompt = agent_eval.render_prompt(meta, t, "/r")
            self.assertNotIn("{", prompt.replace(t["answer"], ""), t["id"])
            self.assertIn("/r", prompt)
            self.assertIn("ANSWER:", prompt)
        self.assertEqual([t["id"] for t in tasks if t["fixture"] == "two-scans"], ["diff-growth"])

    def test_p3_agt_03_the_config_pins_the_model_cells_and_runs(self):
        config = agent_eval.load_config()
        self.assertEqual(config["model"]["id"], "claude-sonnet-5-5")
        self.assertEqual(config["cells"]["runs"], 3)
        self.assertEqual(
            [c["id"] for c in agent_eval.cells_of(config)],
            ["no-skill-json", "no-skill-plain", "skill-json", "skill-plain"],
        )
        self.assertEqual(config["decision"]["min_token_reduction"], 0.20)
        # The Claude Code release is the one the MCP smoke test installs.
        smoke = os.path.join(REPO, "crates", "duscope-mcp", "tests", "smoke", "claude-code.sh")
        with open(smoke, encoding="utf-8") as f:
            pinned = re.search(r"^CLAUDE_CODE_VERSION=(\S+)$", f.read(), re.M).group(1)
        self.assertEqual(config["claude_code"]["version"], pinned)
        allowed = config["claude_code"]["allowed_tools"]
        self.assertIn("Bash(duscope:*)", allowed)
        for tool in ("Write", "Edit", "WebFetch"):
            self.assertNotIn(tool, allowed)
            self.assertIn(tool, config["claude_code"]["disallowed_tools"])

    def test_p3_agt_03_a_bad_config_or_task_is_refused(self):
        d = scratch()
        try:
            with open(os.path.join(EVAL, "config.toml"), encoding="utf-8") as f:
                text = f.read()
            path = os.path.join(d, "config.toml")
            with open(path, "w", encoding="utf-8") as f:
                f.write(text.replace('format = ["json", "plain"]', 'format = ["json", "yaml"]'))
            with self.assertRaisesRegex(agent_eval.EvalError, "cells.format"):
                agent_eval.load_config(path)
            with open(os.path.join(EVAL, "tasks.toml"), encoding="utf-8") as f:
                text = f.read()
            path = os.path.join(d, "tasks.toml")
            with open(path, "w", encoding="utf-8") as f:
                f.write(text.replace('check = "path_count"', 'check = "vibes"'))
            with self.assertRaisesRegex(agent_eval.EvalError, "unknown check"):
                agent_eval.load_tasks(path)
        finally:
            shutil.rmtree(d)


class Tree(unittest.TestCase):
    """The full-size tree, built once: every task's answer stands clear."""

    @classmethod
    def setUpClass(cls):
        cls.dir = scratch()
        cls.now = time.time()
        cls.root = os.path.join(os.path.realpath(cls.dir), "alex")
        evaltree.build(cls.root, cls.now)
        cls.ctx = evalchecks.Context(cls.root, cls.now)
        # The caches duscope finds there (checked against duscope end to end
        # in Fake): what its rules say for these three projects.
        cls.ctx._caches = [
            os.path.join(cls.root, p)
            for p in ("Projects/ml-notebook/.venv", "Projects/rust-cli/target", "Projects/web-app/node_modules")
        ]
        _, cls.tasks = agent_eval.load_tasks()

    @classmethod
    def tearDownClass(cls):
        shutil.rmtree(cls.dir)

    def truth(self, task_id):
        t = next(t for t in self.tasks if t["id"] == task_id)
        return evalchecks.TRUTHS[t["truth"]](self.ctx, t["params"])

    def rel(self, path):
        return os.path.relpath(path, self.root)

    def test_p3_agt_03_the_tree_is_the_same_every_build(self):
        other = scratch()
        try:
            root = os.path.join(other, "alex")
            evaltree.build(root, self.now)
            self.assertEqual(evaltree.manifest(root), evaltree.manifest(self.root))
            with open(os.path.join(root, "Movies/clips/clip-a.mov"), "rb") as f:
                head = f.read(64)
            with open(os.path.join(self.root, "Movies/clips/clip-a.mov"), "rb") as f:
                self.assertEqual(f.read(64), head)
        finally:
            shutil.rmtree(other)

    def test_p3_agt_03_each_answer_stands_clear_of_the_next(self):
        t = self.truth("largest-dir")
        self.assertEqual(self.rel(t.expected["path"]), "Downloads")
        self.assertGreater(t.expected["bytes"], 1.15 * t.data["runner_up"][0])

        t = self.truth("largest-files")
        self.assertEqual(
            [self.rel(p) for p in t.expected["paths"]],
            ["Movies/holiday-2025.mov", "Downloads/ubuntu-24.04.1-live-server-arm64.iso", "Downloads/Xcode_16.1.xip"],
        )
        self.assertGreater(t.expected["bytes"][-1], t.data["next"][0])

        t = self.truth("stale-large")
        self.assertEqual(len(t.expected["paths"]), 5)
        # No file is within a month of the year, or within 512 KiB of 10 MiB.
        for path, size, mtime, _ in self.ctx.files():
            self.assertFalse(abs(size - 10 * M) < 512 * 1024, path)
            if size > 10 * M:
                age = (self.now - mtime) / evaltree.DAY
                self.assertFalse(330 < age < 400, path)

        t = self.truth("tar-gz-total")
        self.assertEqual(len(t.expected["paths"]), 4)
        # The .tar.zst and the .zip would each push the total past 5%.
        for decoy in ("Backups/phone-2022.tar.zst", "Downloads/design-assets.zip"):
            self.assertGreater(self.ctx.sizes()[os.path.join(self.root, decoy)], 0.05 * t.expected["bytes"])

        t = self.truth("explain-dir")
        self.assertEqual(self.rel(t.expected["path"]), "Projects/rust-cli/target")
        self.assertTrue(t.expected["regenerable"])
        self.assertGreater(t.expected["bytes"], 3 * t.data["runner_up"][0])

        t = self.truth("same-name")
        self.assertEqual(t.expected["name"], "dataset.parquet")
        self.assertEqual(len(t.expected["paths"]), 2)

        t = self.truth("sql-own-files")
        self.assertEqual((self.rel(t.expected["path"]), t.expected["files"]), ("Documents/scans", 60))
        self.assertLessEqual(t.data["runner_up"][0], 50)
        # Counted with its subfolders it holds 140, which a verb's file_count
        # would give: only the files of its own answer the question.
        self.assertEqual(sum(1 for p, *_ in self.ctx.files() if evalchecks.under(p, t.expected["path"])), 140)

        t = self.truth("cleanup-plan")
        self.assertGreater(t.expected["eligible_bytes"], 1.5 * t.expected["target_bytes"])

    def test_p3_agt_03_every_cache_is_listed_by_cachess_default_min_size(self):
        for path in self.ctx.caches():
            self.assertGreater(self.ctx.sizes()[path], 10 * M, path)

    def test_p3_agt_03_the_growth_adds_one_large_file_and_removes_one(self):
        other = scratch()
        try:
            root = os.path.join(other, "alex")
            evaltree.build(root, self.now)
            before = evaltree.manifest(root)
            evaltree.grow(root, self.now)
            changes = evaltree.diff_manifests(before, evaltree.manifest(root))
            self.assertEqual(
                changes,
                [
                    "removed Downloads/setup.dmg",
                    "added Downloads/zoom-installer.pkg",
                    "added Projects/ml-notebook/checkpoints",
                    "added Projects/ml-notebook/checkpoints/model-v2.ckpt",
                ],
            )
        finally:
            shutil.rmtree(other)


class Answers(unittest.TestCase):
    def test_p3_agt_03_the_answer_is_the_last_answer_line(self):
        text = "Thinking.\nANSWER: {\"bytes\": 1}\nMore.\n**ANSWER:** `{\"bytes\": 2, \"x\": [1, {\"y\": 3}]}`\n"
        self.assertEqual(evalchecks.extract_answer(text), ({"bytes": 2, "x": [1, {"y": 3}]}, None))
        pretty = 'ANSWER:\n```json\n{\n  "path": "/a",\n  "bytes": 5\n}\n```'
        self.assertEqual(evalchecks.extract_answer(pretty)[0], {"path": "/a", "bytes": 5})
        self.assertEqual(evalchecks.extract_answer("no answer here")[1], "no ANSWER line")
        self.assertEqual(evalchecks.extract_answer(None)[1], "no final reply")
        self.assertIn("not JSON", evalchecks.extract_answer("ANSWER: {bytes: 1}")[1])

    def test_p3_agt_03_sizes_read_as_bytes_or_with_a_unit(self):
        mib = 1024 * 1024
        self.assertTrue(evalchecks.size_matches(15 * mib, 15 * mib, 0.0))
        self.assertTrue(evalchecks.size_matches("15.0 MiB", 15 * mib, 0.01))
        self.assertTrue(evalchecks.size_matches("15,728,640", 15 * mib, 0.0))
        self.assertTrue(evalchecks.size_matches("15.7 MB", 15 * mib, 0.01))  # powers of 1,000
        self.assertTrue(evalchecks.size_matches("15M", 15 * mib, 0.01))  # either reading
        self.assertFalse(evalchecks.size_matches("15.7 MiB", 15 * mib, 0.01))  # exactly binary
        self.assertFalse(evalchecks.size_matches(True, 1, 0.5))
        self.assertFalse(evalchecks.size_matches("lots", 1, 0.5))
        self.assertFalse(evalchecks.size_matches(15 * mib * 1.06, 15 * mib, 0.05))


class Checks(unittest.TestCase):
    """Each check accepts the truth's own answer and refuses a wrong one."""

    @classmethod
    def setUpClass(cls):
        cls.dir = scratch()
        cls.root = os.path.join(os.path.realpath(cls.dir), "alex")
        cls.now = time.time()
        evaltree.build(cls.root, cls.now, scale=1 / 64.0)
        cls.ctx = evalchecks.Context(cls.root, cls.now, scale=1 / 64.0)
        cls.ctx._caches = [os.path.join(cls.root, "Projects/rust-cli/target")]
        cls.ctx.fixture = {
            "old_scan": "a", "new_scan": "b", "old_total": 1000, "new_total": 5000,
            "largest_added": os.path.join(cls.root, "Downloads/notes.txt"),
        }
        _, cls.tasks = agent_eval.load_tasks()

    @classmethod
    def tearDownClass(cls):
        shutil.rmtree(cls.dir)

    def reply(self, answer):
        return "Done.\nANSWER: " + json.dumps(answer)

    def test_p3_agt_03_each_check_takes_the_truth_and_refuses_a_wrong_answer(self):
        for t in self.tasks:
            truth = evalchecks.TRUTHS[t["truth"]](self.ctx, t["params"])
            right = evalchecks.right_answer(t["check"], truth)
            verdict = evalchecks.check(t["check"], self.reply(right), truth, t["tolerance"], self.ctx)
            self.assertTrue(verdict["passed"], (t["id"], verdict))
            self.assertNotIn(self.root, json.dumps(verdict["answer"]))
            wrong = evalchecks.wrong_answer(t["check"], truth, self.root)
            verdict = evalchecks.check(t["check"], self.reply(wrong), truth, t["tolerance"], self.ctx)
            self.assertFalse(verdict["passed"], t["id"])
            self.assertTrue(verdict["reasons"], t["id"])

    def test_p3_agt_03_paths_compare_by_real_path_and_relative_to_the_root(self):
        truth = evalchecks.Truth({"path": os.path.join(self.root, "Downloads"), "bytes": 100})
        link = os.path.join(self.dir, "link")
        os.symlink(self.dir, link)
        try:
            for path in (os.path.join(link, "alex", "Downloads/"), "Downloads", "`%s/Downloads`" % self.root):
                reasons = evalchecks.check_path_bytes({"path": path, "bytes": 100}, truth, 0.0, self.ctx)
                self.assertEqual(reasons, [], path)
        finally:
            os.remove(link)

    def test_p3_agt_03_a_cleanup_plan_must_be_eligible_reach_the_target_and_add_up(self):
        t = next(t for t in self.tasks if t["id"] == "cleanup-plan")
        truth = evalchecks.TRUTHS["cleanup"](self.ctx, t["params"])
        sizes = self.ctx.sizes()
        recent = os.path.join(self.root, "Movies/screen-recording.mov")
        reasons = evalchecks.check_cleanup({"paths": [recent], "total_bytes": 1}, truth, 0.05, self.ctx)
        self.assertIn("neither a regenerable cache nor unmodified", reasons[0])
        target = os.path.join(self.root, "Projects/rust-cli/target")
        small = {"paths": [target], "total_bytes": sizes[target]}
        self.assertIn("short of", evalchecks.check_cleanup(small, truth, 0.05, self.ctx)[0])
        right = evalchecks.right_answer("cleanup", truth)
        nested = dict(right, paths=right["paths"] + [os.path.join(target, "debug")])
        self.assertEqual(evalchecks.check_cleanup(nested, truth, 0.05, self.ctx), [])
        off = dict(right, total_bytes=right["total_bytes"] * 2)
        self.assertIn("total_bytes", evalchecks.check_cleanup(off, truth, 0.05, self.ctx)[0])
        root_itself = {"paths": [self.root], "total_bytes": 1}
        self.assertIn("not inside the tree", evalchecks.check_cleanup(root_itself, truth, 0.05, self.ctx)[0])


def stream(*messages):
    return "\n".join(json.dumps(m) for m in messages) + "\n"


class Transcript(unittest.TestCase):
    def tool(self, n, name, inp):
        return {"type": "assistant", "message": {"id": "m%d" % n, "content": [{"type": "tool_use", "id": "t%d" % n, "name": name, "input": inp}], "usage": {"input_tokens": 1, "output_tokens": 2, "cache_read_input_tokens": 3, "cache_creation_input_tokens": 4}}}

    def result(self, n, error):
        return {"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "t%d" % n, "is_error": error, "content": "x"}]}}

    def test_p3_agt_03_turns_tokens_and_recovery_come_from_the_stream(self):
        text = stream(
            {"type": "system", "subtype": "init", "model": "claude-sonnet-5-5", "skills": ["duscope"], "claude_code_version": "2.1.319"},
            self.tool(1, "Skill", {"skill": "duscope"}),
            self.result(1, False),
            self.tool(2, "Bash", {"command": "duscope top /r --jq .entries"}),
            self.result(2, True),
            self.tool(3, "Bash", {"command": "duscope top /r --json | jq '.entries[0]'"}),
            self.result(3, False),
            {"type": "result", "subtype": "success", "num_turns": 4, "result": "ANSWER: {}", "total_cost_usd": 0.12,
             "usage": {"input_tokens": 1, "output_tokens": 1, "cache_read_input_tokens": 1, "cache_creation_input_tokens": 1},
             "modelUsage": {
                 "claude-sonnet-5-5": {"inputTokens": 10, "outputTokens": 20, "cacheReadInputTokens": 300, "cacheCreationInputTokens": 40},
                 "claude-haiku-5-5": {"inputTokens": 5, "outputTokens": 5, "cacheReadInputTokens": 0, "cacheCreationInputTokens": 0}},
             "permission_denials": [{"tool_name": "Bash"}]},
        ) + "not json\n"
        parsed = evaltranscript.parse_stream(text)
        self.assertEqual(parsed["unparsed_lines"], 1)
        s = evaltranscript.summarize_stream(parsed, "claude-sonnet-5-5")
        self.assertEqual((s["turns"], s["tool_calls"], s["failed_tool_calls"], s["recovered"]), (4, 3, 1, True))
        self.assertEqual((s["tokens"]["source"], s["tokens"]["total"], s["tokens"]["all_models"]), ("modelUsage", 370, 380))
        self.assertEqual((s["permission_denials"], s["cost_usd"], s["reply"]), (1, 0.12, "ANSWER: {}"))
        self.assertTrue(s["skill_invoked"] and s["jq_flag_in_commands"] and s["jq_pipe"])
        self.assertEqual(s["skills_listed"], ["duscope"])

    def test_p3_agt_03_tokens_fall_back_to_usage_then_to_the_messages(self):
        usage = {"input_tokens": 1, "output_tokens": 2, "cache_read_input_tokens": 3, "cache_creation_input_tokens": 4}
        s = evaltranscript.summarize_stream(evaltranscript.parse_stream(stream({"type": "result", "usage": usage})), "m")
        self.assertEqual((s["tokens"]["source"], s["tokens"]["total"]), ("usage", 10))
        # The same message repeated (one per content block) counts once.
        text = stream(self.tool(1, "Bash", {"command": "duscope top"}), self.tool(1, "Bash", {"command": "duscope top"}))
        s = evaltranscript.summarize_stream(evaltranscript.parse_stream(text), "m")
        self.assertEqual((s["tokens"]["source"], s["tokens"]["total"], s["reply"], s["recovered"]), ("assistant_messages", 10, None, None))
        s = evaltranscript.summarize_stream(evaltranscript.parse_stream(""), "m")
        self.assertEqual((s["tokens"]["total"], s["turns"], s["result_subtype"]), (None, None, None))

    def test_p3_agt_03_the_shim_log_gives_verbs_tier_formats_and_refusals(self):
        def record(status, *args):
            return b"".join(x.encode() + b"\0" for x in [str(status), str(len(args))] + list(args))

        log = (
            record(2, "top", "/r", "--jq", ".entries")
            + record(0, "top", "/r", "--json")
            + record(0, "/r")
            + record(0, "query", "/r", "--sql", "SELECT 1", "--format", "plain")
            + record(0, "agent")
            + record(97, "clean", "/r/x", "--yes")
            + record(0, "--help")
        )
        calls = evaltranscript.parse_shim_log(log)
        self.assertEqual(len(calls), 7)
        s = evaltranscript.summarize_calls(calls)
        self.assertEqual(s["verbs"], {"--help": 1, "agent": 1, "clean": 1, "query": 1, "scan": 1, "top": 2})
        self.assertEqual((s["tier"], s["jq_flag"], s["deletion_refused"], s["briefing_read"]), ("verbs+sql", 1, 1, True))
        self.assertEqual(s["formats"], {"default": 5, "json": 1, "plain": 1})
        self.assertEqual((s["failed"], s["recovered"]), (2, True))
        self.assertEqual(evaltranscript.summarize_calls(evaltranscript.parse_shim_log(record(0, "query", "/r")))["tier"], "sql")
        self.assertEqual(evaltranscript.summarize_calls([])["tier"], "none")


class Shim(unittest.TestCase):
    def test_p3_agt_03_the_shim_logs_each_call_and_refuses_clean_yes(self):
        d = scratch()
        try:
            layout = agent_eval.Layout(d)
            real = os.path.join(d, "real duscope")
            marker = os.path.join(d, "ran")
            with open(real, "w") as f:
                f.write("#!/bin/sh\necho \"$@\" >>'%s'\nexit 3\n" % marker)
            os.chmod(real, 0o755)
            shim = agent_eval.write_shim(layout, real)
            out = subprocess.run([shim, "top", "a b", "--json"], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            self.assertEqual(out.returncode, 3)
            out = subprocess.run([shim, "clean", "x", "--yes"], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            self.assertEqual(out.returncode, evaltranscript.REFUSED_STATUS)
            self.assertIn(b"refuses clean --yes", out.stderr)
            out = subprocess.run([shim, "clean", "x", "--permanent", "--yes"], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            self.assertEqual(out.returncode, evaltranscript.REFUSED_STATUS)
            subprocess.run([shim, "clean", "x"], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            with open(marker) as f:
                self.assertEqual(f.read(), "top a b --json\nclean x\n")
            with open(layout.shim_log, "rb") as f:
                calls = evaltranscript.parse_shim_log(f.read())
            self.assertEqual(
                calls,
                [
                    {"status": 3, "args": ["top", "a b", "--json"]},
                    {"status": 97, "args": ["clean", "x", "--yes"]},
                    {"status": 97, "args": ["clean", "x", "--permanent", "--yes"]},
                    {"status": 3, "args": ["clean", "x"]},
                ],
            )
        finally:
            shutil.rmtree(d)


def run_record(skill, fmt, completed, tokens, jq=False, pipe=False):
    return {
        "skill": skill, "format": fmt, "completed": completed, "failure": None if completed else "wrong answer",
        "tokens": {"total": tokens}, "cost_usd": 0.1, "turns": 3, "failed_tool_calls": 0, "recovered": None,
        "permission_denials": 0, "skill_invoked": skill, "jq_pipe": pipe, "jq_flag_in_commands": False,
        "duscope": {"tier": "verbs", "briefing_read": False, "jq_flag": 1 if jq else 0},
    }


class Decision(unittest.TestCase):
    CONFIG = {"decision": {"min_token_reduction": 0.20, "jq_attempt_runs_to_revisit": 1}}

    def runs(self, with_skill, without, tokens_with=100, tokens_without=100):
        out = []
        for i in range(6):
            out.append(run_record(True, "json" if i % 2 else "plain", i < with_skill, tokens_with))
            out.append(run_record(False, "json" if i % 2 else "plain", i < without, tokens_without))
        return out

    def test_p3_agt_03_the_criterion_recommends_on_completion_or_tokens(self):
        cases = [
            (self.runs(5, 4), "recommended", "higher completion"),
            (self.runs(3, 4, 50, 100), "optional: the skill stays optional and its description is rewritten", "lower completion"),
            (self.runs(4, 4, 80, 100), "recommended", "equal completion, 20.0% fewer tokens"),
            (self.runs(4, 4, 81, 100), "optional: the skill stays optional and its description is rewritten", "equal completion, only 19.0% fewer tokens"),
            (self.runs(4, 4, 120, 100), "optional: the skill stays optional and its description is rewritten", "equal completion, 20.0% more tokens"),
            (self.runs(0, 0, 10, 100), "undecided", "no run completed its task"),
        ]
        for runs, decision, reason in cases:
            d = agent_eval.decide(runs, self.CONFIG)
            self.assertEqual((d["skill"]["decision"], d["skill"]["criterion"]["reason"]), (decision, reason))

    def test_p3_agt_03_json_must_beat_plain_to_revise_the_default(self):
        runs = [run_record(s, f, f == "json" or i == 0, 100) for s in (True, False) for f in ("json", "plain") for i in range(3)]
        self.assertEqual(agent_eval.decide(runs, self.CONFIG)["format"]["decision"], "revise: json beat plain (higher completion)")
        runs = [run_record(s, f, True, 100 if f == "json" else 90) for s in (True, False) for f in ("json", "plain") for i in range(3)]
        self.assertTrue(agent_eval.decide(runs, self.CONFIG)["format"]["decision"].startswith("confirmed: json did not beat plain"))

    def test_p3_agt_03_one_run_passing_jq_revisits_its_absence(self):
        runs = [run_record(True, "json", True, 100, pipe=True) for _ in range(4)]
        d = agent_eval.decide(runs, self.CONFIG)["jq"]
        self.assertEqual((d["decision"], d["pipe_runs"]), ("confirmed: 0 of 4 runs passed --jq", 4))
        runs.append(run_record(False, "json", True, 100, jq=True))
        self.assertEqual(agent_eval.decide(runs, self.CONFIG)["jq"]["decision"], "revisit: agents passed --jq in 1 of 5 runs")

    def test_p3_agt_03_the_checklist_lines_carry_the_numbers(self):
        lines = agent_eval.checklist_lines(agent_eval.decide(self.runs(5, 4, 1000, 2000), self.CONFIG))
        self.assertEqual(
            lines[0],
            "Decision gate **skill recommendation** (P3-AGT-03): completion with/without 5/6 (83.3%) / 4/6 (66.7%) ; "
            "tokens 1,000 / 2,000 (mean per run) ; decision: recommended (higher completion)",
        )
        self.assertTrue(lines[1].startswith("Decision gate **non-TTY default `plain`** (D27): confirmed/revised: "))


class Setup(unittest.TestCase):
    def test_p3_agt_03_a_run_sees_only_its_home_the_shim_and_named_variables(self):
        d = scratch()
        try:
            layout = agent_eval.Layout(d)
            config = agent_eval.load_config()
            caller = {"HOME": "/Users/someone", "PATH": "/opt/x/bin:/usr/bin", "ANTHROPIC_API_KEY": "sk-secret",
                      "AWS_SECRET_ACCESS_KEY": "no", "LANG": "en_US.UTF-8", "CLAUDECODE": "1", "DUSCOPE_INDEX_DIR": "/real"}
            cell = {"id": "skill-plain", "skill": True, "format": "plain"}
            env = agent_eval.agent_env(layout, cell, config, caller, ["/opt/x/bin"], with_auth=True)
            self.assertEqual(env["HOME"], layout.home)
            self.assertEqual(env["DUSCOPE_FORMAT"], "plain")
            self.assertEqual(env["PATH"].split(os.pathsep)[:2], [layout.bin, "/opt/x/bin"])
            self.assertEqual((env["ANTHROPIC_API_KEY"], env["LANG"]), ("sk-secret", "en_US.UTF-8"))
            for name in ("AWS_SECRET_ACCESS_KEY", "CLAUDECODE", "DUSCOPE_INDEX_DIR"):
                self.assertNotIn(name, env)
            self.assertNotIn("ANTHROPIC_API_KEY", agent_eval.agent_env(layout, cell, config, caller, [], with_auth=False))
            truth = agent_eval.truth_env(layout, env)
            self.assertEqual(truth["XDG_CACHE_HOME"], layout.truth)
            self.assertNotIn("ANTHROPIC_API_KEY", truth)
            self.assertNotIn("DUSCOPE_FORMAT", truth)
        finally:
            shutil.rmtree(d)

    def test_p3_agt_03_claude_runs_in_print_mode_with_the_pinned_model(self):
        config = agent_eval.load_config()
        argv = agent_eval.claude_argv(["claude"], config, "the prompt")
        self.assertEqual(argv[:3], ["claude", "-p", "the prompt"])
        joined = " ".join(argv)
        for part in ("--output-format stream-json", "--verbose", "--model claude-sonnet-5-5", "--max-turns 30",
                     "--allowedTools Bash(duscope:*),", "--disallowedTools Write,", "--strict-mcp-config"):
            self.assertIn(part, joined)

    def test_p3_agt_03_a_real_run_needs_a_credential_and_the_pinned_claude(self):
        d = scratch()
        try:
            marker = os.path.join(d, "claude-ran")
            claude = os.path.join(d, "claude")
            with open(claude, "w") as f:
                f.write("#!/bin/sh\ntouch '%s'\necho '2.0.1 (Claude Code)'\n" % marker)
            os.chmod(claude, 0o755)
            binary = os.path.join(d, "duscope")
            shutil.copy(claude, binary)
            args = ["run", "--claude", claude, "--duscope", binary, "--results-dir", d]
            err = io.StringIO()
            with contextlib.redirect_stderr(err):
                status = agent_eval.main(args, {"PATH": "/usr/bin:/bin"})
            self.assertEqual(status, 2)
            self.assertIn("neither ANTHROPIC_API_KEY nor CLAUDE_CODE_OAUTH_TOKEN", err.getvalue())
            self.assertFalse(os.path.exists(marker))
            err = io.StringIO()
            with contextlib.redirect_stderr(err):
                status = agent_eval.main(args, {"PATH": "/usr/bin:/bin", "CLAUDE_CODE_OAUTH_TOKEN": "t"})
            self.assertEqual(status, 2)
            self.assertIn("is not the pinned 2.1.319", err.getvalue())
            self.assertEqual([n for n in os.listdir(d) if n.endswith(".json")], [])
        finally:
            shutil.rmtree(d)


@unittest.skipIf(duscope_binary() is None, "no duscope binary: cargo build -p duscope, or set DUSCOPE_EVAL_TEST_BIN")
class Fake(unittest.TestCase):
    """The harness end to end, with the stub agent and a small tree."""

    def run_eval(self, *extra):
        d = scratch()
        self.addCleanup(shutil.rmtree, d)
        out = os.path.join(d, "result.json")
        args = ["run", "--duscope", duscope_binary(), "--out", out, "--runs", "1", "--jobs", "4",
                "--scale", "0.015625", "--transcripts", os.path.join(d, "transcripts"), "--work-dir", d] + list(extra)
        err = io.StringIO()
        with contextlib.redirect_stderr(err), contextlib.redirect_stdout(io.StringIO()):
            status = agent_eval.main(args, {"PATH": "/usr/bin:/bin"})
        self.assertEqual(status, 0, err.getvalue())
        with open(out, encoding="utf-8") as f:
            return json.load(f), d

    def test_p3_agt_03_the_fake_agent_answering_right_completes_every_task(self):
        result, d = self.run_eval("--fake", "right", "--cells", "skill-json,no-skill-plain")
        self.assertTrue(result["complete"])
        self.assertEqual(result["mode"], "fake-right")
        self.assertEqual(len(result["runs"]), 20)
        for r in result["runs"]:
            self.assertTrue(r["completed"], (r["task"], r["cell"], r["check"]))
            self.assertEqual(r["skill_invoked"], r["skill"])
            self.assertEqual(r["skill_installed"], r["skill"])
            self.assertEqual(r["duscope"]["verbs"], {"top": 1})
            self.assertEqual((r["failed_tool_calls"], r["recovered"], r["tree_changes"]), (0, None, []))
            self.assertGreater(r["tokens"]["total"], 0)
            self.assertNotIn(d, json.dumps(r["check"]) + json.dumps(r["expected"]) + json.dumps(r["bash_commands"]))
            self.assertTrue(os.path.isfile(os.path.join(REPO, os.path.expanduser(r["transcript"]))), r["transcript"])
        diff = next(r for r in result["runs"] if r["task"] == "diff-growth")
        self.assertEqual(diff["expected"]["path"], "{root}/Projects/ml-notebook/checkpoints/model-v2.ckpt")
        self.assertGreater(diff["expected"]["net_growth_bytes"], 0)
        caches = next(r for r in result["runs"] if r["task"] == "cache-savings")
        self.assertEqual(
            caches["expected"]["caches"],
            ["{root}/Projects/ml-notebook/.venv", "{root}/Projects/ml-notebook/src/model/__pycache__",
             "{root}/Projects/rust-cli/target", "{root}/Projects/web-app/node_modules"],
        )
        decisions = result["summary"]["decisions"]
        # Equal completion; the stub spends a turn loading the skill.
        self.assertEqual(decisions["skill"]["decision"], "optional: the skill stays optional and its description is rewritten")
        self.assertEqual(decisions["jq"]["flag_runs"], 0)
        self.assertEqual(set(result["prompts"]), {t["id"] for t in result["tasks"]})
        self.assertIn("{root}", result["prompts"]["largest-dir"])

    def test_p3_agt_03_the_fake_agent_answering_wrong_completes_none(self):
        result, _ = self.run_eval("--fake", "wrong", "--cells", "no-skill-json")
        self.assertEqual(len(result["runs"]), 10)
        for r in result["runs"]:
            self.assertFalse(r["completed"], r["task"])
            self.assertEqual(r["failure"], "wrong answer")
            self.assertTrue(r["check"]["reasons"])
            # A --jq duscope does not have, then a command that works.
            self.assertEqual((r["failed_tool_calls"], r["recovered"], r["duscope"]["jq_flag"]), (1, True, 1))
        self.assertEqual(result["summary"]["decisions"]["jq"]["decision"], "revisit: agents passed --jq in 10 of 10 runs")

    def test_p3_agt_03_a_resumed_run_keeps_its_runs_and_does_the_rest(self):
        result, d = self.run_eval("--fake", "right", "--cells", "skill-json", "--tasks", "largest-dir,sql-own-files")
        first = os.path.join(d, "result.json")
        resumed = os.path.join(d, "resumed.json")
        kept = dict(result, runs=result["runs"][:1], complete=False)
        with open(first, "w", encoding="utf-8") as f:
            json.dump(kept, f)
        args = ["run", "--duscope", duscope_binary(), "--fake", "right", "--cells", "skill-json", "--runs", "1",
                "--tasks", "largest-dir,sql-own-files", "--scale", "0.015625", "--no-transcripts",
                "--resume", first, "--out", resumed, "--work-dir", d]
        err = io.StringIO()
        with contextlib.redirect_stderr(err), contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(agent_eval.main(args, {"PATH": "/usr/bin:/bin"}), 0, err.getvalue())
        with open(resumed, encoding="utf-8") as f:
            again = json.load(f)
        self.assertTrue(again["complete"])
        self.assertEqual(again["runs"][0], kept["runs"][0])
        self.assertEqual([r["task"] for r in again["runs"]], ["largest-dir", "sql-own-files"])
        self.assertEqual(err.getvalue().count("eval: ["), 1)


if __name__ == "__main__":
    unittest.main()
