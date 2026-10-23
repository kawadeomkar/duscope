# Benchmark harness

The harness times disk-usage tools with [hyperfine](https://github.com/sharkdp/hyperfine) on
three seeded trees and writes one JSON result file per run. `just bench` runs it on developer
machines; the nightly job (`nightly-bench.yml`, on the `bench-macos` and `bench-linux`
self-hosted runners) runs the full matrix, and from phase 1 the `bench-regression` CI job runs it
on T1. Until duscope can scan, the subject every competitor is measured against is `du`.

## Running it

```sh
just bench                      # T1, du against diskus, warm caches, 10 runs each
just bench --trees all --competitors all --mode both    # the nightly matrix
bench/run.sh show bench/results/2026-10-23-macos.json   # a result file as a table
```

`just bench ARGS` is `bench/run.sh ARGS`; `just` splits its arguments at spaces, so an argument
that holds one (`--command 'x=find {tree}'`) goes to `bench/run.sh` directly. On first use the harness builds the trees it needs
under `bench/.trees/` and installs the competitors it needs under `bench/.tools/`; later runs
reuse both. It prints the result file's path on stdout and its progress on stderr, and exits 1
when a requested tool could not be measured (the result file is still written and says why).

Requirements: Python 3.9 or later (standard library only; `/usr/bin/python3` is preferred, and
`PYTHON` overrides it), hyperfine 2.x (`just setup` installs the pinned 2.0.0), `curl` and `tar`
for release binaries, and rustup with the stable toolchain for the competitors built from source.

| Subcommand | What it does |
| --- | --- |
| `run` (the default) | build the trees, install the tools, measure, write the result file |
| `build` | build the trees only (`--force` rebuilds) |
| `install` | install the pinned tools only (default: all of them) |
| `verify` | walk built trees and compare them with their stamps |
| `tools` | list the pinned tools and how each installs on each platform |
| `show FILE` | print a result file as a table |

Options of `run` (most also apply to `build`, `install` and `verify`):

| Option | Default | Meaning |
| --- | --- | --- |
| `--trees LIST` | `T1` | `T1`, `T2`, `T3`, a comma-separated list, or `all` |
| `--scale F` | `1` | build and measure the trees scaled by F (0 < F ≤ 1), for smoke runs |
| `--subject NAME` | `du` | the tool every competitor is paired with |
| `--competitors LIST` | `diskus` | pinned tools to pair with the subject, `all` or `none` |
| `--command NAME=CMD` | | also pair an ad-hoc command line with the subject; `{tree}` is the tree (repeatable) |
| `--tool-path NAME=PATH` | | use this executable for a pinned tool instead of installing it; its version is still checked (repeatable) |
| `--mode warm\|cold\|both` | `warm` | page cache warm, dropped before every run, or both |
| `--cold-cmd CMD` | `bench/drop-caches.sh` | the command that drops the caches before each cold run |
| `--runs N` | `10` | measured runs of each tool in each pair |
| `--warmup N` | `2` | unmeasured runs of each tool at the start of each warm pair |
| `--jobs N` | `4` | processes that create the trees' files |
| `--trees-dir DIR` | `bench/.trees` | where the trees and their stamps live |
| `--tools-dir DIR` | `bench/.tools` | where the pinned tools install |
| `--results-dir DIR` | `bench/results` | where the result file goes |
| `--out FILE` | `<results-dir>/<date>-<platform>.json` | the result file |
| `--hyperfine CMD` | `hyperfine` | the hyperfine command line |
| `--keep-raw` | off | keep hyperfine's per-round JSON exports under `bench/.runs/` |

A self-hosted runner keeps `--trees-dir` and `--tools-dir` outside the checkout, so that a clean
checkout does not delete them.

## The trees

| Tree | Shape | Directories | Files | Seed | Build (M5 Pro, APFS) | On disk (APFS) |
| --- | --- | --- | --- | --- | --- | --- |
| `T1` | gdu-shaped: a random recursive tree, sparse files | 100,000 | 400,000 | `duscope-bench-T1-v1` | 26 s | 162 MiB (56 GB apparent) |
| `T2` | fdu-shaped: fdu 0.5.0's `balanced` corpus at 1M entries | 125,000 | 875,000 | `fdu-balanced-v1` | 55 s | 3.2 GiB |
| `T3` | flat: one directory of empty files | 0 | 1,000,000 | `duscope-bench-T3-v1` | 45 s | 229 MiB |

Every name, size, content byte and timestamp is a function of the seed: each choice is SHA-256
over the seed, a namespace and an index, so every platform and every Python builds the same
tree. Every mtime (and atime) is 2023-11-14T22:13:20Z plus an even number of seconds below
20,000.

- **T1** has the counts of the tree in gdu's README benchmark (100k directories, 400k files).
  Each directory's parent is chosen uniformly among the directories before it, which gives real
  trees' skewed fan-out and a depth around ln n (at most about e ln n, some 30 levels); each
  file lands in a uniformly chosen directory, with a name, one of 16 common extensions, and a
  size from fdu's "mixed" profile (0 B to 1 MiB). The files are sparse: 56 GB apparent, close to
  gdu's 50 GB, with almost nothing allocated. The harness refuses to build T1 on a filesystem
  that would write the holes out.
- **T2** is the tree fdu's published comparison measures: its generator's `balanced` recipe
  (`explorations/benchmarks` in fdu 0.5.0) with fdu's own seed. One directory per eight
  entries, each directory's parent is directory i / 8 (breadth-first, fan-out 8), files land in
  uniformly chosen directories with fdu's names, extensions, sizes (0 B to 4 KiB, real bytes)
  and times. At 2,000 and 20,000 entries it is byte-identical to fdu's generator (paths, sizes,
  mtimes, contents); its directories are `d-<index>-<token>` and files `f-<index>-<token><ext>`
  rather than names from a real home directory.
