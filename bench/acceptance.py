"""The 1.0 acceptance run (P5-BENCH-01) and its judgement.

    bench/run.sh acceptance --duscope PATH [--platform linux|macos] [options]
    bench/run.sh judge [FILE...] [--dir bench/results/1.0] [--summary FILE]

`acceptance` runs, on this machine, what the 1.0 targets are judged on, and
writes one file, `<out-dir>/<platform>.json`:

- `run`: the harness (`bench/run.sh run`) with duscope as the subject, on
  T1-T3, warm and cold, against every pinned competitor (goal 1);
- `index`: the index measurements on T2 (`bench/run.sh index`), whose warm
  write per entry goal 2 holds;
- `rescan`: the incremental rescan's cost against a full one
  (bench/rescanbench.py), which macOS can measure and Linux cannot.

`judge` reads the result files (by default `linux.json` and `macos.json` in
`bench/results/1.0/`), prints each goal-1 and goal-2 target with the number
and `met` or `MISSED`, and with `--summary` writes the summary the README links
to, generated from the same files. It exits 0 when every target is met, 1 when
one is missed or not measured, and 2 when a file cannot be read.

Standard library only; runs on Python 3.9 and later.
"""

from __future__ import annotations

import contextlib
import json
import os
import platform as platform_module
import shlex
import shutil
import sys
import tempfile
import time
from dataclasses import dataclass
from typing import Dict, List, Optional, Sequence, Tuple

import indexbench
import measure
import rescanbench
import tools
import trees

BENCH = os.path.dirname(os.path.abspath(__file__))
OUT_DIR = os.path.join(BENCH, "results", "1.0")
PLATFORMS = ("linux", "macos")
TREES = ("T1", "T2", "T3")
# The test that holds goal 2's other half: the write starts after the output.
WRITE_AFTER_OUTPUT_TEST = "p2_idx_02_the_write_starts_after_the_output"


@dataclass(frozen=True)
class Target:
    """One goal-1 or goal-2 target: what it holds, on which platform and in
    which mode, the limit, and where the plan states it."""

    goal: int
    platform: str
    mode: str
    # The competitors duscope's time is held to; for more than one, the
    # fastest of them (its ratio is the largest). Empty for the index write.
    against: Tuple[str, ...]
    limit: float
    what: str
    source: str
    trees: Tuple[str, ...] = TREES


_LINUX = "plan/PROPOSAL.md goal 1 and 1.0 acceptance target (1); Gate 1 → 2"
_MACOS = "plan/PROPOSAL.md goal 1 and 1.0 acceptance target (2); Gate 1 → 2, deferred to 1.0 by DEV-52"

# Goal 1: scan speed on the three seeded trees against the pinned competitors,
# p50 at duscope's default threads, the ratio the harness pairs (each
# competitor's `subject_ratio_p50`). Goal 2: the index write per entry, warm,
# on Linux, read from T2 as Gate 2 -> 3 reads it.
TARGETS: Tuple[Target, ...] = (
    Target(1, "linux", "warm", ("diskus",), 1.2, "Linux warm p50 ≤ 1.2× diskus", _LINUX),
    Target(1, "linux", "warm", ("pdu", "fdu"), 1.15, "Linux warm p50 ≤ 1.15× the faster of pdu and fdu", _LINUX),
    Target(1, "linux", "cold", ("diskus",), 1.15, "Linux cold p50 ≤ 1.15× diskus", _LINUX),
    Target(1, "macos", "warm", ("diskus",), 1.0, "macOS warm p50 ≤ 1.0× diskus", _MACOS),
    Target(1, "macos", "warm", ("dua",), 1.2, "macOS warm p50 ≤ 1.2× dua", _MACOS),
    Target(1, "macos", "warm", ("dumac",), 1.3, "macOS warm p50 ≤ 1.3× dumac", _MACOS),
    Target(1, "macos", "cold", ("dua",), 1.2, "macOS cold p50 ≤ 1.2× dua", _MACOS),
    Target(
        2, "linux", "warm", (), 2.75, "Linux warm index write ≤ 2.75 µs per entry",
        "plan/PROPOSAL.md goal 2 and 1.0 acceptance target (3); DEV-107; Gate 2 → 3", ("T2",),
    ),
)


