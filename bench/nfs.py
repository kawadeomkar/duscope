"""What a scan of an NFS mount asks the server for (P5-BENCH-07; bench/README.md, "NFS").

P1-WALK-02 fans one directory's `statx` calls across workers in batches, the pattern the Linux
NFS client's READDIRPLUS heuristic is least likely to recognise, since it watches for stats that
follow a listing; a scan with `--include-network` may then pay a round trip per entry. This
measures it once: an NFS export of a 100k-file tree (T1 at scale 0.25: 25,000 directories and
100,000 sparse files) on one VM, mounted on another, and for a duscope scan, for GNU `du` and for
a duscope scan with `--threads 1`, the time and the mount's per-operation counts from
/proc/self/mountstats, each run on a fresh mount so that no attribute or dentry cache carries over.

    nfs.py tree DIR                       (server) build the tree under DIR, to export DIR/T1-x0.25
    nfs.py run --export HOST:PATH --mountpoint DIR --duscope BIN [--du du] [--vers 4.2]
               [--runs 3] [--out FILE]    (client, as root) measure, write the result file
    nfs.py judge FILE                     the decision gate's answer from a result file
    nfs.py counts MOUNTSTATS MOUNTPOINT   one mount's per-operation counts (the tests read it)

The decision gate **stats on network filesystems** (plan/SPEC.md, Gate 5 -> 1.0) has a fixed
rule: changed when the scan issues more than one GETATTR per ten entries where `du` issues none.
"None" is read as at most one per thousand entries, which leaves the mount's own few calls out.
NFSv4 has no READDIRPLUS operation (its READDIR carries the attributes the client asks for), so
under `--vers 4.x` that count is null; `--vers 3` measures the protocol that has one.

Python 3.9+ standard library only, like the rest of bench/.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import statistics
import subprocess
import sys
import time
from typing import Dict, List, Optional, Sequence

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import harness  # noqa: E402  (host facts and the commit, as every result file has them)
import trees  # noqa: E402

TREE = ("T1", 0.25)
# The counts the row names, and the two that show where a stat's round trip went instead.
OPS = ("READDIR", "READDIRPLUS", "GETATTR", "LOOKUP", "ACCESS")
RULE = "changed when the scan issues more than one GETATTR per ten entries where du issues none"
SCAN_LIMIT = 0.1  # GETATTR per entry above which the scan pays for its stats
NONE_BELOW = 0.001  # GETATTR per entry at or below which du "issues none"


class NfsError(Exception):
    pass


def say(message: str) -> None:
    print(f"nfs: {message}", file=sys.stderr, flush=True)


def mount_block(mountstats: str, mountpoint: str) -> Optional[str]:
    """The lines /proc/self/mountstats holds for the NFS mount at `mountpoint`, or None."""
    want = mountpoint.rstrip("/") or "/"
    blocks = re.split(r"(?m)^(?=device )", mountstats)
    for block in blocks:
        m = re.match(r"device (\S+) mounted on (\S+) with fstype (nfs\S*)", block)
        if m and m.group(2).replace("\\040", " ") == want:
            return block
    return None


def counts(mountstats: str, mountpoint: str) -> Dict[str, object]:
    """One mount's options and per-operation counts: {"fstype", "opts", "ops": {OP: n}}."""
    block = mount_block(mountstats, mountpoint)
    if block is None:
        raise NfsError(f"no NFS mount at {mountpoint} in mountstats")
    fstype = re.match(r"device \S+ mounted on \S+ with fstype (\S+)", block).group(1)
    opts = re.search(r"(?m)^\s*opts:\s*(\S+)", block)
    ops: Dict[str, int] = {}
    per_op = block.split("per-op statistics", 1)
    if len(per_op) == 2:
        for line in per_op[1].splitlines():
            m = re.match(r"\s*([A-Z][A-Z0-9_]*): (\d+)", line)
            if m:
                ops[m.group(1)] = int(m.group(2))
    return {"fstype": fstype, "opts": opts.group(1) if opts else None, "ops": ops}


def mount_options(opts: Optional[str]) -> Optional[str]:
    """The options without the machines' addresses (`addr=`, `clientaddr=`), which say where
    the run was and nothing about how the mount behaves."""
    if opts is None:
        return None
    return ",".join(o for o in opts.split(",") if not o.startswith(("addr=", "clientaddr=", "mountaddr=")))


