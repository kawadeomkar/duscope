"""duscope's benchmark harness: `bench/run.sh` (and `just bench`) run this.

    run [options]      build the trees, install the tools, measure, write the result
    build [options]    build the trees only
    install [options]  install the pinned tools only
    verify [options]   walk built trees and compare them with their stamps
    tools              list the pinned tools and how each installs per platform
    show FILE          print a result file as a table

`run` is the default. bench/README.md documents every option and the result
file. Standard library only; Python 3.9 or later.
"""

from __future__ import annotations

import argparse
import json
import os
import platform as platform_module
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
from typing import Dict, List, Optional

if sys.version_info < (3, 9):  # pragma: no cover - the message is the point
    sys.exit("bench: Python 3.9 or later is needed")

import measure
import sweep
import tools
import trees

BENCH = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(BENCH)
SCHEMA_VERSION = 1


class UsageError(Exception):
    """A bad combination of options (exit status 2)."""


def log(message: str) -> None:
    print(f"bench: {message}", file=sys.stderr, flush=True)


# --- Options -------------------------------------------------------------------


def _names(value: str, known, what: str, allow_none: bool = False) -> List[str]:
    if value == "all":
        return list(known)
    if allow_none and value == "none":
        return []
    names = [n.strip() for n in value.split(",") if n.strip()]
    unknown = [n for n in names if n not in known]
    if unknown or not names:
        raise UsageError(f"unknown {what} {', '.join(unknown) or repr(value)}: choose from {', '.join(known)} or all")
    return list(dict.fromkeys(names))


def _pairs(values: Optional[List[str]], flag: str) -> Dict[str, str]:
    out: Dict[str, str] = {}
    for value in values or []:
        name, sep, rest = value.partition("=")
        if not sep or not name or not rest:
            raise UsageError(f"{flag} takes NAME=VALUE, not {value!r}")
        out[name] = rest
    return out


def _tree_options(p: argparse.ArgumentParser) -> None:
    p.add_argument("--trees", default="T1", help="T1, T2, T3, a comma-separated list, or all (default T1)")
    p.add_argument("--scale", type=float, default=1.0, help="build the trees scaled by this factor, 0 < F <= 1 (default 1)")
    p.add_argument("--trees-dir", default=os.path.join(BENCH, ".trees"), help="where the trees live (default bench/.trees)")


def _jobs_option(p: argparse.ArgumentParser) -> None:
    p.add_argument("--jobs", type=int, default=min(4, os.cpu_count() or 1), help="processes that create files (default 4)")


def _tool_options(p: argparse.ArgumentParser, default_competitors: str) -> None:
    p.add_argument(
        "--competitors", default=default_competitors,
        help=f"pinned tools to pair with the subject: a comma-separated list, all or none (default {default_competitors})",
    )
    p.add_argument("--tools-dir", default=os.path.join(BENCH, ".tools"), help="where pinned tools install (default bench/.tools)")
    p.add_argument("--tool-path", action="append", metavar="NAME=PATH", help="use this executable for a pinned tool (repeatable)")


