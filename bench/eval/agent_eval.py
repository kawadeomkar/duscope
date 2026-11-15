"""The ten-task agent eval (P3-AGT-03): `bench/eval/run.sh` runs this.

    run [options]     run every task in every cell through Claude Code
    plan [options]    print the tasks, the cells, the run count and the limits
    summary FILE      print a result file's table and its decisions
    tree DIR          build the eval tree in DIR and print each task's answer

Each run gets a directory of its own: a scratch HOME (with the duscope skill
written by `duscope agent --skill` in the cells that have it), the tree the
tasks ask about, built fresh, and a `duscope` on PATH that logs each call and
refuses `clean --yes` and `--allow-delete`. Claude Code runs headless in it (`claude -p` with
stream-json output), and the run records whether the answer was right (the
task's check against the truth worked out from the tree), the tokens, the
turns and whether a failed tool call was followed by one that worked. The
result file (`results/<date>.json`) holds every run, each cell's figures and
the decisions the criterion gives. `--fake right|wrong` runs a stub in place
of Claude Code that answers from the truth, or wrongly, so the harness is
tested with no network and no account. bench/README.md documents the options.

Standard library only; Python 3.9 or later.
"""

from __future__ import annotations

import argparse
import concurrent.futures
import datetime
import hashlib
import json
import os
import platform as platform_module
import shlex
import shutil
import signal
import statistics
import subprocess
import sys
import tempfile
import threading
import time
from typing import Any, Dict, List, Optional, Tuple

if sys.version_info < (3, 9):  # pragma: no cover - the message is the point
    sys.exit("eval: Python 3.9 or later is needed")

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))
if HERE not in sys.path:
    sys.path.insert(0, HERE)

import evalchecks  # noqa: E402
import evaltranscript  # noqa: E402
import evaltree  # noqa: E402
import minitoml  # noqa: E402

FAKE_CLAUDE = os.path.join(HERE, "fake_claude.py")
CONFIG = os.path.join(HERE, "config.toml")
TASKS = os.path.join(HERE, "tasks.toml")
RESULTS = os.path.join(HERE, "results")
SCHEMA_VERSION = 1

# Credentials Claude Code reads from the environment: an API key, or the
# long-lived token `claude setup-token` prints for a Claude subscription.
AUTH_VARS = ("ANTHROPIC_API_KEY", "CLAUDE_CODE_OAUTH_TOKEN")
FORMATS = ("json", "plain", "table")
FIXTURES = ("tree", "two-scans")
# The tree's root inside a run's directory: a home directory's name.
TREE_NAME = "alex"
# The tests' knob for the stub agent (fake_claude.py): comma-separated acts,
# each `ACT` for every run or `TASK:ACT` for one task's runs.
FAKE_ACTS_VAR = "DUSCOPE_TEST_EVAL_FAKE"
FAKE_ACTS = ("hang", "api-error", "crash", "model", "no-skills", "bare")
# The failure of a run that measured the infrastructure, not the agent
# (evaltranscript.infrastructure): recorded, left out of every figure, and
# made again by --resume. This many in a row stop the eval.
INFRASTRUCTURE = "infrastructure"
STOP_AFTER_OUTAGES = 3

# The duscope on the agent's PATH. It runs a copy of the binary under test in
# the run's directory, so that its text names no checkout (where
# evaltree.py holds every answer), and appends the exit status, the argument
# count and the arguments, NUL-separated, to the log. It refuses `clean`
# with `--yes` (without it, `--permanent` too, clean only shows its plan)
# and `--allow-delete`, with which `mcp` offers a client the tool that moves
# paths to the Trash: no duscope call of a run moves anything to the Trash
# (which on macOS is the real account's). Past the shim, a run is held only
# by Claude Code's matching of `Bash(duscope:*)` (bench/README.md).
SHIM = """#!/bin/sh
# duscope as the agent runs it in the eval (bench/eval/agent_eval.py): the
# binary under test, each call logged; clean --yes and --allow-delete refused.
real={real}
log={log}
clean=0
yes=0
allow=0
for arg in "$@"; do
  case $arg in
    clean) clean=1 ;;
    --yes) yes=1 ;;
    --allow-delete|--allow-delete=*) allow=1 ;;
  esac
done
if [ "$clean" = 1 ] && [ "$yes" = 1 ]; then
  echo 'duscope-eval: this evaluation refuses clean --yes; nothing was moved' >&2
  status={refused}
elif [ "$allow" = 1 ]; then
  echo 'duscope-eval: this evaluation refuses --allow-delete; nothing was started' >&2
  status={refused}
else
  "$real" "$@"
  status=$?
fi
printf '%s\\0' "$status" "$#" "$@" >>"$log"
exit "$status"
"""


class EvalError(RuntimeError):
    """A problem with the configuration, the tasks or the setup: exit 2."""


class CutShort(Exception):
    """A run the eval stopped before it ended: it is not recorded."""


class Agents:
    """The agents running now, so that a stop can end them. Each runs in a
    session of its own, which the terminal's Ctrl-C does not reach, so the
    eval ends each one's process group itself; once it is stopping, no run
    starts an agent."""

    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._running: Dict[int, subprocess.Popen] = {}
        self._cut: set = set()
        self.stopping = False

    def start(self, argv: List[str], **kw: Any) -> subprocess.Popen:
        with self._lock:
            if self.stopping:
                raise CutShort()
            proc = subprocess.Popen(argv, start_new_session=True, **kw)
            self._running[proc.pid] = proc
            return proc

    def finish(self, proc: subprocess.Popen) -> None:
        """Forgets an agent that has ended; raises CutShort when the stop
        ended it."""
        with self._lock:
            self._running.pop(proc.pid, None)
            if proc.pid in self._cut:
                raise CutShort()

    def stop(self) -> None:
        with self._lock:
            self.stopping = True
            for pid, proc in self._running.items():
                self._cut.add(pid)
                _kill_group(proc, signal.SIGKILL)


# -- configuration and tasks ---------------------------------------------------


def load_config(path: str = CONFIG) -> Dict[str, Any]:
    config = minitoml.load(path)

    def need(ok: bool, what: str) -> None:
        if not ok:
            raise EvalError("%s: missing or bad setting %s" % (path, what))

    try:
        cc, cells, decision = config["claude_code"], config["cells"], config["decision"]
        need(isinstance(config["model"]["id"], str) and bool(config["model"]["id"]), "model.id")
        need(isinstance(cc["version"], str) and bool(cc["version"]), "claude_code.version")
        for key in ("max_turns", "timeout_s", "max_tokens"):
            need(isinstance(cc[key], int) and cc[key] > 0, "claude_code." + key)
        for key in ("allowed_tools", "disallowed_tools", "pass_env"):
            need(isinstance(cc[key], list) and all(isinstance(v, str) for v in cc[key]), "claude_code." + key)
        need(bool(cells["skill"]) and all(isinstance(v, bool) for v in cells["skill"]), "cells.skill")
        need(bool(cells["format"]) and all(v in FORMATS for v in cells["format"]), "cells.format")
        need(isinstance(cells["runs"], int) and cells["runs"] > 0, "cells.runs")
        need(0 < float(decision["min_token_reduction"]) < 1, "decision.min_token_reduction")
        margin = decision["completion_margin_runs"]
        need(isinstance(margin, int) and not isinstance(margin, bool) and margin >= 0, "decision.completion_margin_runs")
        need(isinstance(decision["jq_attempt_runs_to_revisit"], int), "decision.jq_attempt_runs_to_revisit")
    except (KeyError, TypeError) as e:
        raise EvalError("%s: missing or bad setting %s" % (path, e))
    return config


