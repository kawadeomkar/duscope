"""A stand-in for `claude -p` that calls no model: `agent_eval.py run --fake`.

It takes Claude Code's print-mode command line, ignores the prompt, and
writes a stream-json transcript of the shape `claude -p --output-format
stream-json --verbose` writes: the init message, tool calls whose commands it
really runs (so the duscope shim logs them), and the result message with a
usage block. Its reply ends with the answer the harness left for it in
DUSCOPE_TEST_EVAL_FAKE_ANSWER: the truth (`right`) or one every check refuses
(`wrong`). A wrong run first passes duscope a `--jq` it does not have, then
runs a command that works, so the harness sees a failed call and a recovery.
When the duscope skill is in HOME, it loads it with the Skill tool first.

The answer file's `acts` (from the tests' DUSCOPE_TEST_EVAL_FAKE, which the
harness reads) make it misbehave as a real run can: `hang` starts a command
and waits on it, as an agent stuck in a tool call does, and writes its own
process id and the command's to `fake.pids` beside the answer file;
`api-error` ends with the error result a rate limit gives and status 1;
`crash` exits with status 3 and no result; `model=NAME` answers as another
model; `no-skills` lists no skill and loads none; `bare` leaves out of the
result every field but the reply, and the cache counts out of each
message's usage; `notices` also streams events Claude Code sends besides
messages, whose `message` is text, and a content block that is text alone.

Its token counts are made up and deterministic: a fixed amount a turn plus a
quarter of the characters the tools returned.
"""

from __future__ import annotations

import json
import os
import shlex
import subprocess
import sys
import time
from typing import Any, Dict, List

VERSION = "0.0.0 (fake Claude Code, bench/eval/fake_claude.py)"


def emit(message: Dict[str, Any]) -> None:
    sys.stdout.write(json.dumps(message) + "\n")
    sys.stdout.flush()


def option(argv: List[str], name: str, default: str = "") -> str:
    if name in argv and argv.index(name) + 1 < len(argv):
        return argv[argv.index(name) + 1]
    return default