def parser() -> argparse.ArgumentParser:
    top = argparse.ArgumentParser(prog="bench/run.sh", description="duscope's benchmark harness (bench/README.md)")
    sub = top.add_subparsers(dest="subcommand", required=True)

    run = sub.add_parser("run", help="measure the subject and the competitors on the trees")
    _tree_options(run)
    _jobs_option(run)
    run.add_argument("--subject", default="du", help="the tool every competitor is paired with (default du)")
    _tool_options(run, "diskus")
    run.add_argument("--command", action="append", metavar="NAME=CMD", help="also pair this command line with the subject; {tree} is the tree (repeatable)")
    run.add_argument("--mode", choices=("warm", "cold", "both"), default="warm", help="warm caches, cold caches, or both (default warm)")
    run.add_argument("--cold-cmd", default=shlex.quote(os.path.join(BENCH, "drop-caches.sh")), help="the command that drops the caches before each cold run (default bench/drop-caches.sh)")
    run.add_argument("--cold-trees-dir", help="where the trees the cold runs measure live (default --trees-dir)")
    run.add_argument("--cold-competitors", help="the pinned tools paired with the subject in cold runs: a comma-separated list, all or none (default --competitors)")
    run.add_argument("--runs", type=int, default=10, help="measured runs of each tool in each pair (default 10)")
    run.add_argument("--warmup", type=int, default=2, help="warm-up runs of each tool at the start of each warm pair (default 2)")
    run.add_argument("--results-dir", default=os.path.join(BENCH, "results"), help="where the result file goes (default bench/results)")
    run.add_argument("--out", help="the result file (default <results-dir>/<date>-<platform>.json)")
    run.add_argument("--hyperfine", default="hyperfine", help="the hyperfine command (default hyperfine on PATH)")
    run.add_argument("--keep-raw", action="store_true", help="keep hyperfine's per-round exports under bench/.runs/")

    build = sub.add_parser("build", help="build the trees")
    _tree_options(build)
    _jobs_option(build)
    build.add_argument("--force", action="store_true", help="rebuild even when the stamp matches")

    install = sub.add_parser("install", help="install the pinned tools")
    _tool_options(install, "all")

    verify = sub.add_parser("verify", help="walk built trees and compare their digests with the stamps")
    _tree_options(verify)

    sub.add_parser("tools", help="list the pinned tools, one tab-separated line each")

    show = sub.add_parser("show", help="print a result file as a table")
    show.add_argument("file")

    swept = sub.add_parser("sweep", help="measure duscope at each thread count (P1-BENCH-01)")
    _tree_options(swept)
    _jobs_option(swept)
    swept.add_argument("--duscope", required=True, metavar="PATH", help="the duscope executable to measure")
    swept.add_argument("--threads", default=",".join(map(str, sweep.THREADS)), help="the thread counts, comma-separated (default 1 to 8, then the steps up to 64)")
    swept.add_argument("--mode", choices=("warm", "cold", "both"), default="both", help="warm caches, cold caches, or both (default both)")
    swept.add_argument("--cold-cmd", default=shlex.quote(os.path.join(BENCH, "drop-caches.sh")), help="the command that drops the caches before each cold run (default bench/drop-caches.sh)")
    swept.add_argument("--cold-trees-dir", help="where the trees the cold runs measure live (default --trees-dir)")
    swept.add_argument("--runs", type=int, default=5, help="measured runs at each thread count (default 5)")
    swept.add_argument("--warmup", type=int, default=1, help="warm-up runs before each warm count (default 1)")
    swept.add_argument("--tools-dir", default=os.path.join(BENCH, ".tools"), help=argparse.SUPPRESS)
    swept.add_argument("--results-dir", default=os.path.join(BENCH, "results"), help="where the result file goes (default bench/results)")
    swept.add_argument("--out", help="the result file (default <results-dir>/thread-sweep-<platform>-<arch>.json)")
    swept.add_argument("--hyperfine", default="hyperfine", help="the hyperfine command (default hyperfine on PATH)")
    return top


# --- Host facts ----------------------------------------------------------------


def _output(argv: List[str]) -> Optional[str]:
    try:
        done = subprocess.run(argv, capture_output=True, text=True, timeout=10, errors="replace")
    except (OSError, subprocess.TimeoutExpired):
        return None
    return done.stdout.strip() if done.returncode == 0 and done.stdout.strip() else None


def platform_name() -> str:
    return tools.platform_key().split("-")[0]


