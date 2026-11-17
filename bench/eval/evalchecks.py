"""What each task's answer should be, and whether an agent's answer is it.

A task in tasks.toml names a `truth` (how the right answer is worked out for
the tree of the run) and a `check` (how the agent's answer is compared with
it). Most truths come from the tree itself, walked with `lstat`: sizes are
allocated bytes (`st_blocks` x 512, each inode once), which is what duscope
reports by default. The truths that depend on duscope's own rules (which
directories are regenerable caches) or on its saved scans (the diff task's
totals) come from `duscope --json`, run by the harness with an index
directory of its own, so the agent's saved scans are not touched.

The agent ends its reply with `ANSWER: {json}`; `extract_answer` reads the
last such line (`Final ANSWER:` too, in any case) that a JSON object follows,
so a line of prose after it, one that starts `Answer:` included, is passed
over. Paths compare after `realpath` (so `/tmp` and `/private/tmp` agree); a
relative path is under the tree's root, and so is one that starts with `~`,
since the prompt calls the tree a copy of someone's home directory. Sizes
compare within the task's `tolerance` (a fraction of the truth), and may be
written as numbers or with a unit (`12.5 MiB`, `13 MB`); where a unit could
mean powers of 1,000 or 1,024 (`MB`, `M`), either reading counts. A size
followed by its byte count in parentheses (`63 MiB (66,246,656 bytes)`) reads
as that count.
"""

from __future__ import annotations

import json
import os
import re
from typing import Any, Callable, Dict, List, Optional, Tuple

import evaltree

DAY = evaltree.DAY
MIB = evaltree.M


class TruthError(RuntimeError):
    """The tree does not give the task a single answer (its design broke)."""


class Context:
    """What a truth needs: the tree's root, the build time, the scale, a way
    to run `duscope --json` on the tree, and what the fixture prepared."""

    def __init__(
        self,
        root: str,
        now: float,
        scale: float = 1.0,
        duscope_json: Optional[Callable[[List[str]], Dict[str, Any]]] = None,
        fixture: Optional[Dict[str, Any]] = None,
    ) -> None:
        self.root = os.path.realpath(root)
        self.now = now
        self.scale = scale
        self.duscope_json = duscope_json
        self.fixture = fixture or {}
        self._sizes: Optional[Dict[str, int]] = None
        self._caches: Optional[List[str]] = None

    def sizes(self) -> Dict[str, int]:
        if self._sizes is None:
            self._sizes = evaltree.allocated_by_path(self.root)
        return self._sizes

    def caches(self) -> List[str]:
        """The regenerable caches duscope finds under the root, outermost."""
        if self._caches is None:
            if self.duscope_json is None:
                raise TruthError("this truth needs duscope")
            env = self.duscope_json(["caches", self.root, "--min-size", "0", "--limit", "100"])
            paths = sorted(os.path.realpath(e["path"]) for e in env.get("entries", []))
            self._caches = [p for p in paths if not any(under(p, q) for q in paths if q != p)]
        return self._caches

    def files(self) -> List[Tuple[str, int, float, str]]:
        """Each regular file: (path, allocated bytes, mtime, name)."""
        out = []
        sizes = self.sizes()
        for path, st in evaltree.walk_files(self.root):
            if os.path.isfile(path) and not os.path.islink(path):
                out.append((path, sizes[path], st.st_mtime, os.path.basename(path)))
        return out

    def mib(self, value: float) -> int:
        """`value` MiB in bytes, at the tree's scale."""
        return int(value * MIB * self.scale)


class Truth:
    """`expected` is the answer (recorded in the result file); `data` is what
    else a check needs (not recorded)."""

    def __init__(self, expected: Dict[str, Any], data: Optional[Dict[str, Any]] = None) -> None:
        self.expected = expected
        self.data = data or {}


def under(path: str, parent: str) -> bool:
    return path == parent or path.startswith(parent.rstrip("/") + "/")


# -- truths ----------------------------------------------------------------


def truth_largest_child_dir(ctx: Context, params: Dict[str, Any]) -> Truth:
    sizes = ctx.sizes()
    dirs = []
    for name in os.listdir(ctx.root):
        path = os.path.join(ctx.root, name)
        if os.path.isdir(path) and not os.path.islink(path):
            dirs.append((sizes[path], path))
    dirs.sort(reverse=True)
    return Truth({"path": dirs[0][1], "bytes": dirs[0][0]}, {"runner_up": dirs[1]})