def main(argv: List[str]) -> int:
    if "--version" in argv:
        print(VERSION)
        return 0
    if "-p" not in argv or option(argv, "--output-format") != "stream-json" or "--verbose" not in argv:
        print("fake claude: expects -p PROMPT --output-format stream-json --verbose", file=sys.stderr)
        return 2
    path = os.environ.get("DUSCOPE_TEST_EVAL_FAKE_ANSWER")
    if not path:
        print("fake claude: DUSCOPE_TEST_EVAL_FAKE_ANSWER is not set", file=sys.stderr)
        return 2
    with open(path, encoding="utf-8") as f:
        fake = json.load(f)
    acts = fake.get("acts") or []
    model = option(argv, "--model", "fake-model")
    for act in acts:
        if act.startswith("model="):
            model = act.split("=", 1)[1]
    bare = "bare" in acts
    root = fake["root"]
    skill = os.path.join(os.environ.get("HOME", ""), ".claude", "skills", "duscope", "SKILL.md")
    has_skill = os.path.isfile(skill) and "no-skills" not in acts
    session = "fake-session"
    emit({
        "type": "system", "subtype": "init", "session_id": session, "model": model,
        "cwd": os.getcwd(), "tools": ["Bash", "Glob", "Grep", "Read", "Skill"],
        "skills": ["duscope"] if has_skill else [], "claude_code_version": VERSION.split()[0],
        "permissionMode": "default",
    })
    if "notices" in acts:
        emit({"type": "system", "subtype": "notice", "session_id": session, "message": "Usage is close to the limit"})
        emit({"type": "assistant", "session_id": session, "message": "a notice, not a message"})
        emit({"type": "user", "session_id": session, "message": {"role": "user", "content": ["a block as text"]}})
    if "hang" in acts:
        child = subprocess.Popen(["/bin/sleep", "3600"], stdin=subprocess.DEVNULL)
        pids = os.path.join(os.path.dirname(path), "fake.pids")
        with open(pids + ".tmp", "w", encoding="utf-8") as f:
            f.write("%d %d\n" % (os.getpid(), child.pid))
        os.replace(pids + ".tmp", pids)
        time.sleep(3600)
        return 1
    if "crash" in acts:
        print("fake claude: crashed", file=sys.stderr)
        return 3
    if "api-error" in acts:
        emit({
            "type": "result", "subtype": "success", "is_error": True, "session_id": session,
            "duration_ms": 10, "duration_api_ms": 8, "num_turns": 1,
            "result": 'API Error: 429 {"type":"error","error":{"type":"rate_limit_error","message":"slow down"}}',
            "total_cost_usd": 0.0, "usage": totals(0, 0), "modelUsage": {}, "permission_denials": [],
        })
        return 1

    steps: List[Dict[str, Any]] = []
    if has_skill:
        steps.append({"name": "Skill", "input": {"skill": "duscope"}})
    if fake["mode"] == "wrong":
        steps.append({"name": "Bash", "input": {"command": "duscope top %s --jq .entries" % shlex.quote(root)}})
    steps.append({"name": "Bash", "input": {"command": "duscope top %s --depth 1" % shlex.quote(root)}})

    returned = 0
    turn = 0
    for step in steps:
        turn += 1
        call_id = "toolu_fake_%d" % turn
        emit({
            "type": "assistant", "session_id": session,
            "message": {"id": "msg_fake_%d" % turn, "role": "assistant", "model": model,
                        "content": [{"type": "tool_use", "id": call_id, "name": step["name"], "input": step["input"]}],
                        "usage": usage(turn, 0, bare)},
        })
        if step["name"] == "Skill":
            with open(skill, encoding="utf-8") as f:
                output, failed = f.read(4000), False
        else:
            proc = subprocess.run(
                ["/bin/sh", "-c", step["input"]["command"]],
                stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=120,
            )
            output = proc.stdout.decode("utf-8", "replace")
            failed = proc.returncode != 0
            if failed:
                output = "Exit code %d\n%s" % (proc.returncode, output)
        returned += len(output)
        emit({
            "type": "user", "session_id": session,
            "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": call_id, "content": output[:30000], "is_error": failed}]},
        })

    turn += 1
    reply = "Here is what duscope shows.\n\nANSWER: %s" % json.dumps(fake["answer"])
    emit({
        "type": "assistant", "session_id": session,
        "message": {"id": "msg_fake_%d" % turn, "role": "assistant", "model": model,
                    "content": [{"type": "text", "text": reply}], "usage": usage(turn, returned, bare)},
    })
    if bare:
        emit({"type": "result", "subtype": "success", "is_error": False, "session_id": session, "result": reply})
        return 0
    total = totals(turn, returned)
    emit({
        "type": "result", "subtype": "success", "is_error": False, "session_id": session,
        "duration_ms": 10 * turn, "duration_api_ms": 8 * turn, "num_turns": turn, "result": reply,
        "total_cost_usd": 0.0,
        "usage": total,
        "modelUsage": {model: {
            "inputTokens": total["input_tokens"], "outputTokens": total["output_tokens"],
            "cacheReadInputTokens": total["cache_read_input_tokens"],
            "cacheCreationInputTokens": total["cache_creation_input_tokens"], "costUSD": 0.0}},
        "permission_denials": [],
    })
    return 0


def usage(turn: int, returned: int, bare: bool = False) -> Dict[str, int]:
    if bare:
        return {"input_tokens": 10, "output_tokens": 150}
    return {"input_tokens": 10, "cache_creation_input_tokens": 1000 + returned // 4,
            "cache_read_input_tokens": 20000, "output_tokens": 150}


def totals(turns: int, returned: int) -> Dict[str, int]:
    return {"input_tokens": 10 * turns, "cache_creation_input_tokens": 1000 * turns + returned // 4,
            "cache_read_input_tokens": 20000 * turns, "output_tokens": 150 * turns}


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