- **T3** is the flat stress case: 1,000,000 empty files `f0000000` … `f0999999` in its root.

A build writes the tree to `<trees-dir>/<name>` and then a stamp, `<trees-dir>/<name>.json`,
with the spec, the build time and a digest. A run reuses a tree whose stamp matches its spec and
the generator version, and rebuilds anything else. The digest is the sum, modulo 2^128, of the
first 16 bytes of SHA-256 of one line `<kind>\t<size>\t<mtime ns>\t<path>` per entry, so
`bench/run.sh verify` can recompute it from a walk and catch a tree that changed since it was
built. `--scale` builds `T1-x0.001` and the like beside the full trees, with every count scaled.
APFS creates files one at a time however many processes ask, so `--jobs` helps on Linux more
than on macOS; deleting T2 and T3 there takes minutes too (`rm -rf bench/.trees/T2`).

## The tools

| Tool | Pinned | Command | Installed from |
| --- | --- | --- | --- |
| `du` | system | `du -sk {tree}` | `/usr/bin/du`: BSD du on macOS, GNU coreutils on Linux |
| `diskus` | `0.9.0` | `diskus {tree}` | release binary, all four platforms |
| `dua` | `2.46.0` | `dua {tree}` | release binary, all four platforms |
| `pdu` | `0.24.0` | `pdu {tree}` | release binary on x86_64; crates.io build on arm64 |
| `dust` | `1.2.6` | `dust -d0 {tree}` | release binary, all four platforms |
| `gdu` | `5.38.0` | `gdu -npc {tree}` | release binary, all four platforms |
| `ncdu` | `2.9.2` | `ncdu -0 -o /dev/null {tree}` | skipped: no prebuilt 2.9.2; pass `--tool-path` |
| `fdu` | `0.5.0` | `fdu {tree}` | crates.io build |
| `dumac` | `1ffbe3c38d1066c45cac9b4ec1e31eb74edc1076` | `dumac {tree}` | build of that commit, macOS only |