def truth_largest_files(ctx: Context, params: Dict[str, Any]) -> Truth:
    n = int(params.get("n", 3))
    files = sorted(((size, path) for path, size, _, _ in ctx.files()), reverse=True)
    top = files[:n]
    return Truth({"paths": [p for _, p in top], "bytes": [s for s, _ in top]}, {"next": files[n]})


def truth_caches_total(ctx: Context, params: Dict[str, Any]) -> Truth:
    sizes = ctx.sizes()
    caches = ctx.caches()
    return Truth({"bytes": sum(sizes[p] for p in caches), "caches": caches})


def truth_stale_large(ctx: Context, params: Dict[str, Any]) -> Truth:
    floor = ctx.mib(params["min_mib"])
    cutoff = ctx.now - params["days"] * DAY
    paths = sorted(p for p, size, mtime, _ in ctx.files() if size > floor and mtime < cutoff)
    return Truth({"paths": paths})


def truth_suffix_total(ctx: Context, params: Dict[str, Any]) -> Truth:
    suffix = params["suffix"]
    hits = sorted((p, size) for p, size, _, name in ctx.files() if name.endswith(suffix))
    return Truth({"bytes": sum(s for _, s in hits), "paths": [p for p, _ in hits]})


def truth_explain_dir(ctx: Context, params: Dict[str, Any]) -> Truth:
    sizes = ctx.sizes()
    target = os.path.join(ctx.root, params["dir"])
    children = sorted(((sizes[os.path.join(target, n)], os.path.join(target, n)) for n in os.listdir(target)), reverse=True)
    size, path = children[0]
    regenerable = any(under(path, c) for c in ctx.caches())
    return Truth({"path": path, "bytes": size, "regenerable": regenerable}, {"runner_up": children[1]})


def truth_diff_growth(ctx: Context, params: Dict[str, Any]) -> Truth:
    f = ctx.fixture
    if "old_total" not in f:
        raise TruthError("the diff task needs the two-scans fixture")
    return Truth(
        {"path": f["largest_added"], "net_growth_bytes": f["new_total"] - f["old_total"]},
        {"old_scan": f["old_scan"], "new_scan": f["new_scan"]},
    )


def truth_same_name_large(ctx: Context, params: Dict[str, Any]) -> Truth:
    floor = ctx.mib(params["min_mib"])
    groups: Dict[str, List[str]] = {}
    for path, size, _, name in ctx.files():
        if size > floor:
            groups.setdefault(name, []).append(path)
    shared = {name: sorted(paths) for name, paths in groups.items() if len(paths) > 1}
    if len(shared) != 1:
        raise TruthError("expected one shared name over the floor, found %d" % len(shared))
    name, paths = next(iter(shared.items()))
    return Truth({"name": name, "paths": paths})


def truth_cleanup(ctx: Context, params: Dict[str, Any]) -> Truth:
    sizes = ctx.sizes()
    caches = ctx.caches()
    cutoff = ctx.now - params["days"] * DAY
    eligible: Dict[str, bool] = {}
    has_file: Dict[str, bool] = {}
    for path, size, mtime, _ in ctx.files():
        ok = mtime < cutoff or any(under(path, c) for c in caches)
        eligible[path] = ok
        parent = os.path.dirname(path)
        while under(parent, ctx.root) and parent != ctx.root:
            eligible[parent] = eligible.get(parent, True) and ok
            has_file[parent] = True
            parent = os.path.dirname(parent)
    for c in caches:
        eligible[c] = True
        has_file[c] = True
    allowed = {p: sizes[p] for p, ok in eligible.items() if ok and (p in has_file or os.path.isfile(p))}
    outermost = [p for p in allowed if not any(q != p and under(p, q) for q in allowed)]
    return Truth(
        {"target_bytes": ctx.mib(params["target_mib"]), "eligible_bytes": sum(allowed[p] for p in outermost)},
        {"allowed": allowed, "outermost": sorted(outermost, key=lambda p: -allowed[p])},
    )


def truth_most_own_files(ctx: Context, params: Dict[str, Any]) -> Truth:
    counts = []
    for dirpath, dirnames, filenames in os.walk(ctx.root):
        own = len(filenames) + sum(1 for d in dirnames if os.path.islink(os.path.join(dirpath, d)))
        counts.append((own, os.path.realpath(dirpath)))
    counts.sort(reverse=True)
    return Truth({"path": counts[0][1], "files": counts[0][0]}, {"runner_up": counts[1]})