def load_tasks(path: str = TASKS) -> Tuple[Dict[str, Any], List[Dict[str, Any]]]:
    data = minitoml.load(path)
    if data.get("format") != 1:
        raise EvalError("%s: format must be 1" % path)
    for key in ("preamble", "answer_rule"):
        if not isinstance(data.get(key), str):
            raise EvalError("%s: %s is missing" % (path, key))
    tasks = data.get("task") or []
    seen = set()
    for t in tasks:
        for key in ("id", "title", "row", "question", "answer"):
            if not isinstance(t.get(key), str) or not t[key]:
                raise EvalError("%s: a task without %s" % (path, key))
        if t["id"] in seen:
            raise EvalError("%s: task %s given twice" % (path, t["id"]))
        seen.add(t["id"])
        if t.get("fixture") not in FIXTURES:
            raise EvalError("%s: task %s: fixture must be one of %s" % (path, t["id"], ", ".join(FIXTURES)))
        if t.get("truth") not in evalchecks.TRUTHS:
            raise EvalError("%s: task %s: unknown truth %r" % (path, t["id"], t.get("truth")))
        if t.get("check") not in evalchecks.CHECKS:
            raise EvalError("%s: task %s: unknown check %r" % (path, t["id"], t.get("check")))
        if not isinstance(t.get("tolerance"), (int, float)) or not 0 <= t["tolerance"] < 1:
            raise EvalError("%s: task %s: tolerance must be a fraction" % (path, t["id"]))
        t.setdefault("params", {})
        if not t["answer"].startswith("{"):
            raise EvalError("%s: task %s: answer must show a JSON object" % (path, t["id"]))
    return data, tasks


def cells_of(config: Dict[str, Any]) -> List[Dict[str, Any]]:
    return [
        {"id": "%s-%s" % ("skill" if skill else "no-skill", fmt), "skill": skill, "format": fmt}
        for skill in config["cells"]["skill"]
        for fmt in config["cells"]["format"]
    ]


def number(value: float) -> str:
    """A parameter as the prompt writes it: no trailing zeros."""
    if float(value).is_integer():
        return str(int(value))
    return ("%.3f" % value).rstrip("0").rstrip(".")


def render_prompt(meta: Dict[str, Any], task: Dict[str, Any], root: str, scale: float = 1.0) -> str:
    values = {k: number(v * scale) if k.endswith("_mib") else v for k, v in task["params"].items()}
    values["root"] = root
    return "\n\n".join(
        [
            meta["preamble"].strip().format(**values),
            task["question"].strip().format(**values),
            meta["answer_rule"].strip().format(answer=task["answer"]),
        ]
    )


# -- one run's directory and environment ---------------------------------------


class Layout:
    """A run's directory: home/ (HOME), bin/ (the shim), under-test/ (the
    copy of the binary the shim runs), project/ (the agent's working
    directory), tmp/, disk/alex (the tree), truth/ (the harness's own index
    directory) and out/ (the logs)."""

    def __init__(self, work: str) -> None:
        self.work = os.path.realpath(work)
        self.home = os.path.join(self.work, "home")
        self.bin = os.path.join(self.work, "bin")
        self.binary = os.path.join(self.work, "under-test", "duscope")
        self.project = os.path.join(self.work, "project")
        self.tmp = os.path.join(self.work, "tmp")
        self.disk = os.path.join(self.work, "disk")
        self.root = os.path.join(self.disk, TREE_NAME)
        self.truth = os.path.join(self.work, "truth")
        self.out = os.path.join(self.work, "out")
        self.shim_log = os.path.join(self.out, "duscope-calls.log")
        for d in (self.home, self.bin, os.path.dirname(self.binary), self.project, self.tmp, self.disk, self.truth, self.out):
            os.makedirs(d, exist_ok=True)
        for sub in (".config", ".cache", ".local/share", ".local/state"):
            os.makedirs(os.path.join(self.home, sub), exist_ok=True)


def write_shim(layout: Layout, binary: str) -> str:
    shutil.copy2(binary, layout.binary)
    path = os.path.join(layout.bin, "duscope")
    with open(path, "w", encoding="utf-8") as f:
        f.write(
            SHIM.format(
                real=shlex.quote(layout.binary), log=shlex.quote(layout.shim_log), refused=evaltranscript.REFUSED_STATUS
            )
        )
    os.chmod(path, 0o755)
    open(layout.shim_log, "wb").close()
    return path


def tool_dirs(names: List[str], base_env: Dict[str, str]) -> List[str]:
    """The directories of these commands on the caller's PATH, in order."""
    dirs: List[str] = []
    for name in names:
        found = shutil.which(name, path=base_env.get("PATH", ""))
        if found:
            d = os.path.dirname(os.path.realpath(found)) if name != "claude" else os.path.dirname(found)
            if d not in dirs:
                dirs.append(d)
    return dirs


def agent_env(
    layout: Layout,
    cell: Dict[str, Any],
    config: Dict[str, Any],
    base_env: Dict[str, str],
    extra_dirs: List[str],
    with_auth: bool,
) -> Dict[str, str]:
    """The environment Claude Code (and so each duscope call) runs in: the
    scratch HOME and XDG directories, PATH with the shim first, the cell's
    output format, and only the variables config.toml passes through (plus
    the credential, in a real run). Nothing else of the caller's leaks in."""
    home = layout.home
    system = ["/usr/bin", "/bin", "/usr/sbin", "/sbin"]
    env = {
        "HOME": home,
        "XDG_CONFIG_HOME": os.path.join(home, ".config"),
        "XDG_CACHE_HOME": os.path.join(home, ".cache"),
        "XDG_DATA_HOME": os.path.join(home, ".local", "share"),
        "XDG_STATE_HOME": os.path.join(home, ".local", "state"),
        "TMPDIR": layout.tmp,
        "PATH": os.pathsep.join([layout.bin] + [d for d in extra_dirs if d not in system] + system),
        "DUSCOPE_FORMAT": cell["format"],
        "DISABLE_AUTOUPDATER": "1",
        "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1",
    }
    for name in config["claude_code"]["pass_env"]:
        if name in base_env and name not in env:
            env[name] = base_env[name]
    env.setdefault("SHELL", "/bin/bash" if os.path.exists("/bin/bash") else "/bin/sh")
    if with_auth:
        for name in AUTH_VARS:
            if base_env.get(name):
                env[name] = base_env[name]
    return env