def delta(before: Dict[str, int], after: Dict[str, int]) -> Dict[str, Optional[int]]:
    """The counts of OPS a run added; None for an operation the protocol does not have."""
    return {op: (after[op] - before.get(op, 0)) if op in after else None for op in OPS}


def judge(result: Dict[str, object]) -> Dict[str, object]:
    """The gate's answer from a result file's commands and tree."""
    entries = result["tree"]["entries"]
    by_name = {c["name"]: c for c in result["commands"]}

    def rate(name: str, op: str) -> Optional[float]:
        n = by_name[name]["ops_p50"].get(op)
        return None if n is None else n / entries

    scan, du = rate("duscope", "GETATTR"), rate("du", "GETATTR")
    answer = "changed" if scan > SCAN_LIMIT and du <= NONE_BELOW else "kept"
    return {
        "gate": "stats on network filesystems",
        "rule": RULE,
        "scan_above": SCAN_LIMIT,
        "none_at_or_below": NONE_BELOW,
        "getattr_per_entry": {name: rate(name, "GETATTR") for name in by_name},
        "lookup_per_entry": {name: rate(name, "LOOKUP") for name in by_name},
        "answer": answer,
    }


def _mountstats() -> str:
    with open("/proc/self/mountstats", encoding="utf-8", errors="replace") as f:
        return f.read()