TRUTHS: Dict[str, Callable[[Context, Dict[str, Any]], Truth]] = {
    "largest_child_dir": truth_largest_child_dir,
    "largest_files": truth_largest_files,
    "caches_total": truth_caches_total,
    "stale_large": truth_stale_large,
    "suffix_total": truth_suffix_total,
    "explain_dir": truth_explain_dir,
    "diff_growth": truth_diff_growth,
    "same_name_large": truth_same_name_large,
    "cleanup": truth_cleanup,
    "most_own_files": truth_most_own_files,
}

# The truths that run duscope on the tree.
NEEDS_DUSCOPE = {"caches_total", "explain_dir", "cleanup"}


# -- reading an answer -------------------------------------------------------

_ANSWER = re.compile(r"(?im)^[\s>*_`#-]*(?:final[\s*_`]+)?answer[*_`\s]*:")


def extract_answer(text: Optional[str]) -> Tuple[Optional[Dict[str, Any]], Optional[str]]:
    """The JSON object after the last `ANSWER:` (or `Final ANSWER:`) line of
    `text` that one follows, or None and what was wrong. A line after the
    answer that starts with the label but holds no object (`Answer: the
    Downloads folder.`) is prose, and passed over; the last object that is
    not JSON is refused, not passed over."""
    if not text:
        return None, "no final reply"
    matches = list(_ANSWER.finditer(text))
    if not matches:
        return None, "no ANSWER line"
    for match in reversed(matches):
        rest = text[match.end() :]
        start = rest.find("{")
        if start >= 0:
            break
    else:
        return None, "no JSON object after ANSWER:"
    try:
        value, _ = json.JSONDecoder().raw_decode(rest[start:])
    except ValueError as e:
        return None, "the ANSWER object is not JSON (%s)" % e
    if not isinstance(value, dict):
        return None, "the ANSWER value is not an object"
    return value, None


_UNITS = {
    "": (1,),
    "b": (1,),
    "byte": (1,),
    "bytes": (1,),
    "k": (1000, 1024),
    "kb": (1000, 1024),
    "kib": (1024,),
    "m": (1000**2, 1024**2),
    "mb": (1000**2, 1024**2),
    "mib": (1024**2,),
    "g": (1000**3, 1024**3),
    "gb": (1000**3, 1024**3),
    "gib": (1024**3,),
    "t": (1000**4, 1024**4),
    "tb": (1000**4, 1024**4),
    "tib": (1024**4,),
}
_SIZE = re.compile(r"^\s*([0-9][0-9,_]*(?:\.[0-9]+)?)\s*([A-Za-z]*)\s*$")
# A size followed by its byte count in parentheses: `63 MiB (66,246,656 bytes)`.
_WITH_BYTES = re.compile(r"^(.*?)\(([^()]*)\)\s*$")
_BYTE_UNITS = ("", "b", "byte", "bytes")


def size_readings(value: Any) -> List[float]:
    """The byte counts `value` can mean: one for a number or an exact unit,
    two where the unit could be decimal or binary, none when unreadable. A
    size followed by its byte count in parentheses means that count alone."""
    if isinstance(value, bool):
        return []
    if isinstance(value, (int, float)):
        return [float(value)]
    if not isinstance(value, str):
        return []
    pair = _WITH_BYTES.match(value)
    if pair:
        count = _SIZE.match(pair.group(2))
        if not size_readings(pair.group(1)) or not count or count.group(2).lower() not in _BYTE_UNITS:
            return []
        return size_readings(pair.group(2))
    m = _SIZE.match(value)
    if not m:
        return []
    number = float(m.group(1).replace(",", "").replace("_", ""))
    factors = _UNITS.get(m.group(2).lower())
    if factors is None:
        return []
    return [number * f for f in factors]


def size_matches(value: Any, truth: int, tolerance: float) -> bool:
    return any(abs(r - truth) <= tolerance * max(truth, 1) for r in size_readings(value))


def norm_path(value: Any, root: str) -> Optional[str]:
    """`value` as an absolute real path. A relative path is under the root,
    and so is one that starts with `~`: the prompt calls the tree a copy of
    someone's home directory, so `~/Downloads` is the tree's Downloads (the
    run's own HOME lies beside the tree, and holds no answer)."""
    if not isinstance(value, str) or not value.strip():
        return None
    text = value.strip().strip("`'\"").rstrip("/") or "/"
    if text == "~" or text.startswith("~/"):
        text = root + text[1:]
    if not os.path.isabs(text):
        text = os.path.join(root, text)
    return os.path.realpath(os.path.normpath(text))