def truth_env(layout: Layout, env: Dict[str, str]) -> Dict[str, str]:
    """The harness's own duscope environment: the run's, with an index
    directory the agent never sees and no credential."""
    out = {k: v for k, v in env.items() if k not in AUTH_VARS and k != "DUSCOPE_FORMAT"}
    out["XDG_CACHE_HOME"] = layout.truth
    return out


def claude_argv(claude: List[str], config: Dict[str, Any], prompt: str) -> List[str]:
    """Claude Code in print mode: stream-json (which needs --verbose), the
    pinned model, the turn limit, the tools allowed without asking (anything
    else is denied, since print mode cannot ask) and none of the caller's MCP
    servers."""
    cc = config["claude_code"]
    return claude + [
        "-p",
        prompt,
        "--output-format",
        "stream-json",
        "--verbose",
        "--model",
        config["model"]["id"],
        "--max-turns",
        str(cc["max_turns"]),
        "--allowedTools",
        ",".join(cc["allowed_tools"]),
        "--disallowedTools",
        ",".join(cc["disallowed_tools"]),
        "--strict-mcp-config",
    ]


def run_duscope(binary: str, args: List[str], env: Dict[str, str], cwd: str) -> Dict[str, Any]:
    """`duscope ARGS --json`, run by the harness: its envelope."""
    proc = subprocess.run(
        [binary] + args + ["--json"],
        cwd=cwd,
        env=env,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=300,
    )
    try:
        envelope = json.loads(proc.stdout.decode("utf-8"))
    except ValueError:
        envelope = None
    if proc.returncode != 0 or not isinstance(envelope, dict) or not envelope.get("ok"):
        raise EvalError(
            "duscope %s failed (exit %d): %s"
            % (" ".join(args), proc.returncode, proc.stderr.decode("utf-8", "replace").strip()[-500:])
        )
    return envelope


def prepare_two_scans(binary: str, env: Dict[str, str], layout: Layout, now: float, scale: float) -> Dict[str, Any]:
    """The diff task's fixture: a saved scan of the tree, the changes, and a
    second saved scan, both in the agent's index directory. The second starts
    in a later second than the first, so `duscope scans` orders them."""
    scan = ["scan", layout.root, "--save", "--treemap", "never"]
    old = run_duscope(binary, scan, env, layout.project)["scan"]
    evaltree.grow(layout.root, now, scale)
    time.sleep(1.1)
    new = run_duscope(binary, scan, env, layout.project)["scan"]
    added = max(evaltree.GROWTH_ADDED, key=lambda a: a[1])[0]
    return {
        "old_scan": old["id"],
        "new_scan": new["id"],
        "old_total": old["total_allocated_bytes"],
        "new_total": new["total_allocated_bytes"],
        "largest_added": os.path.join(os.path.realpath(layout.root), added),
    }


# -- one run ---------------------------------------------------------------------


class Settings:
    """What every run of an invocation shares."""

    def __init__(self, **kw: Any) -> None:
        self.config: Dict[str, Any] = kw["config"]
        self.meta: Dict[str, Any] = kw["meta"]
        self.binary: str = kw["binary"]
        self.claude: List[str] = kw["claude"]
        self.fake: Optional[str] = kw["fake"]
        self.scale: float = kw["scale"]
        self.base_env: Dict[str, str] = kw["base_env"]
        self.extra_dirs: List[str] = kw["extra_dirs"]
        self.work_dir: Optional[str] = kw["work_dir"]
        self.keep_work: bool = kw["keep_work"]
        self.transcripts: Optional[str] = kw["transcripts"]
        self.fake_acts: List[Tuple[Optional[str], str]] = kw.get("fake_acts") or []
        self.agents = Agents()


def run_one(s: Settings, task: Dict[str, Any], cell: Dict[str, Any], repeat: int) -> Dict[str, Any]:
    """Sets up one run, runs the agent, checks its answer, and returns the
    record; the run's directory is removed unless --keep-work."""
    work = tempfile.mkdtemp(prefix="duscope-eval-", dir=s.work_dir)
    try:
        return _run_in(work, s, task, cell, repeat)
    finally:
        if not s.keep_work:
            shutil.rmtree(work, ignore_errors=True)