def host() -> Dict[str, object]:
    uname = os.uname()
    cpu: Optional[str] = None
    memory: Optional[int] = None
    if sys.platform == "darwin":
        name = _output(["sw_vers", "-productName"]) or "macOS"
        version = _output(["sw_vers", "-productVersion"]) or platform_module.mac_ver()[0]
        build = _output(["sw_vers", "-buildVersion"])
        os_name = f"{name} {version}" + (f" ({build})" if build else "")
        cpu = _output(["sysctl", "-n", "machdep.cpu.brand_string"])
        memsize = _output(["sysctl", "-n", "hw.memsize"])
        memory = int(memsize) if memsize and memsize.isdigit() else None
    else:
        os_name = "Linux"
        try:
            with open("/etc/os-release", encoding="utf-8") as f:
                for line in f:
                    if line.startswith("PRETTY_NAME="):
                        os_name = line.split("=", 1)[1].strip().strip('"')
        except OSError:
            pass
        try:
            with open("/proc/cpuinfo", encoding="utf-8", errors="replace") as f:
                for line in f:
                    if line.lower().startswith("model name"):
                        cpu = line.split(":", 1)[1].strip()
                        break
        except OSError:
            pass
        if cpu is None:
            for line in (_output(["lscpu"]) or "").splitlines():
                if line.startswith("Model name:"):
                    # lscpu prints "-" when the kernel does not name the core.
                    cpu = line.split(":", 1)[1].strip().strip("-").strip() or None
                    break
        try:
            with open("/proc/meminfo", encoding="utf-8") as f:
                for line in f:
                    if line.startswith("MemTotal:"):
                        memory = int(line.split()[1]) * 1024
                        break
        except (OSError, ValueError):
            pass
    return {
        "os": os_name,
        "kernel": f"{uname.sysname} {uname.release}",
        "arch": uname.machine,
        "cpu": cpu,
        "logical_cpus": os.cpu_count() or 1,
        "memory_bytes": memory,
    }


def filesystem(path: str) -> Optional[str]:
    """The filesystem type of the volume holding `path`."""
    real = os.path.realpath(path)
    mounts = []
    if sys.platform == "darwin":
        for line in (_output(["mount"]) or "").splitlines():
            m = re.match(r"^.* on (.*) \(([^,)]+)", line)
            if m:
                mounts.append((m.group(1), m.group(2)))
    else:
        try:
            with open("/proc/self/mounts", encoding="utf-8", errors="replace") as f:
                for line in f:
                    fields = line.split()
                    if len(fields) >= 3:
                        point = re.sub(r"\\([0-7]{3})", lambda m: chr(int(m.group(1), 8)), fields[1])
                        mounts.append((point, fields[2]))
        except OSError:
            return None
    best = None
    for point, kind in mounts:
        inside = real == point or real.startswith(point.rstrip("/") + "/")
        if inside and (best is None or len(point) >= len(best[0])):
            best = (point, kind)
    return best[1] if best else None


def git_commit() -> Optional[str]:
    return _output(["git", "-C", REPO, "rev-parse", "HEAD"])


# --- Commands ------------------------------------------------------------------


def _trees(args) -> List[trees.Tree]:
    names = _names(args.trees, trees.SPECS, "tree")
    try:
        return [trees.tree(n, args.scale) for n in names]
    except trees.TreeError as e:
        raise UsageError(str(e)) from None


def _resolve_tools(args, names: List[str], key: str) -> List[tools.Resolved]:
    overrides = _pairs(args.tool_path, "--tool-path")
    unknown = [n for n in overrides if n not in tools.PINS]
    if unknown:
        raise UsageError(f"--tool-path names unknown tools: {', '.join(unknown)}")
    return [
        tools.resolve(tools.PINS[n], key, args.tools_dir, True, overrides.get(n), log)
        for n in names
    ]


def cmd_build(args) -> int:
    for t in _trees(args):
        trees.build(t, args.trees_dir, args.jobs, force=args.force, log=log)
    return 0