The versions are the ones plan/PROPOSAL.md's 1.0 acceptance targets pin. Release archives are
checked against the SHA-256 digests in `tools.py` before anything is unpacked; crates.io builds
run `cargo install --locked` on the stable toolchain, and cargo checks the crates' checksums.
Nothing installs outside `--tools-dir`, and the download requests carry no identity beyond the
user agent `duscope-bench`. The commands are those of gdu's README benchmark where the tool
appears there, and otherwise the plain invocation; every tool runs at its default thread count.
A tool that cannot run on a platform (dumac on Linux) or cannot be installed by the script
(ncdu 2.9.2) is listed in the result file as skipped, with the reason.

`bench/run.sh tools` prints the same table per platform; a test keeps the two in step.

## How a run measures

Each competitor is measured in a pair with the subject. A pair is `--runs` rounds; each round is
one hyperfine invocation that runs each of the two commands once, the subject first on even
rounds and second on odd ones, so slow drift and order effects fall on both alike. Warm pairs
start with `--warmup` unmeasured runs of each command; before any pair, each tool runs once on
the tree so that a failing tool is named before hyperfine starts. With no competitor, the
subject runs alone for `--runs` rounds.

hyperfine runs the commands without a shell, sends their output to `/dev/null`, and records wall
clock, user and system CPU time and peak RSS for every run (`--metrics
time_wall_clock,time_user,time_system,memory_peak_resident`). p50 and p95 interpolate linearly
between the closest ranks, as NumPy does by default: with ten runs, p95 lies 55% of the way from
the ninth fastest to the slowest run.

### Cold runs

`--mode cold` (or `both`) passes `bench/drop-caches.sh` to hyperfine's `--prepare`, so the
caches are dropped before every measured run: `sync`, then `purge` on macOS or `echo 3 >
/proc/sys/vm/drop_caches` on Linux. That needs root or passwordless sudo for exactly these
commands, which the runner user gets in a sudoers file:

```text
runner ALL=(root) NOPASSWD: /usr/sbin/purge
runner ALL=(root) NOPASSWD: /usr/sbin/sysctl -q vm.drop_caches=3
```

The harness runs the command once before building or measuring anything, and stops with an
error that says what to configure when it fails, so a cold run never silently measures warm
caches. `purge` drops the unified buffer cache, not every APFS metadata cache; a volume remount
clears more, and `--cold-cmd` takes any command that does it, such as a script that unmounts and
mounts the volume holding `--trees-dir`.

## Result file

A run writes `<results-dir>/<date>-<platform>.json`, where `<date>` is the UTC date the run
started and `<platform>` is `macos` or `linux`; a second run on the same day replaces it. These
files are not committed to `main`: `bench/.gitignore` ignores them, and the nightly job pushes
its own to the `bench-results` branch, where `compare.py` reads baselines. Results a package
means to keep (`thread-sweep-*.json`, `results/1.0/`) have other names and are committed.

Every field is always present; a field that does not apply is `null`. Times are seconds.