def _run_in(work: str, s: Settings, task: Dict[str, Any], cell: Dict[str, Any], repeat: int) -> Dict[str, Any]:
    if s.agents.stopping:
        raise CutShort()
    layout = Layout(work)
    now = time.time()
    evaltree.build(layout.root, now, s.scale)
    root = os.path.realpath(layout.root)
    env = agent_env(layout, cell, s.config, s.base_env, s.extra_dirs, with_auth=s.fake is None)
    tenv = truth_env(layout, env)

    skill_installed = False
    if cell["skill"]:
        proc = subprocess.run(
            [s.binary, "agent", "--skill", "--dir", layout.home],
            cwd=layout.project,
            env=env,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=60,
        )
        skill_installed = os.path.isfile(os.path.join(layout.home, ".claude", "skills", "duscope", "SKILL.md"))
        if proc.returncode != 0 or not skill_installed:
            raise EvalError("duscope agent --skill failed: %s" % proc.stderr.decode("utf-8", "replace").strip())

    fixture: Dict[str, Any] = {}
    if task["fixture"] == "two-scans":
        fixture = prepare_two_scans(s.binary, env, layout, now, s.scale)

    def duscope_json(args: List[str]) -> Dict[str, Any]:
        return run_duscope(s.binary, args + ["--no-save"], tenv, layout.project)

    ctx = evalchecks.Context(root, now, s.scale, duscope_json, fixture)
    truth = evalchecks.TRUTHS[task["truth"]](ctx, task["params"])
    before = evaltree.manifest(root)
    write_shim(layout, s.binary)
    prompt = render_prompt(s.meta, task, root, s.scale)

    if s.fake:
        answer = (
            evalchecks.right_answer(task["check"], truth)
            if s.fake == "right"
            else evalchecks.wrong_answer(task["check"], truth, root)
        )
        fake_file = os.path.join(layout.out, "fake-answer.json")
        with open(fake_file, "w", encoding="utf-8") as f:
            acts = [act for only, act in s.fake_acts if only in (None, task["id"])]
            json.dump({"mode": s.fake, "root": root, "answer": answer, "acts": acts}, f)
        env = dict(env, DUSCOPE_EVAL_FAKE_ANSWER=fake_file)

    argv = claude_argv(s.claude, s.config, prompt)
    transcript = os.path.join(layout.out, "transcript.jsonl")
    stderr_path = os.path.join(layout.out, "claude.stderr")
    started = time.time()
    timed_out = False
    with open(transcript, "wb") as out, open(stderr_path, "wb") as err:
        proc = s.agents.start(argv, cwd=layout.project, env=env, stdin=subprocess.DEVNULL, stdout=out, stderr=err)
        try:
            proc.wait(timeout=s.config["claude_code"]["timeout_s"])
        except subprocess.TimeoutExpired:
            timed_out = True
            _kill_group(proc, signal.SIGTERM)
            try:
                proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                _kill_group(proc, signal.SIGKILL)
                proc.wait()
        _kill_group(proc, signal.SIGKILL)  # anything the agent left running
    s.agents.finish(proc)
    wall = time.time() - started

    with open(transcript, "r", encoding="utf-8", errors="replace") as f:
        stream = evaltranscript.summarize_stream(evaltranscript.parse_stream(f.read()), s.config["model"]["id"])
    with open(layout.shim_log, "rb") as f:
        duscope = evaltranscript.summarize_calls(evaltranscript.parse_shim_log(f.read()))
    with open(stderr_path, "rb") as f:
        stderr_tail = f.read()[-2000:].decode("utf-8", "replace")
    changes = evaltree.diff_manifests(before, evaltree.manifest(root))
    verdict = evalchecks.check(task["check"], stream["reply"], truth, float(task["tolerance"]), ctx)

    max_tokens = s.config["claude_code"]["max_tokens"]
    total = stream["tokens"]["total"]
    outage = None if timed_out else evaltranscript.infrastructure(stream, proc.returncode, s.config["model"]["id"], cell["skill"])
    # What the agent did to the tree outranks an outage, which --resume
    # would otherwise make again and so hide.
    failure = None
    if timed_out:
        failure = "timeout"
    elif changes:
        failure = "tree changed"
    elif duscope["deletion_refused"]:
        failure = "deletion refused"
    elif outage:
        failure = INFRASTRUCTURE
    elif stream["reply"] is None:
        failure = "no reply"
    elif total is not None and total > max_tokens:
        failure = "over the token limit"
    elif not verdict["passed"]:
        failure = "wrong answer"

    saved = None
    if s.transcripts:
        os.makedirs(s.transcripts, exist_ok=True)
        saved = os.path.join(s.transcripts, "%s--%s--%d.jsonl" % (task["id"], cell["id"], repeat))
        shutil.copyfile(transcript, saved)

    reply = stream.pop("reply")
    commands = stream.pop("bash_commands")
    record: Dict[str, Any] = {
        "task": task["id"],
        "cell": cell["id"],
        "skill": cell["skill"],
        "format": cell["format"],
        "repeat": repeat,
        "completed": failure is None,
        "failure": failure,
        "infrastructure": evalchecks.portable(outage, root) if outage else None,
        "check": verdict,
        "expected": evalchecks.portable(truth.expected, root),
        "exit_status": proc.returncode,
        "timed_out": timed_out,
        "wall_s": round(wall, 3),
        "skill_installed": skill_installed,
    }
    record.update(stream)
    record["reply"] = evalchecks.portable(reply[-2000:], root) if isinstance(reply, str) else None
    record["bash_commands"] = [evalchecks.portable(c, root) for c in commands]
    record["duscope"] = duscope
    record["tree_changes"] = changes
    record["claude_stderr_tail"] = evalchecks.portable(stderr_tail, root) if stderr_tail.strip() else None
    record["transcript"] = shown_path(saved) if saved else None
    if fixture:
        record["fixture"] = {"old_scan": fixture["old_scan"], "new_scan": fixture["new_scan"]}
    return record


def fake_acts(text: str) -> List[Tuple[Optional[str], str]]:
    """DUSCOPE_TEST_EVAL_FAKE read: (task or None for every task, act)."""
    acts: List[Tuple[Optional[str], str]] = []
    for item in (x.strip() for x in text.split(",")):
        if not item:
            continue
        only, _, act = item.rpartition(":")
        if act.split("=", 1)[0] not in FAKE_ACTS:
            raise EvalError("%s: unknown act %r" % (FAKE_ACTS_VAR, act))
        acts.append((only or None, act))
    return acts


def shown_path(path: str) -> str:
    """`path` as a result file keeps it: relative to the checkout when it is
    inside it, else with the home directory written `~`, so that a committed
    file names no account."""
    rel = os.path.relpath(path, REPO)
    if not rel.startswith(".."):
        return rel
    home = os.path.expanduser("~")
    if home not in ("", "/") and evalchecks.under(path, home):
        return "~" + path[len(home) :]
    return path


def _kill_group(proc: subprocess.Popen, sig: int) -> None:
    try:
        os.killpg(proc.pid, sig)
    except (ProcessLookupError, PermissionError):
        pass


# -- the summary and the decisions ----------------------------------------------


def outage(record: Dict[str, Any]) -> bool:
    return record["failure"] == INFRASTRUCTURE


def stats(every: List[Dict[str, Any]]) -> Dict[str, Any]:
    """A set of runs' figures. The runs that failed for the infrastructure
    are counted apart and left out of every figure but the cost."""
    runs = [r for r in every if not outage(r)]
    # Tokens at equal completion: the runs that completed their task alone.
    tokens = [r["tokens"]["total"] for r in runs if r["completed"] and r["tokens"]["total"] is not None]
    costs = [r["cost_usd"] for r in every if isinstance(r.get("cost_usd"), (int, float))]
    turns = [r["turns"] for r in runs if isinstance(r.get("turns"), int)]
    tiers: Dict[str, int] = {}
    for r in runs:
        tiers[r["duscope"]["tier"]] = tiers.get(r["duscope"]["tier"], 0) + 1
    with_failures = [r for r in runs if r["failed_tool_calls"]]
    n = len(runs)
    done = sum(1 for r in runs if r["completed"])
    return {
        "runs": n,
        "infrastructure": len(every) - n,
        "completed": done,
        "completion": round(done / n, 4) if n else None,
        "tokens_mean": round(statistics.mean(tokens), 1) if tokens else None,
        "tokens_median": statistics.median(tokens) if tokens else None,
        "cost_usd": round(sum(costs), 4) if costs else None,
        "turns_mean": round(statistics.mean(turns), 2) if turns else None,
        "runs_with_failed_calls": len(with_failures),
        "recovered_runs": sum(1 for r in with_failures if r["recovered"]),
        "recovered_and_completed": sum(1 for r in with_failures if r["recovered"] and r["completed"]),
        "permission_denials": sum(r["permission_denials"] or 0 for r in runs),
        "skill_invoked_runs": sum(1 for r in runs if r["skill_invoked"]),
        "briefing_read_runs": sum(1 for r in runs if r["duscope"]["briefing_read"]),
        "tiers": dict(sorted(tiers.items())),
        "jq_flag_runs": sum(1 for r in runs if jq_flag(r)),
        "jq_pipe_runs": sum(1 for r in runs if r["jq_pipe"]),
        "default_format_runs": sum(1 for r in runs if took_default(r)),
        "failures": _count(r["failure"] for r in runs if r["failure"]),
    }