def cmd_install(args) -> int:
    key = tools.platform_key()
    names = _names(args.competitors, tools.PINS, "tool", allow_none=True)
    failed = 0
    for r in _resolve_tools(args, names, key):
        detail = r.version or r.reason or ""
        print(f"{r.tool.name}\t{r.status}\t{r.source}\t{r.path or '-'}\t{detail}")
        failed += r.status == "failed"
    return 1 if failed else 0


def cmd_verify(args) -> int:
    bad = 0
    for t in _trees(args):
        observed, expected = trees.verify(t, args.trees_dir)
        ok = observed == expected
        bad += not ok
        log(f"{t.dirname}: {'matches' if ok else 'DIFFERS FROM'} its stamp (walk {observed}, stamp {expected})")
    return 1 if bad else 0


def cmd_tools(_args) -> int:
    print("\t".join(["name", "pinned", "command", *tools.PLATFORMS]))
    for tool in tools.PINS.values():
        sources = [tools.source_for(tool, key)[0] for key in tools.PLATFORMS]
        print("\t".join([tool.name, tool.pinned or "-", " ".join(tool.args), *sources]))
    return 0


def cmd_show(args, out=None) -> int:
    with open(args.file, encoding="utf-8") as f:
        result = json.load(f)

    def say(line: str = "") -> None:
        print(line, file=out or sys.stdout)

    h = result["host"]
    say(f"{result['date']} {result['platform']}: {h['os']}, {h['cpu'] or h['arch']}, {h['logical_cpus']} CPUs")
    say(f"subject {result['subject']}; hyperfine {result['harness']['hyperfine']}, {result['harness']['runs']} runs per pair")
    for t in result["trees"]:
        say(f"{t['name']} (scale {t['scale']:g}): {t['dirs']:,} directories, {t['files']:,} files on {t['filesystem'] or 'unknown'}, digest {t['digest']}")
    for tool in result["tools"]:
        if tool["status"] != "measured":
            say(f"{tool['name']}: {tool['status']}: {tool['reason']}")
    subject = result["subject"]
    current = None
    for r in result["results"]:
        if (r["tree"], r["mode"]) != current:
            current = (r["tree"], r["mode"])
            say(f"\n{r['tree']} {r['mode']}{'':<16}{'p50':>10}{'p95':>10}{subject + ' / tool':>16}")
        ratio = f"{r['subject_ratio_p50']:.2f}x" if r["subject_ratio_p50"] is not None else "-"
        say(f"  {r['tool']:<22}{r['p50_s']:>9.3f}s{r['p95_s']:>9.3f}s{ratio:>16}")
    return 0