def ascii(text: str) -> str:
    """`text` for a terminal that may not take UTF-8."""
    for wide, narrow in (("≤", "<="), ("×", "x"), ("µ", "u"), ("→", "->")):
        text = text.replace(wide, narrow)
    return text


@dataclass(frozen=True)
class Verdict:
    target: Target
    tree: str
    value: Optional[float]
    status: str  # met | MISSED | not measured
    note: str = ""


# --- Reading the files --------------------------------------------------------------


class InputError(Exception):
    """A result file that cannot be judged."""


def load(path: str) -> dict:
    try:
        with open(path, encoding="utf-8") as f:
            doc = json.load(f)
    except (OSError, ValueError) as e:
        raise InputError(f"{path}: cannot read an acceptance file: {e}") from e
    if not isinstance(doc, dict) or doc.get("kind") != "acceptance" or doc.get("platform") not in PLATFORMS:
        raise InputError(f"{path}: not an acceptance file (`bench/run.sh acceptance` writes them)")
    return doc


def _ratio(run: dict, tree: str, mode: str, tool: str) -> Optional[float]:
    for r in run.get("results") or []:
        if (r.get("tree"), r.get("mode"), r.get("tool"), r.get("paired_with")) == (tree, mode, tool, run.get("subject")):
            value = r.get("subject_ratio_p50")
            return float(value) if isinstance(value, (int, float)) else None
    return None


def _why_not(run: dict, tool: str, tree: str, mode: str) -> str:
    """Why the run has no ratio of `tool` on `tree` in `mode`."""
    for entry in run.get("tools") or []:
        if entry.get("name") != tool:
            continue
        if entry.get("status") != "measured":
            return f"{tool} {entry.get('status')}: {entry.get('reason')}"
        return f"{tool} was not paired with duscope on {tree} {mode}"
    return f"{tool} was not among the run's competitors"


def judge_one(target: Target, tree: str, doc: Optional[dict]) -> Verdict:
    if doc is None:
        return Verdict(target, tree, None, "not measured", f"no {target.platform} result file")
    if not target.against:
        index = doc.get("index") or {}
        gate = next((g for g in index.get("gates") or [] if g.get("name") == "write_us_per_entry_warm_p50"), None)
        value = gate.get("value") if gate and index.get("gates_tree") == tree else None
        if not isinstance(value, (int, float)):
            return Verdict(target, tree, None, "not measured", "the index part did not measure T2's warm write")
        return Verdict(target, tree, float(value), "met" if value <= target.limit else "MISSED")
    run = doc.get("run")
    if not run:
        return Verdict(target, tree, None, "not measured", "the harness part did not run")
    if run.get("subject") != "duscope":
        return Verdict(target, tree, None, "not measured", f"the subject is {run.get('subject')}, not duscope")
    found = {c: _ratio(run, tree, target.mode, c) for c in target.against}
    present = {c: v for c, v in found.items() if v is not None}
    missing = [_why_not(run, c, tree, target.mode) for c, v in found.items() if v is None]
    if not present:
        return Verdict(target, tree, None, "not measured", "; ".join(missing))
    against, value = max(present.items(), key=lambda kv: kv[1])
    notes = [f"against {against}, the faster"] if len(target.against) > 1 else []
    notes += missing
    return Verdict(target, tree, value, "met" if value <= target.limit else "MISSED", "; ".join(notes))


def verdicts(docs: Dict[str, Optional[dict]]) -> List[Verdict]:
    """Every target on every tree it names, from each platform's file (None
    when there is none)."""
    return [judge_one(t, tree, docs.get(t.platform)) for t in TARGETS for tree in t.trees]