def took_default(record: Dict[str, Any]) -> bool:
    """Whether a run called duscope and passed no format in any call, so
    that every answer came in the cell's format."""
    formats = record["duscope"].get("formats") or {}
    return bool(formats) and set(formats) == {"default"}


def jq_flag(record: Dict[str, Any]) -> bool:
    """Whether a run passed `--jq` to duscope (as the shim saw it, or in a
    command that never reached it)."""
    return bool(record["duscope"]["jq_flag"] or record["jq_flag_in_commands"])


def _count(values) -> Dict[str, int]:
    out: Dict[str, int] = {}
    for v in values:
        out[v] = out.get(v, 0) + 1
    return dict(sorted(out.items()))


def criterion(treated: Dict[str, Any], baseline: Dict[str, Any], min_reduction: float, margin: int) -> Dict[str, Any]:
    """The row's criterion for `treated` against `baseline`: better when it
    completed at least `margin` runs more, not better when as many fewer.
    Within the margin the arms count as equal, and it is better with at
    least `min_reduction` fewer tokens (the mean of a completed run). Arms
    of unequal size (a run left for --resume) compare as rates, the margin
    taken at their mean size. Undecided when either has no runs or neither
    completed anything."""
    if not treated["runs"] or not baseline["runs"]:
        return {"verdict": "undecided", "reason": "no runs to compare"}
    if not treated["completed"] and not baseline["completed"]:
        return {"verdict": "undecided", "reason": "no run completed its task"}
    n, m = treated["runs"], baseline["runs"]
    ahead = (treated["completed"] / n - baseline["completed"] / m) * (n + m) / 2
    apart = round(abs(ahead), 1)
    runs = "%g run%s" % (apart, "" if apart == 1 else "s")
    if apart > 0 and apart >= margin:
        if ahead > 0:
            return {"verdict": "better", "reason": "higher completion, %s ahead with a margin of %d" % (runs, margin)}
        return {"verdict": "not better", "reason": "lower completion, %s behind with a margin of %d" % (runs, margin)}
    within = "completion %s apart, within the margin of %d" % (runs, margin)
    a, b = treated["tokens_mean"], baseline["tokens_mean"]
    if a is None or b is None or b == 0:
        return {"verdict": "undecided", "reason": within + "; no token counts"}
    reduction = 1 - a / b
    if reduction >= min_reduction - 1e-12:
        return {"verdict": "better", "reason": "%s; %.1f%% fewer tokens" % (within, reduction * 100)}
    if reduction >= 0:
        why = "%s; only %.1f%% fewer tokens" % (within, reduction * 100)
    else:
        why = "%s; %.1f%% more tokens" % (within, -reduction * 100)
    return {"verdict": "not better", "reason": why}


def decide(runs: List[Dict[str, Any]], config: Dict[str, Any]) -> Dict[str, Any]:
    d = config["decision"]
    reduction = float(d["min_token_reduction"])
    margin = int(d["completion_margin_runs"])
    with_skill = stats([r for r in runs if r["skill"]])
    without = stats([r for r in runs if not r["skill"]])
    skill = criterion(with_skill, without, reduction, margin)
    skill_decision = {
        "better": "recommended",
        "not better": "optional: the skill stays optional and its description is rewritten",
        "undecided": "undecided",
    }[skill["verdict"]]

    json_runs = stats([r for r in runs if r["format"] == "json"])
    plain_runs = stats([r for r in runs if r["format"] == "plain"])
    fmt = criterion(json_runs, plain_runs, reduction, margin)
    if not json_runs["runs"] or not plain_runs["runs"]:
        fmt_decision = "not measured: the cells lack json or plain"
    elif fmt["verdict"] == "better":
        fmt_decision = "revise: json beat plain (%s)" % fmt["reason"]
    elif fmt["verdict"] == "not better":
        fmt_decision = "confirmed: json did not beat plain (%s)" % fmt["reason"]
    else:
        fmt_decision = "undecided (%s)" % fmt["reason"]

    runs = [r for r in runs if not outage(r)]
    flagged = sum(1 for r in runs if jq_flag(r))
    piped = sum(1 for r in runs if r["jq_pipe"])
    threshold = d["jq_attempt_runs_to_revisit"]
    if flagged >= threshold:
        jq_decision = "revisit: agents passed --jq in %d of %d runs" % (flagged, len(runs))
    else:
        jq_decision = "confirmed: %d of %d runs passed --jq" % (flagged, len(runs))
    return {
        "model": config["model"]["id"],
        "margin_runs": margin,
        "skill": {"with": with_skill, "without": without, "criterion": skill, "decision": skill_decision},
        "format": {"json": json_runs, "plain": plain_runs, "criterion": fmt, "decision": fmt_decision},
        "jq": {"runs": len(runs), "flag_runs": flagged, "pipe_runs": piped, "threshold": threshold, "decision": jq_decision},
    }


# When both arms complete at least this share of their runs (58 of 60),
# completion cannot separate them, and the skill line says so.
CEILING = (58, 60)


def checklist_lines(decisions: Dict[str, Any]) -> List[str]:
    """The gate checklist's two decision lines, filled, and the --jq note."""

    def completion(s: Dict[str, Any]) -> str:
        if not s["runs"]:
            return "no runs"
        return "%d/%d (%.1f%%)" % (s["completed"], s["runs"], 100.0 * s["completed"] / s["runs"])

    def tokens(s: Dict[str, Any]) -> str:
        return "{:,.0f}".format(s["tokens_mean"]) if s["tokens_mean"] is not None else "n/a"

    def no_format(s: Dict[str, Any]) -> str:
        return "%d of %d runs passed no format" % (s.get("default_format_runs", 0), s["runs"])

    def at_ceiling(s: Dict[str, Any]) -> bool:
        return bool(s["runs"]) and s["completed"] * CEILING[1] >= CEILING[0] * s["runs"]

    sk, fm, jq = decisions["skill"], decisions["format"], decisions["jq"]
    model = "model `%s`" % decisions.get("model")
    skill_line = (
        "Decision gate **skill recommendation** (P3-AGT-03, %s): completion with/without %s / %s, higher with a "
        "margin of %d runs ; tokens %s / %s (mean per completed run) ; decision: %s (%s)"
        % (model, completion(sk["with"]), completion(sk["without"]), decisions.get("margin_runs"),
           tokens(sk["with"]), tokens(sk["without"]), sk["decision"], sk["criterion"]["reason"])
    )
    if at_ceiling(sk["with"]) and at_ceiling(sk["without"]):
        skill_line += "; both arms completed %d of %d or more: the eval measured tokens alone" % CEILING
    return [
        skill_line,
        "Decision gate **non-TTY default `plain`** (D27; P3-AGT-03, %s): confirmed/revised: %s; json %s, %s tokens, "
        "%s; plain %s, %s tokens, %s" % (model, fm["decision"], completion(fm["json"]), tokens(fm["json"]),
                                         no_format(fm["json"]), completion(fm["plain"]), tokens(fm["plain"]),
                                         no_format(fm["plain"])),
        "`--jq` absent (D36): %s; %d runs piped duscope to jq" % (jq["decision"], jq["pipe_runs"]),
    ]


