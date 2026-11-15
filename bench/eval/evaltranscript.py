"""What a run did: Claude Code's stream-json transcript and the duscope calls.

`claude -p ... --output-format stream-json --verbose` writes one JSON object
per line: a `system` `init` message (the model, the tools, the skills), each
`assistant` message (text and `tool_use` blocks, with the message's usage),
each `user` message carrying `tool_result` blocks (`is_error` set when the
call failed: a non-zero exit, a denied permission), and a last `result`
message with the reply (`result`), `num_turns`, `usage`, `modelUsage` (per
model: input, output, cache read and cache creation tokens, cost),
`total_cost_usd` and `permission_denials`. Fields this module does not find
are recorded as null, never guessed or counted as 0.

`infrastructure` tells a run that measured Claude Code or the service (a
rate limit, an expired token, a crash, another model, a skill not listed)
from one that measured the agent.

The duscope the agent runs is a shim (agent_eval.SHIM) that logs each call as
NUL-separated fields: the exit status, the argument count, the arguments.
"""

from __future__ import annotations

import json
import re
from typing import Any, Dict, List, Optional

# The verbs and utilities of `duscope --help`, and which tier each is
# (PROPOSAL, Query engine: purpose-built verbs, then `query --sql`).
VERBS = {
    "scan", "top", "tree", "by-type", "stale", "caches", "summary", "diff", "explain", "clean",
    "query", "view", "schema", "scans", "rescan", "export", "import", "render", "mcp", "agent",
    "man", "completions", "help",
}

# The status the shim answers a refused `clean --yes` or `--permanent` with.
REFUSED_STATUS = 97
# The model Claude Code names on the messages it makes itself (an API
# error's text), which no model wrote.
SYNTHETIC_MODEL = "<synthetic>"

_JQ_PIPE = re.compile(r"\|\s*jq\b")
_TOKEN_FIELDS = (
    ("input_tokens", "inputTokens"),
    ("cache_creation_input_tokens", "cacheCreationInputTokens"),
    ("cache_read_input_tokens", "cacheReadInputTokens"),
    ("output_tokens", "outputTokens"),
)


def parse_stream(text: str) -> Dict[str, Any]:
    """The transcript's messages, in order, and the lines that were not JSON."""
    messages, bad = [], 0
    for line in text.splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            value = json.loads(line)
        except ValueError:
            bad += 1
            continue
        if isinstance(value, dict):
            messages.append(value)
    return {"messages": messages, "unparsed_lines": bad}


def summarize_stream(parsed: Dict[str, Any], model: str) -> Dict[str, Any]:
    """Turns, tool calls, failures, recovery, tokens and the final reply."""
    messages = parsed["messages"]
    init = next((m for m in messages if m.get("type") == "system" and m.get("subtype") == "init"), {})
    result = next((m for m in reversed(messages) if m.get("type") == "result"), None)
    calls: List[Dict[str, Any]] = []
    by_id: Dict[str, Dict[str, Any]] = {}
    assistant_usage: Dict[str, Dict[str, Any]] = {}
    texts: List[str] = []
    models = {init["model"]} if isinstance(init.get("model"), str) and init["model"] else set()
    for m in messages:
        body = m.get("message") or {}
        content = body.get("content")
        if m.get("type") == "assistant":
            synthetic = body.get("model") == SYNTHETIC_MODEL
            if isinstance(body.get("model"), str) and body["model"] and not synthetic:
                models.add(body["model"])
            if body.get("id") and isinstance(body.get("usage"), dict) and not synthetic:
                assistant_usage[body["id"]] = body["usage"]
            for block in content if isinstance(content, list) else []:
                if block.get("type") == "tool_use":
                    call = {"id": block.get("id"), "name": block.get("name"), "input": block.get("input") or {}, "is_error": None}
                    calls.append(call)
                    if call["id"]:
                        by_id[call["id"]] = call
                elif block.get("type") == "text":
                    texts.append(block.get("text") or "")
        elif m.get("type") == "user":
            for block in content if isinstance(content, list) else []:
                if block.get("type") == "tool_result" and block.get("tool_use_id") in by_id:
                    by_id[block["tool_use_id"]]["is_error"] = bool(block.get("is_error"))
    failed = [i for i, c in enumerate(calls) if c["is_error"]]
    recovered: Optional[bool] = None
    if failed:
        recovered = any(c["is_error"] is False for c in calls[failed[0] + 1 :])
    commands = [c["input"].get("command", "") for c in calls if c["name"] == "Bash"]
    reply = result.get("result") if result else None
    if reply is None and texts:
        reply = texts[-1]
    return {
        "model_seen": init.get("model"),
        "models_seen": sorted(models),
        "claude_code_version": init.get("claude_code_version"),
        "skills_listed": init.get("skills"),
        "result_subtype": result.get("subtype") if result else None,
        "result_is_error": result.get("is_error") if result else None,
        "reply": reply,
        "turns": result.get("num_turns") if result and "num_turns" in result else (len(assistant_usage) or None),
        "duration_ms": result.get("duration_ms") if result else None,
        "duration_api_ms": result.get("duration_api_ms") if result else None,
        "cost_usd": result.get("total_cost_usd") if result else None,
        "tokens": tokens(result, assistant_usage, model),
        "tool_calls": len(calls),
        "tool_names": sorted({c["name"] for c in calls if c["name"]}),
        "failed_tool_calls": len(failed),
        "recovered": recovered,
        "permission_denials": len(result["permission_denials"])
        if result and isinstance(result.get("permission_denials"), list)
        else None,
        "skill_invoked": any(skill_call(c) for c in calls),
        "bash_commands": commands,
        "jq_flag_in_commands": any("--jq" in c for c in commands if "duscope" in c),
        "jq_pipe": any("duscope" in c and _JQ_PIPE.search(c) for c in commands),
    }