def conditions(doc: dict) -> List[str]:
    """What the file says of the conditions the targets name, where they differ:
    a smoke run, Linux off ext4, macOS off APFS or off Apple silicon."""
    notes = []
    run = doc.get("run") or {}
    scales = sorted({float(t.get("scale", 1)) for t in run.get("trees") or []})
    if any(s != 1 for s in scales):
        notes.append(f"a smoke run at scale {', '.join(f'{s:g}' for s in scales)}: not the acceptance run")
    want = "ext4" if doc["platform"] == "linux" else "apfs"
    for t in run.get("trees") or []:
        for key in ("filesystem", "cold_filesystem"):
            fs = t.get(key)
            if fs is not None and fs != want:
                notes.append(f"{t.get('name')}'s {'cold ' if key == 'cold_filesystem' else ''}copy is on {fs}, where the target names {want}")
    if doc["platform"] == "macos" and (doc.get("host") or {}).get("arch") != "arm64":
        notes.append(f"measured on {(doc.get('host') or {}).get('arch')}, where the target names Apple silicon")
    return notes


def header(name: str, doc: dict) -> str:
    host = doc.get("host") or {}
    commit = ((doc.get("harness") or {}).get("commit") or "unknown")[:12]
    return (
        f"{doc['platform']}: {name}, {doc.get('date')}, {host.get('cpu') or host.get('arch')}, "
        f"{host.get('logical_cpus')} CPUs, {host.get('os')}, {(doc.get('duscope') or {}).get('version')} at {commit}"
    )


def _shown(value: Optional[float]) -> str:
    return "-" if value is None else f"{value:.3f}"


def report(
    docs: Dict[str, Optional[dict]], names: Dict[str, str], platforms: Sequence[str] = PLATFORMS
) -> Tuple[List[str], bool]:
    """The judge's lines for the targets of `platforms`, and whether each of
    them is met."""
    lines = []
    for plat in platforms:
        doc = docs.get(plat)
        if doc is None:
            lines.append(f"{plat}: no result file")
            continue
        lines.append(header(names[plat], doc))
        lines += [f"  note: {n}" for n in conditions(doc)]
    found = [v for v in verdicts(docs) if v.target.platform in platforms]
    current = None
    for v in found:
        if v.target is not current:
            current = v.target
            lines.append(f"goal {v.target.goal}: {v.target.what} ({v.target.source})")
        lines.append(f"  {v.tree}: {_shown(v.value)} {v.status}" + (f" ({v.note})" if v.note else ""))
    lines.append(
        "goal 2: the write starts only after the treemap or JSON is out: held by the test "
        f"{WRITE_AFTER_OUTPUT_TEST} on every platform (DEV-108), not by these files"
    )
    share = _cold_share(docs.get("linux")) if "linux" in platforms else None
    if share is not None:
        lines.append(f"goal 2: the write's share of a cold T2 scan on Linux, recorded and no target (DEV-108): {share:.3f}")
    met = sum(v.status == "met" for v in found)
    missed = sum(v.status == "MISSED" for v in found)
    lines.append(f"{met} of {len(found)} targets met, {missed} missed, {len(found) - met - missed} not measured")
    return [ascii(line) for line in lines], met == len(found)


def _cold_share(doc: Optional[dict]) -> Optional[float]:
    if not doc:
        return None
    for t in (doc.get("index") or {}).get("trees") or []:
        if t.get("name") == "T2":
            share = (((t.get("write") or {}).get("cold") or {}).get("share_of_scan") or {}).get("p50")
            return float(share) if isinstance(share, (int, float)) else None
    return None


# --- The summary ----------------------------------------------------------------------


def _cell(v: Verdict) -> str:
    if v.value is None:
        return "not measured"
    unit = " µs" if not v.target.against else "×"
    return f"{v.value:.2f}{unit} {'met' if v.status == 'met' else '**missed**'}"