def norm_paths(value: Any, root: str) -> Optional[List[str]]:
    if not isinstance(value, list):
        return None
    out = [norm_path(v, root) for v in value]
    return None if any(p is None for p in out) else out  # type: ignore[misc]


def as_bool(value: Any) -> Optional[bool]:
    if isinstance(value, bool):
        return value
    if isinstance(value, str) and value.strip().lower() in ("yes", "true", "no", "false"):
        return value.strip().lower() in ("yes", "true")
    return None


def as_count(value: Any) -> Optional[int]:
    if isinstance(value, bool):
        return None
    if isinstance(value, int):
        return value
    if isinstance(value, float) and value.is_integer():
        return int(value)
    if isinstance(value, str) and re.fullmatch(r"\s*[0-9][0-9,]*\s*", value):
        return int(value.replace(",", ""))
    return None


# -- checks ----------------------------------------------------------------


class _Verdict:
    def __init__(self) -> None:
        self.reasons: List[str] = []

    def fail(self, reason: str) -> None:
        self.reasons.append(reason)

    def path(self, answer: Dict[str, Any], key: str, want: str, root: str) -> None:
        got = norm_path(answer.get(key), root)
        if got is None:
            self.fail("%s: missing or not a path" % key)
        elif got != os.path.realpath(want):
            self.fail("%s: %s, not %s" % (key, rel(got, root), rel(want, root)))

    def size(self, answer: Dict[str, Any], key: str, want: int, tolerance: float) -> None:
        value = answer.get(key)
        if not size_readings(value):
            self.fail("%s: missing or not a size" % key)
        elif not size_matches(value, want, tolerance):
            self.fail("%s: %s, not within %g%% of %d" % (key, value, tolerance * 100, want))

    def path_set(self, answer: Dict[str, Any], key: str, want: List[str], root: str) -> None:
        got = norm_paths(answer.get(key), root)
        if got is None:
            self.fail("%s: missing or not a list of paths" % key)
            return
        want_set = {os.path.realpath(p) for p in want}
        missing = sorted(want_set - set(got))
        extra = sorted(set(got) - want_set)
        if missing:
            self.fail("%s: missing %s" % (key, ", ".join(rel(p, root) for p in missing)))
        if extra:
            self.fail("%s: not expected %s" % (key, ", ".join(rel(p, root) for p in extra)))


def rel(path: str, root: str) -> str:
    """`path` with the root written `{root}`, as the result file keeps it."""
    root = os.path.realpath(root)
    if under(path, root):
        return "{root}" + path[len(root) :]
    return path


def check_path_bytes(answer, truth, tolerance, ctx) -> List[str]:
    v = _Verdict()
    v.path(answer, "path", truth.expected["path"], ctx.root)
    v.size(answer, "bytes", truth.expected["bytes"], tolerance)
    return v.reasons


def check_path_set(answer, truth, tolerance, ctx) -> List[str]:
    v = _Verdict()
    v.path_set(answer, "paths", truth.expected["paths"], ctx.root)
    return v.reasons


def check_bytes(answer, truth, tolerance, ctx) -> List[str]:
    v = _Verdict()
    v.size(answer, "bytes", truth.expected["bytes"], tolerance)
    return v.reasons


def check_explain(answer, truth, tolerance, ctx) -> List[str]:
    v = _Verdict()
    v.path(answer, "path", truth.expected["path"], ctx.root)
    v.size(answer, "bytes", truth.expected["bytes"], tolerance)
    got = as_bool(answer.get("regenerable"))
    if got is None:
        v.fail("regenerable: missing or not true or false")
    elif got != truth.expected["regenerable"]:
        v.fail("regenerable: %s, not %s" % (got, truth.expected["regenerable"]))
    return v.reasons


def check_diff(answer, truth, tolerance, ctx) -> List[str]:
    v = _Verdict()
    v.path(answer, "path", truth.expected["path"], ctx.root)
    v.size(answer, "net_growth_bytes", truth.expected["net_growth_bytes"], tolerance)
    return v.reasons


def check_name_paths(answer, truth, tolerance, ctx) -> List[str]:
    v = _Verdict()
    name = answer.get("name")
    if not isinstance(name, str) or name.strip() != truth.expected["name"]:
        v.fail("name: %r, not %r" % (name, truth.expected["name"]))
    v.path_set(answer, "paths", truth.expected["paths"], ctx.root)
    return v.reasons


