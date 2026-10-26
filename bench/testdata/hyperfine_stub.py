"""A stand-in for hyperfine 2.0 in the harness's tests.

CI's test job does not install hyperfine, so duscope/tests/bench_harness.rs
passes `--hyperfine "<python> bench/testdata/hyperfine_stub.py"`. The stub takes
the options the harness passes and no others, runs each command the way
hyperfine 2.0 does without a shell, and writes the subset of hyperfine 2.0's
schema_version 2 export that the harness reads (test_bench.py checks that the
subset matches the real export in hyperfine-2.0.0-export.json).

When BENCH_STUB_LOG names a file, the stub appends one line per event to it:
`prepare <command>`, `warmup <name>` or `run <name>`, so tests can check the
order of the runs.
"""

from __future__ import annotations

import argparse
import json
import os
import shlex
import subprocess
import sys
import time


def log(event: str) -> None:
    path = os.environ.get("BENCH_STUB_LOG")
    if path:
        with open(path, "a", encoding="utf-8") as f:
            f.write(event + "\n")


def execute(argv):
    started = time.perf_counter()
    process = subprocess.Popen(argv, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    _, status, usage = os.wait4(process.pid, 0)
    wall = time.perf_counter() - started
    process.returncode = os.waitstatus_to_exitcode(status)
    # ru_maxrss is bytes on macOS and KiB on Linux.
    rss = usage.ru_maxrss if sys.platform == "darwin" else usage.ru_maxrss * 1024
    return process.returncode, wall, usage.ru_utime, usage.ru_stime, rss


def main() -> int:
    if sys.argv[1:] == ["--version"]:
        print("hyperfine 2.0.0")
        return 0
    p = argparse.ArgumentParser(prog="hyperfine-stub", allow_abbrev=False)
    # The harness always passes -N; the stub runs commands without a shell either way.
    p.add_argument("-N", dest="no_shell", action="store_true", required=True)
    p.add_argument("--runs", type=int, required=True)
    p.add_argument("--warmup", type=int, default=0)
    p.add_argument("--style", choices=("none",), required=True)
    p.add_argument("--metrics", required=True)
    p.add_argument("--export-json", required=True)
    p.add_argument("--prepare")
    p.add_argument("--command-name", action="append", default=[])
    p.add_argument("commands", nargs="+")
    args = p.parse_intermixed_args()
    if len(args.command_name) != len(args.commands):
        p.error("one --command-name per command")

    def prepare() -> None:
        if args.prepare:
            log(f"prepare {args.prepare}")
            if subprocess.run(args.prepare, shell=True).returncode != 0:
                sys.exit("Error: the preparation command terminated with a non-zero exit code")

    results = []
    for name, command in zip(args.command_name, args.commands):
        argv = shlex.split(command)
        for _ in range(args.warmup):
            prepare()
            log(f"warmup {name}")
            execute(argv)
        measurements = []
        for _ in range(args.runs):
            prepare()
            log(f"run {name}")
            code, wall, user, system, rss = execute(argv)
            if code != 0:
                print(f"Error: Command terminated with non-zero exit code {code}", file=sys.stderr)
                return 1
            measurements.append(
                {
                    "time_wall_clock": {"value": wall, "unit": "second"},
                    "time_user": {"value": user, "unit": "second"},
                    "time_system": {"value": system, "unit": "second"},
                    "memory_peak_resident": {"value": float(rss), "unit": "byte"},
                    "exit_code": code,
                }
            )
        results.append({"command": command, "name": name, "measurements": measurements})
    export = {
        "schema_version": 2,
        "metadata": {"hyperfine_version": "2.0.0"},
        "primary_metric": "time_wall_clock",
        "results": results,
    }
    with open(args.export_json, "w", encoding="utf-8") as f:
        json.dump(export, f, indent=2)
    return 0


if __name__ == "__main__":
    sys.exit(main())