def _md(text: object) -> str:
    """`text` for a table cell or a list item."""
    return str(text).replace("|", "\\|")


def summary(docs: Dict[str, Optional[dict]], names: Dict[str, str]) -> str:
    """The Markdown summary of the files, every figure from them."""
    found = verdicts(docs)
    out = [
        "# The 1.0 acceptance run",
        "",
        "Generated by `bench/run.sh judge --summary` from the result files in this directory; do",
        "not edit it by hand. `bench/README.md` (\"The 1.0 acceptance run\") says how they are made.",
        "",
        "| Platform | File | Date | Machine | duscope |",
        "| --- | --- | --- | --- | --- |",
    ]
    for plat in PLATFORMS:
        doc = docs.get(plat)
        if doc is None:
            out.append(f"| {plat} | none | | | |")
            continue
        host = doc.get("host") or {}
        machine = f"{host.get('cpu') or host.get('arch')}, {host.get('logical_cpus')} CPUs, {host.get('os')}"
        commit = ((doc.get("harness") or {}).get("commit") or "unknown")[:12]
        version = (doc.get("duscope") or {}).get("version")
        out.append(f"| {plat} | `{names[plat]}` | {doc.get('date')} | {_md(machine)} | {version} at `{commit}` |")
    notes = [f"{plat}: {n}" for plat in PLATFORMS if docs.get(plat) for n in conditions(docs[plat])]  # type: ignore[arg-type]
    if notes:
        out += ["", *[f"- {n}" for n in notes]]
    for goal, title in ((1, "Goal 1: scan speed"), (2, "Goal 2: the index write")):
        out += ["", f"## {title}", "", "| Target | " + " | ".join(TREES) + " | Source |", "| --- |" + " --- |" * (len(TREES) + 1)]
        notes = []
        for target in (t for t in TARGETS if t.goal == goal):
            mine = [v for v in found if v.target is target]
            cells = {v.tree: _cell(v) for v in mine}
            row = [_md(target.what), *[cells.get(tree, "") for tree in TREES], _md(target.source)]
            out.append("| " + " | ".join(row) + " |")
            # Each note once, with the trees it holds for; a missing file is
            # said once, in the table of files above.
            said: Dict[str, List[str]] = {}
            for v in mine:
                if v.note and docs.get(target.platform) is not None:
                    said.setdefault(v.note, []).append(v.tree)
            notes += [f"- {_md(target.what)}, {', '.join(trees_)}: {_md(note)}" for note, trees_ in said.items()]
        if notes:
            out += ["", *notes]
    out += [
        "",
        f"The write starts only after the treemap or JSON is out: `{WRITE_AFTER_OUTPUT_TEST}` holds it on",
        "every platform (DEV-108).",
    ]
    share = _cold_share(docs.get("linux"))
    if share is not None:
        out.append(f"Its share of a cold T2 scan on Linux, recorded and no target: {share:.2f}.")
    out += [
        "",
        "## The incremental rescan against a full one",
        "",
        "| Platform | Tree | Directories changed | Directories read again | Incremental p50 | Full p50 | Incremental / full |",
        "| --- | --- | --- | --- | --- | --- | --- |",
    ]
    reasons: Dict[Tuple[str, str], List[str]] = {}
    for plat in PLATFORMS:
        doc = docs.get(plat)
        part = (doc or {}).get("rescan") or {}
        for t in part.get("trees") or []:
            if t.get("status") == "measured":
                read = t.get("rescanned_dirs") or [0]
                span = f"{min(read)}" if min(read) == max(read) else f"{min(read)}–{max(read)}"
                out.append(
                    f"| {plat} | {t['name']} | {t.get('changed_dirs')} of {t.get('dir_count')} | {span} | "
                    f"{t['incremental_p50_s']:.3f} s | {t['full_p50_s']:.3f} s | {t['incremental_over_full_p50']:.2f} |"
                )
            else:
                out.append(f"| {plat} | {t.get('name')} | not measured | | | | |")
            if t.get("reason"):
                reasons.setdefault((plat, str(t["reason"])), []).append(str(t.get("name")))
    if reasons:
        out += ["", *[f"- {plat} {', '.join(names_)}: {_md(reason)}" for (plat, reason), names_ in reasons.items()]]
    met = sum(v.status == "met" for v in found)
    out += ["", f"{met} of {len(found)} targets met.", ""]
    return "\n".join(out)