def check_cleanup(answer, truth, tolerance, ctx) -> List[str]:
    v = _Verdict()
    paths = norm_paths(answer.get("paths"), ctx.root)
    allowed = truth.data["allowed"]
    if not paths:
        v.fail("paths: missing, empty or not a list of paths")
        return v.reasons
    for p in paths:
        if p == ctx.root or not under(p, ctx.root):
            v.fail("paths: %s is not inside the tree" % rel(p, ctx.root))
        elif p not in allowed:
            v.fail("paths: %s is neither a regenerable cache nor unmodified for a year" % rel(p, ctx.root))
    if v.reasons:
        return v.reasons
    chosen = {p for p in paths if not any(q != p and under(p, q) for q in paths)}
    frees = sum(allowed[p] for p in chosen)
    if frees < truth.expected["target_bytes"]:
        v.fail("the plan frees %d bytes, short of %d" % (frees, truth.expected["target_bytes"]))
    v.size(answer, "total_bytes", frees, tolerance)
    return v.reasons


def check_path_count(answer, truth, tolerance, ctx) -> List[str]:
    v = _Verdict()
    v.path(answer, "path", truth.expected["path"], ctx.root)
    got = as_count(answer.get("files"))
    if got is None:
        v.fail("files: missing or not a count")
    elif got != truth.expected["files"]:
        v.fail("files: %d, not %d" % (got, truth.expected["files"]))
    return v.reasons


CHECKS: Dict[str, Callable[..., List[str]]] = {
    "path_bytes": check_path_bytes,
    "path_set": check_path_set,
    "bytes": check_bytes,
    "explain": check_explain,
    "diff": check_diff,
    "name_paths": check_name_paths,
    "cleanup": check_cleanup,
    "path_count": check_path_count,
}


def check(name: str, text: Optional[str], truth: Truth, tolerance: float, ctx: Context) -> Dict[str, Any]:
    """The verdict on the reply `text`: passed, the reasons it did not, and
    the answer it gave (paths written with `{root}`)."""
    answer, problem = extract_answer(text)
    if answer is None:
        return {"passed": False, "reasons": [problem], "answer": None}
    reasons = CHECKS[name](answer, truth, tolerance, ctx)
    return {"passed": not reasons, "reasons": reasons, "answer": portable(answer, ctx.root)}


def portable(value: Any, root: str, work: Optional[str] = None) -> Any:
    """`value` with the root's path written `{root}` wherever it appears, and
    the run's own directory (its scratch HOME, the skill in it, the disk
    image's folder) `{work}`, so a result file reads the same whichever
    directory a run was in and names no path of the account that ran it."""
    if isinstance(value, str):
        names = {p: "{root}" for p in (os.path.realpath(root), os.path.normpath(root))}
        if work:
            names.update({p: "{work}" for p in (os.path.realpath(work), os.path.normpath(work))})
        for prefix in sorted(names, key=len, reverse=True):
            value = value.replace(prefix, names[prefix])
        return value
    if isinstance(value, list):
        return [portable(v, root, work) for v in value]
    if isinstance(value, dict):
        return {k: portable(v, root, work) for k, v in value.items()}
    return value


# -- answers for the fake agent ----------------------------------------------


def right_answer(name: str, truth: Truth) -> Dict[str, Any]:
    """The answer a fake agent gives when it answers from the truth."""
    e = truth.expected
    if name == "path_set":
        return {"paths": e["paths"]}
    if name == "bytes":
        return {"bytes": e["bytes"]}
    if name == "cleanup":
        paths, frees = [], 0
        for p in truth.data["outermost"]:
            if frees >= e["target_bytes"]:
                break
            paths.append(p)
            frees += truth.data["allowed"][p]
        return {"paths": paths, "total_bytes": frees}
    if name == "name_paths":
        return {"name": e["name"], "paths": e["paths"]}
    return {k: v for k, v in e.items()}


def wrong_answer(name: str, truth: Truth, root: str) -> Dict[str, Any]:
    """An answer a fake agent gives that each check must refuse."""
    answer = right_answer(name, truth)
    elsewhere = os.path.join(root, "nowhere")
    for key, value in list(answer.items()):
        if key in ("path",):
            answer[key] = elsewhere
        elif key == "paths":
            answer[key] = [elsewhere]
        elif key in ("bytes", "net_growth_bytes", "total_bytes"):
            answer[key] = value * 3 + 1
        elif key == "files":
            answer[key] = value + 1
        elif key == "regenerable":
            answer[key] = not value
        elif key == "name":
            answer[key] = "nowhere.bin"
    return answer