def _run(argv: Sequence[str], check: bool = True) -> subprocess.CompletedProcess:
    p = subprocess.run(list(argv), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    if check and p.returncode != 0:
        raise NfsError(f"{' '.join(argv)} exited {p.returncode}: {p.stderr.strip()[:300]}")
    return p


def _mount(export: str, mountpoint: str, vers: str) -> None:
    fstype = "nfs4" if vers.startswith("4") else "nfs"
    _run(["mount", "-t", fstype, "-o", f"vers={vers},ro", export, mountpoint])


def _umount(mountpoint: str) -> None:
    _run(["umount", mountpoint])


def _count_entries(mountpoint: str) -> Dict[str, int]:
    """Directories and other entries below the mount's root, from listings alone."""
    dirs = files = 0
    stack = [mountpoint]
    while stack:
        with os.scandir(stack.pop()) as it:
            for e in it:
                if e.is_dir(follow_symlinks=False):
                    dirs += 1
                    stack.append(e.path)
                else:
                    files += 1
    return {"dirs": dirs, "files": files, "entries": dirs + files}


def commands(duscope: str, du: str, mountpoint: str) -> List[Dict[str, object]]:
    scan = [duscope, "scan", "--json", "--treemap", "never", "--include-network"]
    return [
        {"name": "duscope", "command": scan + [mountpoint]},
        {"name": "du", "command": [du, "-sk", mountpoint]},
        {"name": "duscope --threads 1", "command": scan + ["--threads", "1", mountpoint]},
    ]


def cmd_run(args) -> int:
    if os.geteuid() != 0:
        raise NfsError("run mounts the export: run it as root")
    if not os.path.isdir(args.mountpoint):
        raise NfsError(f"{args.mountpoint} is not a directory")
    if os.path.ismount(args.mountpoint):
        raise NfsError(f"{args.mountpoint} is already a mount point: unmount it first")
    plan = commands(args.duscope, args.du, args.mountpoint)
    for c in plan:
        first = c["command"][0]
        c["version"] = (_run([first, "--version"], check=False).stdout.strip().splitlines() or [None])[0]
        c["runs"] = []
    started = time.time()
    say(f"warm-up: du on a fresh mount of {args.export} (the server's cache then holds the tree)")
    _mount(args.export, args.mountpoint, args.vers)
    try:
        _run(plan[1]["command"])
        mount_facts = counts(_mountstats(), args.mountpoint)
        tree_counts = _count_entries(args.mountpoint)
    finally:
        _umount(args.mountpoint)
    # Round-robin, so that the commands share whatever drifts between runs.
    for r in range(args.runs):
        for c in plan:
            _mount(args.export, args.mountpoint, args.vers)
            try:
                before = counts(_mountstats(), args.mountpoint)["ops"]
                t0 = time.monotonic()
                p = _run(c["command"], check=False)
                elapsed = time.monotonic() - t0
                after = counts(_mountstats(), args.mountpoint)["ops"]
            finally:
                _umount(args.mountpoint)
            if p.returncode != 0:
                raise NfsError(f"{c['name']} exited {p.returncode}: {p.stderr.strip()[:300]}")
            ops = delta(before, after)
            c["runs"].append({"time_s": round(elapsed, 4), "ops": ops})
            say(f"run {r + 1}/{args.runs} {c['name']}: {elapsed:.2f} s, "
                + ", ".join(f"{op} {n}" for op, n in ops.items() if n is not None))
    for c in plan:
        c["p50_s"] = round(statistics.median(run["time_s"] for run in c["runs"]), 4)
        c["ops_p50"] = {
            op: (None if any(run["ops"][op] is None for run in c["runs"])
                 else int(statistics.median(run["ops"][op] for run in c["runs"])))
            for op in OPS
        }
    t = trees.tree(*TREE)
    result: Dict[str, object] = {
        "schema_version": 1,
        "kind": "nfs-readdirplus",
        "date": time.strftime("%Y-%m-%d", time.gmtime(started)),
        "started_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(started)),
        "finished_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "client": harness.host(),
        "server": {"export": args.export, "note": args.server_note},
        "mount": {"vers": args.vers, "fstype": mount_facts["fstype"], "opts": mount_options(mount_facts["opts"])},
        "tree": {"name": t.name, "scale": t.scale, "expected_entries": t.entries, **tree_counts},
        "harness": {"commit": harness.git_commit(), "python": sys.version.split()[0], "runs": args.runs,
                    "warmup": "one du run on its own mount, not recorded",
                    "fresh_mount_per_run": True},
        "commands": plan,
    }
    if tree_counts["entries"] != t.entries:
        say(f"the export holds {tree_counts['entries']:,} entries, not T1 at 0.25's {t.entries:,}")
    result["decision"] = judge(result)
    text = json.dumps(result, indent=2) + "\n"
    if args.out:
        with open(args.out, "w", encoding="utf-8") as f:
            f.write(text)
        say(f"wrote {args.out}: the gate's answer is {result['decision']['answer']}")
    else:
        sys.stdout.write(text)
    return 0


def cmd_tree(args) -> int:
    t = trees.tree(*TREE)
    stamp = trees.build(t, args.dir, jobs=args.jobs, log=say)
    say(f"export {trees.tree_root(t, args.dir)} ({t.dirs:,} directories, {t.files:,} files, digest {stamp['digest']})")
    return 0


def cmd_judge(args) -> int:
    with open(args.file, encoding="utf-8") as f:
        result = json.load(f)
    decision = judge(result)
    print(json.dumps(decision, indent=2))
    recorded = result.get("decision", {}).get("answer")
    if recorded != decision["answer"]:
        say(f"the file records {recorded!r}; its counts give {decision['answer']!r}")
        return 1
    return 0


def cmd_counts(args) -> int:
    with open(args.mountstats, encoding="utf-8") as f:
        print(json.dumps(counts(f.read(), args.mountpoint), indent=2))
    return 0


def parser() -> argparse.ArgumentParser:
    top = argparse.ArgumentParser(prog="nfs.py", description=__doc__.split("\n\n")[0])
    sub = top.add_subparsers(dest="subcommand", required=True)
    tree = sub.add_parser("tree", help="build the 100k-file tree under DIR, for the server to export")
    tree.add_argument("dir")
    tree.add_argument("--jobs", type=int, default=os.cpu_count() or 1)
    run = sub.add_parser("run", help="(root) mount the export and measure the three commands")
    run.add_argument("--export", required=True, help="HOST:PATH of the tree's root")
    run.add_argument("--mountpoint", required=True)
    run.add_argument("--duscope", required=True, help="the duscope binary")
    run.add_argument("--du", default="du", help="GNU du (default: du from PATH)")
    run.add_argument("--vers", default="4.2", help="NFS version (default 4.2; 3 has READDIRPLUS)")
    run.add_argument("--runs", type=int, default=3)
    run.add_argument("--server-note", default=None, help="the server's machine, for the result file")
    run.add_argument("--out", help="the result file (default: stdout)")
    judged = sub.add_parser("judge", help="the decision gate's answer from a result file")
    judged.add_argument("file")
    counted = sub.add_parser("counts", help="one mount's per-operation counts from a mountstats file")
    counted.add_argument("mountstats")
    counted.add_argument("mountpoint")
    return top


def main(argv: Optional[List[str]] = None) -> int:
    args = parser().parse_args(argv)
    try:
        return {"run": cmd_run, "tree": cmd_tree, "judge": cmd_judge, "counts": cmd_counts}[args.subcommand](args)
    except (NfsError, trees.TreeError, OSError) as e:
        say(str(e))
        return 1


if __name__ == "__main__":
    sys.exit(main())