| Field | Type | Meaning |
| --- | --- | --- |
| `schema_version` | integer | `1`; it grows when a field changes meaning or goes away, never when one is added |
| `date` | string | the UTC date the run started, `YYYY-MM-DD` |
| `started_at` | string | when the run started, RFC 3339 UTC |
| `finished_at` | string | when the last measurement ended, RFC 3339 UTC |
| `platform` | string | `macos` or `linux` |
| `subject` | string | the tool every competitor was paired with |
| `host` | object | the machine |
| `host.os` | string | the OS release, such as `macOS 26.0 (25A354)` or `Ubuntu 24.04.3 LTS` |
| `host.kernel` | string | `uname -sr` |
| `host.arch` | string | `uname -m` |
| `host.cpu` | string or null | the CPU model |
| `host.logical_cpus` | integer | logical CPUs |
| `host.memory_bytes` | integer or null | physical memory |
| `harness` | object | how the run measured |
| `harness.commit` | string or null | the commit of the checkout |
| `harness.hyperfine` | string | the hyperfine version |
| `harness.python` | string | the Python version |
| `harness.runs` | integer | `--runs` |
| `harness.warmup` | integer | `--warmup` |
| `harness.cold_command` | string or null | the cache-drop command, when the run had cold runs |
| `trees` | array of object | the trees measured |
| `trees[].name` | string | `T1`, `T2` or `T3` |
| `trees[].shape` | string | `gdu`, `fdu-balanced` or `flat` |
| `trees[].seed` | string | the seed |
| `trees[].scale` | number | `1` for the full tree, below 1 for a `--scale` run |
| `trees[].dirs` | integer | directories below the root |
| `trees[].files` | integer | regular files |
| `trees[].apparent_bytes` | integer | the sum of the files' sizes |
| `trees[].digest` | string | the build's digest: equal digests mean identical trees |
| `trees[].filesystem` | string or null | the filesystem type holding the tree, such as `apfs` or `ext4` |
| `trees[].build_s` | number | how long the build took |
| `tools` | array of object | every tool the run was asked for, measured or not |
| `tools[].name` | string | the tool |
| `tools[].pinned` | string or null | the pinned version (a commit for dumac); null for `du` and `--command` tools |
| `tools[].version` | string or null | what the tool reports, from `--version` |
| `tools[].source` | string | `system`, `release`, `cargo`, `git`, `path` (`--tool-path`), `command` (`--command`) or `skipped` |
| `tools[].path` | string or null | the executable |
| `tools[].status` | string | `measured`, `skipped` (not for this platform) or `failed` |
| `tools[].reason` | string or null | why it was skipped or failed |
| `results` | array of object | one entry per tree, mode and measured tool, the subject first |
| `results[].tree` | string | the tree |
| `results[].mode` | string | `warm` or `cold` |
| `results[].tool` | string | the tool |
| `results[].command` | string | the command line, `{tree}` standing for the tree's path |
| `results[].paired_with` | string or null | the subject this tool was paired with; null in the subject's own entry |
| `results[].run_count` | integer | measured runs; the subject's entry holds its runs from every pair |
| `results[].times_s` | array of number | wall-clock time of each run, in the order they ran |
| `results[].p50_s` | number | median wall-clock time |
| `results[].p95_s` | number | 95th percentile wall-clock time |
| `results[].mean_s` | number | mean wall-clock time |
| `results[].stddev_s` | number or null | sample standard deviation; null for a single run |
| `results[].min_s` | number | fastest run |
| `results[].max_s` | number | slowest run |
| `results[].user_s` | number or null | mean user CPU time |
| `results[].system_s` | number or null | mean system CPU time |
| `results[].peak_rss_bytes` | integer or null | the largest peak resident set size of any run |
| `results[].subject_times_s` | array of number or null | the subject's runs in this tool's pair, round by round; null in the subject's entry |
| `results[].subject_ratio_p50` | number or null | the median over the pair's rounds of subject time divided by this tool's time: above 1, the subject is slower |

## Reading results

`bench/run.sh show FILE` prints each tree and mode with every tool's p50 and p95 and its
`subject / tool` ratio. Compare p50s only between files whose `trees[].digest`, `host` and
`harness.hyperfine` agree: a different tree, machine or hyperfine is a different experiment.
Within a file, `subject_ratio_p50` is the fairer comparison than the ratio of two p50s, because
each of its ratios comes from runs a moment apart. The phase gates are judged on the nightly
runners' cold and warm p50s, never on shared CI runners.

## Files

| File | Purpose |
| --- | --- |
| `run.sh` | the entry point `just bench` calls; picks the Python |
| `harness.py` | the command line, host facts and the result file |
| `trees.py` | the tree generator and the digest |
| `tools.py` | the pinned tools and their installation |
| `measure.py` | hyperfine in interleaved pairs, and the statistics |
| `drop-caches.sh` | the cold-run cache drop |
| `test_bench.py` | unit tests, run by `duscope/tests/bench_harness.rs` |
| `testdata/` | a real hyperfine 2.0.0 export and the hyperfine stand-in the tests use |