def skill_call(call: Dict[str, Any]) -> bool:
    """Whether a tool call loaded the duscope skill: the Skill tool, or a
    read of its SKILL.md."""
    inp = call.get("input") or {}
    if call.get("name") == "Skill":
        return "duscope" in json.dumps(inp)
    if call.get("name") == "Read":
        return str(inp.get("file_path", "")).endswith("skills/duscope/SKILL.md")
    return False


def tokens(result: Optional[Dict[str, Any]], assistant_usage: Dict[str, Dict[str, Any]], model: str) -> Dict[str, Any]:
    """The run's tokens. `total` counts every token the pinned model read or
    wrote (input, cache creation, cache reads, output) over the run; it comes
    from `modelUsage`'s entry for that model, else the result's `usage`, else
    the sum of each assistant message's usage. `all_models` adds any other
    model Claude Code called (`modelUsage`). A count the source lacks is
    null, and so is a total that needs it."""
    out: Dict[str, Any] = {"source": None, "total": None, "all_models": None}
    for field, _ in _TOKEN_FIELDS:
        out[field] = None
    model_usage = (result or {}).get("modelUsage") or {}
    mine = [v for k, v in model_usage.items() if isinstance(v, dict) and k.startswith(model)]
    if mine:
        out["source"] = "modelUsage"
        for field, camel in _TOKEN_FIELDS:
            out[field] = _sum(mine, camel)
    elif isinstance((result or {}).get("usage"), dict):
        out["source"] = "usage"
        for field, _ in _TOKEN_FIELDS:
            out[field] = _sum([result["usage"]], field)  # type: ignore[index]
    elif assistant_usage:
        out["source"] = "assistant_messages"
        for field, _ in _TOKEN_FIELDS:
            out[field] = _sum(list(assistant_usage.values()), field)
    counts = [out[field] for field, _ in _TOKEN_FIELDS]
    if out["source"] and None not in counts:
        out["total"] = sum(counts)
    if model_usage:
        entries = [v for v in model_usage.values() if isinstance(v, dict)]
        each = [_sum(entries, camel) for _, camel in _TOKEN_FIELDS]
        out["all_models"] = None if None in each else sum(each)
    return out


def _sum(entries: List[Dict[str, Any]], key: str) -> Optional[int]:
    """`key` summed over `entries`, or None when one of them lacks it."""
    values = [e.get(key) for e in entries]
    if not values or any(isinstance(v, bool) or not isinstance(v, int) for v in values):
        return None
    return sum(values)


def lists_skill(skills: Any, name: str = "duscope") -> bool:
    """Whether the init message's `skills` (names) holds `name`."""
    return isinstance(skills, list) and any(s == name or (isinstance(s, dict) and s.get("name") == name) for s in skills)