def cmd_run(args) -> int:
    if args.runs < 1 or args.warmup < 0:
        raise UsageError("--runs must be at least 1 and --warmup at least 0")
    key = tools.platform_key()
    plat = key.split("-")[0]
    modes = ["warm", "cold"] if args.mode == "both" else [args.mode]
    tree_list = _trees(args)
    competitors = _names(args.competitors, tools.COMPETITORS + ("du",), "competitor", allow_none=True)
    cold_competitors = competitors
    if args.cold_competitors is not None:
        cold_competitors = _names(args.cold_competitors, tools.COMPETITORS + ("du",), "competitor", allow_none=True)
    commands = _pairs(args.command, "--command")
    if args.subject not in tools.PINS:
        raise UsageError(f"unknown subject {args.subject!r}: choose from {', '.join(tools.PINS)}")
    competitors = [c for c in competitors if c != args.subject]
    cold_competitors = [c for c in cold_competitors if c != args.subject]
    # Each mode's tools and trees: cold runs can measure fewer tools, on trees in
    # a directory of their own (bench/README.md, "Cold runs").
    wanted = {"warm": set(competitors) | set(commands), "cold": set(cold_competitors) | set(commands)}
    tree_dirs = {"warm": args.trees_dir, "cold": args.cold_trees_dir or args.trees_dir}
    clash = [n for n in commands if n in tools.PINS or n == args.subject]
    if clash:
        raise UsageError(f"--command names clash with pinned tools: {', '.join(clash)}")
    hyperfine = shlex.split(args.hyperfine)

    if "cold" in modes and not args.cold_cmd.strip():
        raise UsageError("--cold-cmd is empty: a cold run needs the command that drops the caches before every run")

    started = time.gmtime()
    hyperfine_version = measure.hyperfine_version(hyperfine)
    if "cold" in modes:
        measure.check_cold(args.cold_cmd)

    subject = _resolve_tools(args, [args.subject], key)[0]
    if subject.status != "ready":
        raise measure.MeasureError(f"the subject {subject.tool.name} is unavailable: {subject.reason}")
    # The tools of the modes this run measures, in the order they were named.
    names: List[str] = []
    for mode in modes:
        names += [c for c in (competitors if mode == "warm" else cold_competitors) if c not in names]
    others = _resolve_tools(args, names, key)
    for name, command in commands.items():
        try:
            others.append(tools.ad_hoc(name, command))
        except tools.ToolError as e:
            raise UsageError(str(e)) from None
    for r in others:
        if r.status != "ready":
            log(f"{r.tool.name}: {r.status}: {r.reason}")

    # The trees in each directory a mode measures; the first mode's describe them
    # in the result file.
    dirs = list(dict.fromkeys(tree_dirs[m] for m in modes))
    built = {d: [trees.build(t, d, args.jobs, log=log) for t in tree_list] for d in dirs}
    stamps = built[dirs[0]]

    # hyperfine's per-round exports go to a temporary directory, removed however
    # the run ends; --keep-raw keeps them under bench/.runs/ instead.
    raw_parent = os.path.join(BENCH, ".runs") if args.keep_raw else None
    if raw_parent:
        os.makedirs(raw_parent, exist_ok=True)
    raw_dir = tempfile.mkdtemp(prefix=time.strftime("%Y%m%dT%H%M%SZ-", started), dir=raw_parent)
    try:
        failures: Dict[str, str] = {}
        results: List[Dict[str, object]] = []
        measured = set()
        smoked = set()
        for t in tree_list:
            for mode in modes:
                # Each tool runs once on each tree it is measured on before hyperfine
                # does, so a failure names the tool.
                path = trees.tree_root(t, tree_dirs[mode])
                if (subject.tool.name, path) not in smoked:
                    measure.smoke(subject, path)
                    smoked.add((subject.tool.name, path))
                ready = []
                for r in others:
                    if r.status != "ready" or r.tool.name in failures or r.tool.name not in wanted[mode]:
                        continue
                    if (r.tool.name, path) not in smoked:
                        try:
                            measure.smoke(r, path)
                        except measure.MeasureError as e:
                            failures[r.tool.name] = str(e)
                            log(f"{r.tool.name}: failed on {t.name}: {e}")
                            continue
                        smoked.add((r.tool.name, path))
                    ready.append(r)
                session = measure.Session(hyperfine, t.name, path, mode, args.runs, args.warmup, args.cold_cmd, raw_dir, log)
                subject_command = subject.command()
                if not ready:
                    results.append(measure.row(t.name, mode, subject.tool.name, subject_command, session.alone(subject)))
                    measured.add(subject.tool.name)
                    continue
                subject_runs: List[measure.Run] = []
                rows = []
                for r in ready:
                    if r.tool.name in failures:
                        continue
                    try:
                        s_runs, o_runs = session.pair(subject, r)
                    except measure.MeasureError as e:
                        failures[r.tool.name] = str(e)
                        log(f"{r.tool.name}: failed on {t.name} {mode}: {e}")
                        continue
                    subject_runs += s_runs
                    rows.append(measure.row(t.name, mode, r.tool.name, r.command(), o_runs, subject.tool.name, s_runs))
                    measured.add(r.tool.name)
                if not subject_runs:
                    subject_runs = session.alone(subject)
                results.append(measure.row(t.name, mode, subject.tool.name, subject_command, subject_runs))
                results.extend(rows)
                measured.add(subject.tool.name)
    finally:
        if not args.keep_raw:
            shutil.rmtree(raw_dir, ignore_errors=True)
    finished = time.gmtime()

    def tool_entry(r: tools.Resolved) -> Dict[str, object]:
        name = r.tool.name
        if name in failures:
            status, reason = "failed", failures[name]
        elif name in measured:
            status, reason = "measured", None
        else:
            status, reason = ("skipped", r.reason) if r.status == "skipped" else ("failed", r.reason)
        return {
            "name": name,
            "pinned": r.tool.pinned,
            "version": r.version,
            "source": r.source,
            "path": r.path,
            "built_with": r.built_with,
            "status": status,
            "reason": reason,
        }

    date = time.strftime("%Y-%m-%d", started)
    result = {
        "schema_version": SCHEMA_VERSION,
        "date": date,
        "started_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", started),
        "finished_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", finished),
        "platform": plat,
        "subject": subject.tool.name,
        "host": host(),
        "harness": {
            "commit": git_commit(),
            "hyperfine": hyperfine_version,
            "python": platform_module.python_version(),
            "runs": args.runs,
            "warmup": args.warmup,
            "cold_command": args.cold_cmd if "cold" in modes else None,
            "cold_competitors": cold_competitors if "cold" in modes and args.cold_competitors is not None else None,
        },
        "trees": [
            {
                "name": t.name,
                "shape": t.spec.shape,
                "seed": t.spec.seed,
                "scale": float(t.scale),
                "dirs": t.dirs,
                "files": t.files,
                "apparent_bytes": stamp["apparent_bytes"],
                "digest": stamp["digest"],
                "filesystem": filesystem(trees.tree_root(t, dirs[0])),
                "cold_filesystem": filesystem(trees.tree_root(t, dirs[1])) if len(dirs) > 1 else None,
                "build_s": float(stamp["build_s"]),
            }
            for t, stamp in zip(tree_list, stamps)
        ],
        "tools": [tool_entry(r) for r in [subject, *others]],
        "results": results,
    }
    out = args.out or os.path.join(args.results_dir, f"{date}-{plat}.json")
    write_result(result, out)
    if sys.stderr.isatty():
        cmd_show(argparse.Namespace(file=out), out=sys.stderr)
    print(out)
    failed = [e["name"] for e in result["tools"] if e["status"] == "failed"]
    if failed:
        log(f"not measured: {', '.join(failed)} (see `tools` in the result file)")
        return 1
    return 0


def write_result(result: Dict[str, object], out: str) -> None:
    """Writes a result file whole, or not at all."""
    os.makedirs(os.path.dirname(os.path.abspath(out)), exist_ok=True)
    with open(out + ".tmp", "w", encoding="utf-8") as f:
        json.dump(result, f, indent=2)
        f.write("\n")
    os.replace(out + ".tmp", out)
    log(f"wrote {out}")


COMMANDS = {
    "sweep": lambda args: sweep.cmd_sweep(args, sys.modules[__name__]),
    "run": cmd_run,
    "build": cmd_build,
    "install": cmd_install,
    "verify": cmd_verify,
    "tools": cmd_tools,
    "show": cmd_show,
}


def main(argv: List[str]) -> int:
    if not argv or argv[0].startswith("-") and argv[0] not in ("-h", "--help"):
        argv = ["run", *argv]
    args = parser().parse_args(argv)
    try:
        return COMMANDS[args.subcommand](args)
    except UsageError as e:
        print(f"bench: {e}", file=sys.stderr)
        return 2
    except (measure.MeasureError, tools.ToolError, trees.TreeError, OSError) as e:
        print(f"bench: {e}", file=sys.stderr)
        return 1
    except KeyboardInterrupt:
        return 130


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