def summarize(runs: List[Dict[str, Any]], cells: List[Dict[str, Any]], tasks: List[Dict[str, Any]], config: Dict[str, Any]) -> Dict[str, Any]:
    by_cell = {c["id"]: stats([r for r in runs if r["cell"] == c["id"]]) for c in cells}
    by_task = {}
    for t in tasks:
        row = {}
        for c in cells:
            mine = [r for r in runs if r["task"] == t["id"] and r["cell"] == c["id"] and not outage(r)]
            row[c["id"]] = "%d/%d" % (sum(1 for r in mine if r["completed"]), len(mine))
        by_task[t["id"]] = row
    decisions = decide(runs, config)
    return {"cells": by_cell, "tasks": by_task, "decisions": decisions, "checklist": checklist_lines(decisions)}


def format_summary(result: Dict[str, Any]) -> str:
    cells = [c["id"] for c in result["cells"]]
    s = result["summary"]
    lines = ["%s, %s, model %s, %d runs%s" % (
        result["date"], result["mode"], result["model"], len(result["runs"]),
        "" if result["complete"] else " (incomplete)")]
    d = result["config"]["decision"]
    lines.append(
        "criterion: higher completion by %s runs, or within that margin %.0f%% fewer tokens (the mean of a "
        "completed run)" % (d.get("completion_margin_runs", "-"), 100 * float(d["min_token_reduction"]))
    )
    lines.append("")
    head = "%-18s %10s %12s %9s %7s %10s %9s %10s %7s" % (
        "cell", "completed", "tokens", "cost", "turns", "recovered", "skill", "no format", "infra")
    lines.append(head)
    for cid in cells:
        c = s["cells"][cid]
        lines.append("%-18s %10s %12s %9s %7s %10s %9s %10s %7s" % (
            cid,
            "%d/%d" % (c["completed"], c["runs"]),
            "{:,.0f}".format(c["tokens_mean"]) if c["tokens_mean"] is not None else "-",
            "$%.2f" % c["cost_usd"] if c["cost_usd"] is not None else "-",
            c["turns_mean"] if c["turns_mean"] is not None else "-",
            "%d/%d" % (c["recovered_runs"], c["runs_with_failed_calls"]),
            c["skill_invoked_runs"],
            c.get("default_format_runs", "-"),
            c.get("infrastructure", 0),
        ))
    lines.append("")
    lines.append("%-16s " % "task" + " ".join("%-14s" % cid for cid in cells))
    for tid, row in s["tasks"].items():
        lines.append("%-16s " % tid + " ".join("%-14s" % row[cid] for cid in cells))
    lines.append("")
    for line in s["checklist"]:
        lines.append("- " + line)
    return "\n".join(lines)


# -- the result file --------------------------------------------------------------


def sha256_file(path: str) -> str:
    with open(path, "rb") as f:
        return hashlib.sha256(f.read()).hexdigest()


def git_commit() -> Optional[str]:
    try:
        out = subprocess.run(["git", "-C", REPO, "rev-parse", "HEAD"], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=10)
    except (OSError, subprocess.TimeoutExpired):
        return None
    if out.returncode != 0:
        return None
    return out.stdout.decode().strip() or None


def version_of(argv: List[str], env: Dict[str, str]) -> Optional[str]:
    try:
        out = subprocess.run(argv + ["--version"], env=env, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=60)
    except (OSError, subprocess.TimeoutExpired):
        return None
    text = out.stdout.decode("utf-8", "replace").strip()
    return text or None


def write_json(path: str, value: Any) -> None:
    tmp = path + ".tmp"
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(value, f, indent=1, sort_keys=False)
        f.write("\n")
    os.replace(tmp, path)


def utc_now() -> str:
    return datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


# -- the command line ---------------------------------------------------------------


def parse_args(argv: List[str]) -> argparse.Namespace:
    p = argparse.ArgumentParser(prog="bench/eval/run.sh", description=__doc__.split("\n\n")[0])
    sub = p.add_subparsers(dest="command")

    def common(q: argparse.ArgumentParser) -> None:
        q.add_argument("--config", default=CONFIG, help="the configuration (default: bench/eval/config.toml)")
        q.add_argument("--tasks-file", default=TASKS, help="the tasks (default: bench/eval/tasks.toml)")
        q.add_argument("--tasks", default=None, help="comma-separated task ids (default: all ten)")
        q.add_argument("--cells", default=None, help="comma-separated cell ids, such as skill-json (default: all)")
        q.add_argument("--runs", type=int, default=None, help="runs per task and cell (default: config.toml's)")

    run = sub.add_parser("run", help="run the eval")
    common(run)
    run.add_argument("--claude", default="claude", help="the Claude Code executable (default: claude on PATH)")
    run.add_argument("--duscope", default=os.path.join(REPO, "target", "release", "duscope"), help="the binary under test")
    run.add_argument("--fake", choices=("right", "wrong"), default=None, help="run the stub agent, answering right or wrong")
    run.add_argument("--jobs", type=int, default=1, help="runs at once (default 1)")
    run.add_argument("--results-dir", default=RESULTS)
    run.add_argument("--out", default=None, help="the result file (default: <results-dir>/<date>.json)")
    run.add_argument("--resume", default=None, help="a result file to continue: its runs are kept and not run again")
    run.add_argument("--transcripts", default=None, help="where each run's transcript is kept (default: bench/eval/.runs/<date>)")
    run.add_argument("--no-transcripts", action="store_true", help="keep no transcripts")
    run.add_argument("--work-dir", default=None, help="where run directories are made (default: the system's)")
    run.add_argument("--keep-work", action="store_true", help="keep each run's directory")
    run.add_argument("--scale", type=float, default=1.0, help=argparse.SUPPRESS)

    plan = sub.add_parser("plan", help="print what a run would do")
    common(plan)
    summary = sub.add_parser("summary", help="print a result file")
    summary.add_argument("file")
    tree = sub.add_parser("tree", help="build the eval tree and print each task's answer")
    tree.add_argument("dir")
    tree.add_argument("--duscope", default=os.path.join(REPO, "target", "release", "duscope"))
    tree.add_argument("--tasks-file", default=TASKS)
    args = p.parse_args(argv)
    if args.command is None:
        args = p.parse_args(["run"] + argv)
    return args