def infrastructure(stream: Dict[str, Any], exit_status: Optional[int], model: str, skill: bool) -> Optional[str]:
    """Why a run that ended before its time limit measured the infrastructure
    rather than the agent, or None. The stream (`summarize_stream`) ended
    without a result, or with an error result (a rate limit, an expired
    token, a failed request: `is_error`), or Claude Code exited with another
    status than 0; a model other than the pinned one answered, or none is
    named; or a run of a cell with the skill lists no duscope skill. A run
    that used every turn is the agent's own failure, though Claude Code
    marks its result an error."""
    subtype = stream["result_subtype"]
    if subtype == "error_max_turns":
        return None
    if subtype is None:
        return "no result: Claude Code exited with status %s" % exit_status
    if stream["result_is_error"]:
        text = stream["reply"] if isinstance(stream["reply"], str) else ""
        return "Claude Code ended with an error (%s): %s" % (subtype, text.strip()[:300])
    if exit_status != 0:
        return "Claude Code exited with status %s" % exit_status
    if not stream["models_seen"]:
        return "the stream names no model"
    other = [m for m in stream["models_seen"] if not m.startswith(model)]
    if other:
        return "the stream names %s, not the pinned %s" % (", ".join(other), model)
    if skill and not lists_skill(stream["skills_listed"]):
        return "the run lists no duscope skill"
    return None


def parse_shim_log(data: bytes) -> List[Dict[str, Any]]:
    """The duscope calls the shim logged: each one's status and arguments."""
    fields = data.split(b"\0")
    calls, i = [], 0
    while i + 1 < len(fields):
        try:
            status = int(fields[i])
            count = int(fields[i + 1])
        except ValueError:
            break
        args = [f.decode("utf-8", "replace") for f in fields[i + 2 : i + 2 + count]]
        if len(args) < count:
            break
        calls.append({"status": status, "args": args})
        i += 2 + count
    return calls


def verb_of(args: List[str]) -> str:
    """The verb a duscope command line ran: `scan` for a bare path, `--help`
    or `--version` for those, else the first word."""
    for arg in args:
        if arg in ("-h", "--help"):
            return "--help"
        if arg in ("-V", "--version"):
            return "--version"
        if arg.startswith("-"):
            continue
        return arg if arg in VERBS else "scan"
    return "scan"


def output_format(args: List[str]) -> Optional[str]:
    """The format a command line asked for, or None for the default."""
    for i, arg in enumerate(args):
        if arg == "--json":
            return "json"
        if arg == "--plain":
            return "plain"
        if arg == "--format" and i + 1 < len(args):
            return args[i + 1]
        if arg.startswith("--format="):
            return arg.split("=", 1)[1]
    return None


def summarize_calls(calls: List[Dict[str, Any]]) -> Dict[str, Any]:
    """The duscope calls of a run: counts, verbs, tier, formats, --jq, and
    refused deletions."""
    verbs: Dict[str, int] = {}
    formats: Dict[str, int] = {}
    for call in calls:
        verb = verb_of(call["args"])
        verbs[verb] = verbs.get(verb, 0) + 1
        fmt = output_format(call["args"]) or "default"
        formats[fmt] = formats.get(fmt, 0) + 1
    data_verbs = set(verbs) - {"query", "agent", "schema", "help", "--help", "--version", "man", "completions"}
    if "query" in verbs and data_verbs:
        tier = "verbs+sql"
    elif "query" in verbs:
        tier = "sql"
    elif data_verbs:
        tier = "verbs"
    else:
        tier = "none"
    failed = [c for c in calls if c["status"] != 0]
    recovered: Optional[bool] = None
    if failed:
        first = calls.index(failed[0])
        recovered = any(c["status"] == 0 for c in calls[first + 1 :])
    return {
        "calls": len(calls),
        "failed": len(failed),
        "recovered": recovered,
        "verbs": dict(sorted(verbs.items())),
        "tier": tier,
        "formats": dict(sorted(formats.items())),
        "jq_flag": sum(1 for c in calls if any(a == "--jq" or a.startswith("--jq=") for a in c["args"])),
        "briefing_read": any(verb_of(c["args"]) == "agent" and "--skill" not in c["args"] for c in calls),
        "deletion_refused": sum(1 for c in calls if c["status"] == REFUSED_STATUS),
    }