def cmd_judge(args, harness) -> int:
    paths = args.files or [os.path.join(args.dir, f"{p}.json") for p in PLATFORMS if os.path.exists(os.path.join(args.dir, f"{p}.json"))]
    docs: Dict[str, Optional[dict]] = {p: None for p in PLATFORMS}
    names: Dict[str, str] = {}
    try:
        for path in paths:
            doc = load(path)
            if docs[doc["platform"]] is not None:
                raise harness.UsageError(f"two {doc['platform']} files: {names[doc['platform']]} and {path}")
            docs[doc["platform"]] = doc
            names[doc["platform"]] = os.path.basename(path)
    except InputError as e:
        print(f"bench: {e}", file=sys.stderr)
        return 2
    lines, all_met = report(docs, names)
    for line in lines:
        print(line)
    if args.summary:
        with open(args.summary + ".tmp", "w", encoding="utf-8") as f:
            f.write(summary(docs, names))
        os.replace(args.summary + ".tmp", args.summary)
        harness.log(f"wrote {args.summary}")
    return 0 if all_met else 1


# --- The run -----------------------------------------------------------------------------


def _part(what: str, failures: List[str], harness, run_it) -> Optional[dict]:
    """Runs one measuring subcommand with its stdout (the file's path) on
    stderr, and reads the file it wrote; None, and a failure, when it could
    not."""
    try:
        with contextlib.redirect_stdout(sys.stderr):
            status, out = run_it()
        with open(out, encoding="utf-8") as f:
            doc = json.load(f)
    except (measure.MeasureError, tools.ToolError, trees.TreeError, OSError, ValueError) as e:
        failures.append(f"{what}: {e}")
        harness.log(f"{what}: not measured: {e}")
        return None
    if status != 0:
        failures.append(f"{what}: exited {status}; the file's `tools` and `failures` say what was not measured")
    return doc