def select(tasks, cells, args) -> Tuple[List[Dict[str, Any]], List[Dict[str, Any]]]:
    if args.tasks:
        wanted = args.tasks.split(",")
        unknown = [t for t in wanted if t not in {x["id"] for x in tasks}]
        if unknown:
            raise EvalError("unknown task: %s" % ", ".join(unknown))
        tasks = [t for t in tasks if t["id"] in wanted]
    if args.cells:
        wanted = args.cells.split(",")
        unknown = [c for c in wanted if c not in {x["id"] for x in cells}]
        if unknown:
            raise EvalError("unknown cell: %s" % ", ".join(unknown))
        cells = [c for c in cells if c["id"] in wanted]
    return tasks, cells


def cmd_plan(args) -> int:
    config = load_config(args.config)
    meta, tasks = load_tasks(args.tasks_file)
    tasks, cells = select(tasks, cells_of(config), args)
    runs = args.runs or config["cells"]["runs"]
    total = runs * len(tasks) * len(cells)
    cc = config["claude_code"]
    print("model %s, Claude Code %s" % (config["model"]["id"], cc["version"]))
    print("%d tasks x %d cells x %d runs = %d runs" % (len(tasks), len(cells), runs, total))
    print("cells: " + ", ".join(c["id"] for c in cells))
    print("limits per run: %d turns, %d s, %s tokens" % (cc["max_turns"], cc["timeout_s"], "{:,}".format(cc["max_tokens"])))
    print("at most %.1f hours and %s tokens in all" % (total * cc["timeout_s"] / 3600.0, "{:,}".format(total * cc["max_tokens"])))
    print("")
    for t in tasks:
        print("%s: %s" % (t["id"], render_prompt(meta, t, "/path/to/" + TREE_NAME).replace("\n", "\n    ")))
        print("")
    return 0


def cmd_summary(args) -> int:
    with open(args.file, encoding="utf-8") as f:
        result = json.load(f)
    print(format_summary(result))
    return 0


def cmd_tree(args) -> int:
    _, tasks = load_tasks(args.tasks_file)
    layout = Layout(args.dir)
    if os.path.exists(layout.root):
        raise EvalError("%s exists" % layout.root)
    now = time.time()
    evaltree.build(layout.root, now)
    env = {"HOME": layout.home, "XDG_CACHE_HOME": layout.truth, "PATH": "/usr/bin:/bin"}
    ctx = evalchecks.Context(layout.root, now, 1.0, lambda a: run_duscope(args.duscope, a + ["--no-save"], env, layout.project))
    for t in tasks:
        if t["fixture"] != "tree":
            print("%s: needs its fixture's two scans" % t["id"])
            continue
        truth = evalchecks.TRUTHS[t["truth"]](ctx, t["params"])
        print("%s: %s" % (t["id"], json.dumps(evalchecks.portable(truth.expected, ctx.root))))
    print(layout.root)
    return 0