def cmd_acceptance(args, harness) -> int:
    key = tools.platform_key()
    plat = key.split("-")[0]
    if args.platform and args.platform != plat:
        raise harness.UsageError(f"--platform {args.platform}, but this machine is {plat}: its file would be the wrong platform's")
    if args.runs < 1 or args.index_runs < 1 or args.changed_dirs < 1 or args.warmup < 0 or args.settle < 0:
        raise harness.UsageError(
            "--runs, --index-runs and --changed-dirs must be at least 1, --warmup and --settle at least 0"
        )
    overrides = harness._pairs(args.tool_path, "--tool-path")
    if "duscope" in overrides:
        raise harness.UsageError("--tool-path duscope=...: acceptance measures the --duscope it is given")
    unknown = [n for n in overrides if n not in tools.PINS]
    if unknown:
        raise harness.UsageError(f"--tool-path names unknown tools: {', '.join(unknown)}")
    tree_list = harness._trees(args)
    competitors = harness._names(args.competitors, tools.COMPETITORS + ("du",), "competitor", allow_none=True)
    if args.cold_competitors is not None:
        harness._names(args.cold_competitors, tools.COMPETITORS + ("du",), "competitor", allow_none=True)
    # What would stop a part hours in stops the run before it starts.
    measure.hyperfine_version(shlex.split(args.hyperfine))
    if args.mode != "warm":
        if not args.cold_cmd.strip():
            raise harness.UsageError("--cold-cmd is empty: a cold run needs the command that drops the caches before every run")
        measure.check_cold(args.cold_cmd)
    duscope = tools.resolve(tools.PINS["duscope"], key, args.tools_dir, False, args.duscope, harness.log)
    if duscope.status != "ready" or duscope.path is None:
        raise measure.MeasureError(f"duscope at {args.duscope} is unavailable: {duscope.reason}")
    scaled = any(float(t.scale) != 1.0 for t in tree_list)
    out = args.out or os.path.join(args.out_dir, f"{plat}" + (f"-x{float(args.scale):g}" if scaled else "") + ".json")

    common = ["--scale", str(args.scale), "--jobs", str(args.jobs), "--mode", args.mode, "--cold-cmd", args.cold_cmd,
              "--warmup", str(args.warmup), "--trees-dir", args.trees_dir, "--tools-dir", args.tools_dir,
              "--hyperfine", args.hyperfine]
    if args.cold_trees_dir:
        common += ["--cold-trees-dir", args.cold_trees_dir]
    started = time.gmtime()
    failures: List[str] = []
    work = tempfile.mkdtemp(prefix="duscope-acceptance-")
    try:
        run_out = os.path.join(work, "run.json")
        argv = ["run", "--subject", "duscope", "--tool-path", f"duscope={duscope.path}", "--trees", args.trees,
                "--competitors", args.competitors, "--runs", str(args.runs), "--out", run_out, *common]
        if args.cold_competitors is not None:
            argv += ["--cold-competitors", args.cold_competitors]
        for name, path in overrides.items():
            argv += ["--tool-path", f"{name}={path}"]
        run_args = harness.parser().parse_args(argv)
        harness.log("the harness: " + " ".join(argv))
        run_doc = _part("run", failures, harness, lambda: (harness.cmd_run(run_args), run_out))

        index_doc = None
        if any(t.name == "T2" for t in tree_list):
            index_out = os.path.join(work, "index.json")
            d34 = ",".join(c for c in indexbench.COMPETITORS if c in competitors) or "none"
            argv = ["index", "--duscope", duscope.path, "--trees", "T2", "--competitors", d34,
                    "--runs", str(args.index_runs), "--out", index_out, *common]
            for name, path in overrides.items():
                if name in indexbench.COMPETITORS:
                    argv += ["--tool-path", f"{name}={path}"]
            index_args = harness.parser().parse_args(argv)
            harness.log("the index measurements: " + " ".join(argv))
            index_doc = _part("index", failures, harness, lambda: (indexbench.cmd_index(index_args, harness), index_out))
        else:
            harness.log("the index measurements read T2, which --trees leaves out: not measured")

        harness.log(f"the rescans, on the warm trees in {args.trees_dir}")
        rescan_doc, lost = rescanbench.measure_rescans(
            duscope.path, tree_list, args.trees_dir, args.runs, args.settle, args.changed_dirs, harness.log
        )
        failures += lost
    finally:
        shutil.rmtree(work, ignore_errors=True)
    finished = time.gmtime()
    doc = {
        "schema_version": harness.SCHEMA_VERSION,
        "kind": "acceptance",
        "date": time.strftime("%Y-%m-%d", started),
        "started_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", started),
        "finished_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", finished),
        "platform": plat,
        "host": harness.host(),
        "harness": {"commit": harness.git_commit(), "python": platform_module.python_version(), "scale": float(args.scale)},
        "duscope": {"path": duscope.path, "version": duscope.version},
        "run": run_doc,
        "index": index_doc,
        "rescan": rescan_doc,
        "failures": failures,
    }
    harness.write_result(doc, out)
    for line in report({plat: doc}, {plat: os.path.basename(out)}, (plat,))[0]:
        harness.log(line)
    for failure in failures:
        harness.log(f"not measured: {failure}")
    print(out)
    return 1 if failures else 0