def cmd_run(args, base_env: Dict[str, str]) -> int:
    config = load_config(args.config)
    meta, tasks = load_tasks(args.tasks_file)
    tasks, cells = select(tasks, cells_of(config), args)
    runs = args.runs or config["cells"]["runs"]
    if args.scale != 1.0 and not args.fake:
        raise EvalError("--scale is for the fake agent only")
    binary = os.path.realpath(args.duscope)
    if not os.access(binary, os.X_OK):
        raise EvalError("no duscope binary at %s: build it (cargo build --release -p duscope) or pass --duscope" % args.duscope)

    if args.fake:
        claude = [sys.executable, "-B", "-E", "-s", FAKE_CLAUDE]
        claude_version = version_of(claude, {"PATH": "/usr/bin:/bin"})
        mode = "fake-" + args.fake
    else:
        auth = [n for n in AUTH_VARS if base_env.get(n)]
        if not auth:
            raise EvalError(
                "neither ANTHROPIC_API_KEY nor CLAUDE_CODE_OAUTH_TOKEN is set: Claude Code runs with a "
                "scratch HOME and needs one of them (`claude setup-token` prints a token for a subscription)"
            )
        found = shutil.which(args.claude, path=base_env.get("PATH", ""))
        if not found:
            raise EvalError("no Claude Code at %s" % args.claude)
        claude = [found]
        claude_version = version_of(claude, dict(base_env, DISABLE_AUTOUPDATER="1"))
        pinned = config["claude_code"]["version"]
        if not claude_version or claude_version.split()[0] != pinned:
            raise EvalError(
                "Claude Code %s is not the pinned %s (config.toml): install it with "
                "npm install --global --prefix DIR @anthropic-ai/claude-code@%s and pass --claude DIR/bin/claude"
                % (claude_version, pinned, pinned)
            )
        mode = "claude"

    date = datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%d")
    suffix = "" if mode == "claude" else "-" + mode
    out = args.out or os.path.join(args.results_dir, "%s%s.json" % (date, suffix))
    # A second start on the same day would overwrite the first's runs.
    if os.path.exists(out) and not (args.resume and os.path.realpath(args.resume) == os.path.realpath(out)):
        raise EvalError("%s exists: --resume %s continues it, --out names another file" % (out, out))
    os.makedirs(os.path.dirname(os.path.abspath(out)), exist_ok=True)

    header = {
        "schema_version": SCHEMA_VERSION,
        "date": date,
        "started_at": utc_now(),
        "finished_at": None,
        "complete": False,
        "mode": mode,
        "model": config["model"]["id"],
        "host": {
            "platform": "macos" if sys.platform == "darwin" else "linux" if sys.platform.startswith("linux") else sys.platform,
            "os": platform_module.platform(),
            "arch": platform_module.machine(),
        },
        "harness": {
            "commit": git_commit(),
            "python": platform_module.python_version(),
            "config_sha256": sha256_file(args.config),
            "tasks_sha256": sha256_file(args.tasks_file),
            "scale": args.scale,
            "jobs": args.jobs,
            "runs_per_cell": runs,
        },
        "claude_code": {
            "path": shown_path(claude[-1]),
            "version": claude_version,
            "auth": [n for n in AUTH_VARS if base_env.get(n)] if mode == "claude" else [],
        },
        "duscope": {"path": shown_path(binary), "version": version_of([binary], {"PATH": "/usr/bin:/bin"})},
        "config": config,
        "cells": cells,
        "tasks": [
            {k: t[k] for k in ("id", "title", "row", "fixture", "truth", "check", "tolerance", "params")}
            for t in tasks
        ],
        "prompts": {t["id"]: render_prompt(meta, t, "{root}", args.scale) for t in tasks},
        "runs": [],
    }
    done: Dict[Tuple[str, str, int], Dict[str, Any]] = {}
    if args.resume:
        with open(args.resume, encoding="utf-8") as f:
            old = json.load(f)
        for key in ("config_sha256", "tasks_sha256", "scale"):
            if old["harness"][key] != header["harness"][key]:
                raise EvalError("%s was run with another %s: start a new file" % (args.resume, key))
        if old["mode"] != mode or old["claude_code"]["version"] != claude_version:
            raise EvalError("%s was run with another agent: start a new file" % args.resume)
        # One file, one binary: the runs it keeps and those it adds must
        # measure the same duscope.
        if old["harness"].get("commit") != header["harness"]["commit"]:
            raise EvalError(
                "%s was run at another commit (%s; this checkout is at %s): start a new file"
                % (args.resume, old["harness"].get("commit"), header["harness"]["commit"])
            )
        if old["duscope"].get("version") != header["duscope"]["version"]:
            raise EvalError(
                "%s was run with another duscope (%s; this one is %s): start a new file"
                % (args.resume, old["duscope"].get("version"), header["duscope"]["version"])
            )
        for r in old["runs"]:
            if not outage(r):
                done[(r["task"], r["cell"], r["repeat"])] = r
        header["started_at"] = old["started_at"]
        header["date"] = old["date"]

    transcripts = None if args.no_transcripts else (args.transcripts or os.path.join(HERE, ".runs", header["date"] + suffix))
    if transcripts and not args.resume and os.path.isdir(transcripts) and os.listdir(transcripts):
        raise EvalError(
            "%s holds the transcripts of another run: --transcripts names another directory, "
            "--no-transcripts keeps none" % transcripts
        )

    extra_dirs = tool_dirs(["claude", "node", "jq"], base_env) if mode == "claude" else tool_dirs(["jq"], base_env)
    if mode == "claude":
        extra_dirs = [os.path.dirname(claude[0])] + [d for d in extra_dirs if d != os.path.dirname(claude[0])]
    settings = Settings(
        config=config,
        meta=meta,
        binary=binary,
        claude=claude,
        fake=args.fake,
        scale=args.scale,
        base_env=base_env,
        extra_dirs=extra_dirs,
        work_dir=args.work_dir,
        keep_work=args.keep_work,
        transcripts=transcripts,
        fake_acts=fake_acts(base_env.get(FAKE_ACTS_VAR, "")) if args.fake else [],
    )
    # Interleaved: every cell runs a task before the next task, and every task
    # once before any runs a second time, so a slow hour or a changed rate
    # limit falls on every cell alike.
    order = [(t, c, r) for r in range(runs) for t in tasks for c in cells if (t["id"], c["id"], r) not in done]
    records = list(done.values())
    lock = threading.Lock()

    task_index = {t["id"]: i for i, t in enumerate(tasks)}
    cell_index = {c["id"]: i for i, c in enumerate(cells)}
    wanted = {(t["id"], c["id"], r) for r in range(runs) for t in tasks for c in cells}

    def save(final: bool) -> bool:
        ordered = sorted(
            records,
            key=lambda x: (x["repeat"], task_index.get(x["task"], len(task_index)), cell_index.get(x["cell"], len(cell_index))),
        )
        have = {(x["task"], x["cell"], x["repeat"]) for x in ordered if not outage(x)}
        result = dict(header, runs=ordered, complete=final and wanted <= have)
        if final:
            result["finished_at"] = utc_now()
        result["summary"] = summarize(ordered, cells, tasks, config)
        write_json(out, result)
        return result["complete"]

    total = len(order) + len(done)
    failures: List[str] = []

    def take(future: concurrent.futures.Future) -> Optional[Dict[str, Any]]:
        t, c, r = futures[future]
        try:
            record = future.result()
        except CutShort:
            cut.append(future)
            return None
        except (EvalError, OSError, subprocess.SubprocessError, evalchecks.TruthError) as e:
            if settings.agents.stopping:
                cut.append(future)
                return None
            failures.append("%s/%s/%d: %s" % (t["id"], c["id"], r, e))
            print("eval: %s %s run %d could not run: %s" % (t["id"], c["id"], r + 1, e), file=sys.stderr)
            return None
        with lock:
            records.append(record)
            save(False)
            print(
                "eval: [%d/%d] %s %s run %d: %s%s"
                % (len(records), total, t["id"], c["id"], r + 1,
                   "completed" if record["completed"] else "not completed",
                   "" if record["completed"] else " (%s)" % (record["infrastructure"] or record["failure"])),
                file=sys.stderr,
            )
        return record

    # On Ctrl-C, after STOP_AFTER_OUTAGES outages in a row (or on any error
    # here) the queue is cancelled and the running agents are ended; a run
    # cut short is not recorded, so --resume runs it.
    cut: List[concurrent.futures.Future] = []
    taken = set()
    interrupted = False
    drained = False
    stopped: Optional[str] = None
    streak = 0
    pool = concurrent.futures.ThreadPoolExecutor(max_workers=max(1, args.jobs))
    futures = {pool.submit(run_one, settings, t, c, r): (t, c, r) for t, c, r in order}
    try:
        for future in concurrent.futures.as_completed(futures):
            taken.add(future)
            record = take(future)
            if record is None:
                continue
            streak = streak + 1 if outage(record) else 0
            if streak >= STOP_AFTER_OUTAGES:
                stopped = record["infrastructure"]
                break
        else:
            drained = True
    except KeyboardInterrupt:
        interrupted = True
    finally:
        if not drained:
            settings.agents.stop()
            pool.shutdown(wait=False, cancel_futures=True)
        pool.shutdown(wait=True)
    # A run that ended while the eval stopped is kept.
    for future in futures:
        if future not in taken and not future.cancelled():
            take(future)

    complete = save(not interrupted and not stopped)
    with open(out, encoding="utf-8") as f:
        print(format_summary(json.load(f)), file=sys.stderr)
    print(out)
    left = "%d runs cut short and not recorded, %d not started" % (len(cut), sum(1 for f in futures if f.cancelled()))
    if interrupted:
        print("eval: interrupted: %s; the same command with --resume %s runs them" % (left, out), file=sys.stderr)
        return 130
    outages = sum(1 for x in records if outage(x))
    if stopped:
        print(
            "eval: stopped after %d infrastructure failures in a row (the last: %s); %s. Once that is "
            "fixed, the same command with --resume %s runs them and the failures again"
            % (STOP_AFTER_OUTAGES, stopped, left, out),
            file=sys.stderr,
        )
    elif outages:
        print(
            "eval: %d runs failed for the infrastructure and are not counted; the same command with "
            "--resume %s runs them again" % (outages, out),
            file=sys.stderr,
        )
    return 0 if complete else 1


def main(argv: Optional[List[str]] = None, environ: Optional[Dict[str, str]] = None) -> int:
    args = parse_args(sys.argv[1:] if argv is None else argv)
    base_env = dict(os.environ if environ is None else environ)
    try:
        if args.command == "plan":
            return cmd_plan(args)
        if args.command == "summary":
            return cmd_summary(args)
        if args.command == "tree":
            return cmd_tree(args)
        return cmd_run(args, base_env)
    except (EvalError, minitoml.TomlError) as e:
        print("eval: %s" % e, file=sys.stderr)
        return 2
    except KeyboardInterrupt:
        print("eval: interrupted", file=sys.stderr)
        return 130


if __name__ == "__main__":
    sys.exit(main())
