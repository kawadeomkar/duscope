# Benchmark harness

The harness times disk-usage tools with [hyperfine](https://github.com/sharkdp/hyperfine) on
three seeded trees and writes one JSON result file per run. `just bench` runs it on developer
machines; the nightly job (`nightly-bench.yml`, on the `bench-macos` and `bench-linux`
self-hosted runners) runs the full matrix, and the `bench-regression` CI job runs it on T1. Both
measure duscope, built from the checkout, as the subject every competitor is paired with; `du`
is one of the competitors. Without `--subject`, the subject is `du`.

## Running it

```sh
just bench                      # T1, du against diskus, warm caches, 10 runs each
cargo build --release -p duscope
just bench --subject duscope --tool-path duscope=target/release/duscope   # duscope against diskus
just bench --subject duscope --tool-path duscope=target/release/duscope --trees all --competitors all --mode both
                                # the nightly matrix
bench/run.sh sweep --duscope target/release/duscope    # the thread sweep
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
| `sweep` | measure duscope at each thread count and apply the defaults rule ([The thread sweep](#the-thread-sweep)) |

Options of `run` (most also apply to `build`, `install` and `verify`):

| Option | Default | Meaning |
| --- | --- | --- |
| `--trees LIST` | `T1` | `T1`, `T2`, `T3`, a comma-separated list, or `all` |
| `--scale F` | `1` | build and measure the trees scaled by F (0 < F ≤ 1), for smoke runs |
| `--subject NAME` | `du` | the tool every competitor is paired with |
| `--competitors LIST` | `diskus` | pinned tools to pair with the subject, `all` or `none` |
| `--command NAME=CMD` | | also pair an ad-hoc command line with the subject; `{tree}` is the tree (repeatable) |
| `--tool-path NAME=PATH` | | use this executable for a pinned tool instead of installing it; its `--version` must name the pin, except dumac's, which has no version to check and is recorded at its pinned commit (repeatable) |
| `--mode warm\|cold\|both` | `warm` | page cache warm, dropped before every run, or both |
| `--cold-cmd CMD` | `bench/drop-caches.sh` | the command that drops the caches before each cold run; it may not be empty |
| `--cold-trees-dir DIR` | `--trees-dir` | where the trees the cold runs measure live, built there as in `--trees-dir` ([Cold runs](#cold-runs)) |
| `--cold-competitors LIST` | `--competitors` | the pinned tools paired with the subject in cold runs, `all` or `none`; `--command` tools run in both modes |
| `--runs N` | `10` | measured runs of each tool in each pair |
| `--warmup N` | `2` | unmeasured runs of each tool at the start of each warm pair |
| `--jobs N` | `4` | processes that create the trees' files |
| `--trees-dir DIR` | `bench/.trees` | where the trees and their stamps live; a new or empty directory, or one the harness built trees in |
| `--tools-dir DIR` | `bench/.tools` | where the pinned tools install |
| `--results-dir DIR` | `bench/results` | where the result file goes |
| `--out FILE` | `<results-dir>/<date>-<platform>.json` | the result file |
| `--hyperfine CMD` | `hyperfine` | the hyperfine command line |
| `--keep-raw` | off | keep hyperfine's per-round JSON exports under `bench/.runs/`; without it they go to a temporary directory, removed however the run ends |

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
  Each directory's parent is chosen uniformly among the directories before it (a random
  recursive tree): fan-out is uneven but small, at most 19 subdirectories and 14 files in any
  directory, half the directories are leaves, and the depth stays around ln n (26 levels at
  most). It has no large directory; T3 is the large-directory case. Each file lands in a
  uniformly chosen directory, with a name, one of 16 common extensions, and a
  size from fdu's "mixed" profile (0 B to 1 MiB). The files are sparse: 56 GB apparent, close to
  gdu's 50 GB, with almost nothing allocated. The harness refuses to build T1 on a filesystem
  that would write the holes out.
- **T2** is the tree fdu's published comparison measures: its generator's `balanced` recipe
  (`explorations/benchmarks` in fdu 0.5.0) with fdu's own seed. One directory per eight
  entries, each directory's parent is directory i / 8 (breadth-first, fan-out 8), files land in
  uniformly chosen directories with fdu's names, extensions, sizes (0 B to 4 KiB, real bytes)
  and times. It was checked against fdu's generator at 2,000 and 20,000 entries (paths, sizes,
  mtimes, contents); the unit tests pin a few of its directory names and mtimes, not the whole
  listing. Its directories are `d-<index>-<token>` and files `f-<index>-<token><ext>`, not names
  from a real home directory: it is fdu's shape, not a home-like tree.
- **T3** is the flat stress case: 1,000,000 empty files `f0000000` … `f0999999` in its root.

A build writes the tree to `<trees-dir>/<name>` and then a stamp, `<trees-dir>/<name>.json`,
with the spec, the build time and a digest. The digest is the sum, modulo 2^128, of the first 16
bytes of SHA-256 of one line `<kind>\t<size>\t<mtime ns>\t<path>` per entry, so a walk can
recompute it. A run reuses a tree only when its stamp matches its spec and the generator version
and a walk of the tree still gives the stamp's digest (seconds per million entries), and
rebuilds anything else: a tree changed or half-deleted since its build is never measured under
its old digest. `bench/run.sh verify` runs the same walk on its own. The harness marks the trees
directory with `.duscope-bench-trees` and refuses to build in a directory that holds other
files, since a build deletes the trees it replaces. `--scale` builds `T1-x0.001` and the like beside the full trees, with every count scaled.
APFS creates files one at a time however many processes ask, so `--jobs` helps on Linux more
than on macOS; deleting T2 and T3 there takes minutes too (`rm -rf bench/.trees/T2`).

## The tools

| Tool | Pinned | Command | Installed from |
| --- | --- | --- | --- |
| `du` | system | `du -sk {tree}` | `/usr/bin/du`: BSD du on macOS, GNU coreutils on Linux |
| `duscope` | this checkout | `duscope scan --json --treemap never {tree}` | never installed: built from the checkout and passed with `--tool-path duscope=target/release/duscope` |
| `diskus` | `0.9.0` | `diskus {tree}` | release binary, all four platforms |
| `dua` | `2.46.0` | `dua {tree}` | release binary, all four platforms |
| `pdu` | `0.24.0` | `pdu {tree}` | release binary on x86_64; crates.io build on arm64 |
| `dust` | `1.2.6` | `dust -d0 {tree}` | release binary, all four platforms |
| `gdu` | `5.38.0` | `gdu -npc {tree}` | release binary, all four platforms |
| `ncdu` | `2.9.1` | `ncdu -0 -o /dev/null {tree}` | static release binary on Linux; skipped on macOS (no prebuilt binary): pass `--tool-path` |
| `fdu` | `0.5.0` | `fdu {tree}` | crates.io build |
| `dumac` | `1ffbe3c38d1066c45cac9b4ec1e31eb74edc1076` | `dumac {tree}` | build of that commit, macOS only |

The versions are the ones plan/PROPOSAL.md's 1.0 acceptance targets pin, except ncdu's: 2.9.2 has
no prebuilt binary, so the harness pins 2.9.1, the last release with static Linux builds, from
ncdu's own site. Release archives are
checked against the SHA-256 digests in `tools.py` before anything is unpacked; crates.io builds
run `cargo install --locked` on the stable toolchain, and cargo checks the crates' checksums.
Nothing installs outside `--tools-dir`, and the download requests carry no identity beyond the
user agent `duscope-bench`. The commands are those of gdu's README benchmark where the tool
appears there, and otherwise the plain invocation; every tool runs at its default thread count.
A tool that cannot run on a platform (dumac on Linux) or has no build the script can install there
(ncdu on macOS) is listed in the result file as skipped, with the reason.

`bench/run.sh tools` prints the same table per platform; a test keeps the two in step.

## How a run measures

Each competitor is measured in a pair with the subject. A pair is `--runs` rounds; each round is
one hyperfine invocation that runs each of the two commands once, the subject first on even
rounds and second on odd ones, so slow drift and order effects fall on both alike. Warm pairs
start with `--warmup` unmeasured runs of each command; before any pair, each tool runs once on
the tree so that a failing tool is named before hyperfine starts. With no competitor, the
subject runs alone for `--runs` rounds.

hyperfine runs the commands without a shell (`-N`; the cold-run command it runs before each cold
run goes through one), sends their output to `/dev/null`, and records wall clock, user and system
CPU time and peak RSS for every run (`--metrics
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
error that says what to configure when it fails (or when `--cold-cmd` is empty), so a cold run
never silently measures warm caches.

`purge` drops the unified buffer cache, not APFS's metadata caches: with `purge` alone, `du` on T1
took 2.94 s cold against 2.85 s warm on the Mac. So the macOS bench runner keeps its trees on an
APFS disk image (a sparse bundle it owns) and passes `--cold-cmd 'bench/drop-caches.sh --remount
IMAGE MOUNTPOINT'`, which detaches the image when it is attached, purges, and attaches it again.
Detaching drops the volume's metadata caches; the purge in between drops the image's own blocks
from the cache of the volume it is stored on. `hdiutil` needs no root for an image its user owns.
Both tools of a pair read through the image alike, so the ratios the gates use compare like with
like. `--cold-cmd` takes any other command too.

The image is slow even warm, so the runner measures only the cold runs on it. macOS keeps at most
`kern.maxvnodes` vnodes (263,168 on the bench Mac), fewer than T1's 500,000 entries, so a warm run
looks up much of the tree's metadata again, which on the image is slow. `du` on T1 took 2.85 s
warm on the internal disk and 13 s warm on the image, where every
competitor took 7 to 10 s, and T2's runs took a minute each. The runner passes
`--cold-trees-dir` (the image) and keeps `--trees-dir` on the internal disk, so warm runs measure
the disk a user scans. It also passes `--cold-competitors dua,du`: dua is the competitor gate 1's
macOS cold target names and `du` is every result's reference, and pairing all eight tools cold
on the image would take more than the job's 300 minutes.

## Result file

A run writes `<results-dir>/<date>-<platform>.json`, where `<date>` is the UTC date the run
started and `<platform>` is `macos` or `linux`; a second run on the same day replaces it. The
platform names the OS only: `host.arch` tells an arm64 run from an x86_64 one, and anything that
compares results (`compare.py`) keys on both. These
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
| `harness.cold_competitors` | array of string or null | `--cold-competitors`, when the run had cold runs and the option; null when cold runs paired the subject with `--competitors` |
| `trees` | array of object | the trees measured |
| `trees[].name` | string | `T1`, `T2` or `T3` |
| `trees[].shape` | string | `gdu`, `fdu-balanced` or `flat` |
| `trees[].seed` | string | the seed |
| `trees[].scale` | number | `1` for the full tree, below 1 for a `--scale` run |
| `trees[].dirs` | integer | directories below the root |
| `trees[].files` | integer | regular files |
| `trees[].apparent_bytes` | integer | the sum of the files' sizes |
| `trees[].digest` | string | the build's digest: equal digests mean identical trees |
| `trees[].filesystem` | string or null | the filesystem type holding the tree, such as `apfs` or `ext4`; the warm runs' copy when the run had both |
| `trees[].cold_filesystem` | string or null | the filesystem type holding the cold runs' copy, when `--cold-trees-dir` put it in a directory of its own; null otherwise |
| `trees[].build_s` | number | how long the build took |
| `tools` | array of object | every tool the run was asked for, measured or not |
| `tools[].name` | string | the tool |
| `tools[].pinned` | string or null | the pinned version (a commit for dumac); null for `du` and `--command` tools |
| `tools[].version` | string or null | what the tool reports, from `--version` |
| `tools[].source` | string | `system`, `release`, `cargo`, `git`, `path` (`--tool-path`), `command` (`--command`) or `skipped` |
| `tools[].path` | string or null | the executable |
| `tools[].built_with` | string or null | for a tool built from source (`cargo`, `git`), the `rustc --version` that built it |
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

## Attribute costs on macOS

`gab_bench.c` measures what each `getattrlistbulk` attribute set costs per entry, apart from
the rest of a scan (P1-BENCH-02). It walks the trees it is given single-threaded, warm, once
per mode, in a rotated order each round, and reports the median nanoseconds per entry:

| Mode | Attributes |
| --- | --- |
| `lean` | name, type and allocated size |
| `basic` | duscope's basic list (P1-WALK-03) |
| `clones` | `basic` plus `ATTR_CMNEXT_CLONEID` and `ATTR_CMNEXT_EXT_FLAGS` |
| `privatesize` | `clones` plus `ATTR_CMNEXT_PRIVATESIZE` |
| `fstatat` | `readdir` and `fstatat`, the reference |

```sh
clang -O2 -Wall -Wextra -o gab_bench bench/gab_bench.c
./gab_bench --reps 7 --json out.json bench/.trees/T1 /Applications
./gab_bench --make /tmp/gab_tree 60000 10    # 60,000 cp -c clones of one 64 KiB file
```

The decision (D23) is taken on duscope's own warm T1 scan, timed in alternating pairs, because
single-threaded medians on a busy machine swing by several percent between runs. The pairs come
from `crates/duscope-walk/examples/clone_pairs.rs`, which prints the `walker.clones` object:

```sh
cargo run --release -p duscope-walk --example clone_pairs -- bench/.trees/T1 40
```

`results/clone-attributes-macos.json` holds both measurements, a re-run of the clone pairs with
the committed driver (`walker_rerun`, +0.1%), and the decision:
- **Clone attributes.** They add 0.7% to the warm T1 scan, so `--clones` stays on by default.
- **`PRIVATESIZE`.** It adds 36%, so it is not read: it leaves `FullAttrs`.

## The thread sweep

`bench/run.sh sweep --duscope target/release/duscope` measures duscope alone at each thread count
(1 to 8, then 10, 12, 16, 20, 24, 32, 48 and 64; `--threads` takes another list) on each tree,
warm and cold by default, five runs each, and writes
`bench/results/thread-sweep-<platform>-<arch>.json`: the usual header, one row per tree, mode and
thread count (the result file's row fields plus `threads`), and `default`. On scaled trees
(`--scale` below 1) it is a smoke run: the file is `thread-sweep-<platform>-<arch>-x<scale>.json`
and `default` is null. `--trees-dir`, `--cold-trees-dir` and `--cold-cmd` mean what they mean in
`run`, so bench-macos sweeps the trees its harness reads: warm on the internal disk, cold on the
disk image ([Cold runs](#cold-runs)).

The default is set by a fixed rule (P1-BENCH-01): among the warm T1 rows, the smallest thread
count whose p50 is within 5% of the best p50, rounded to the nearest of 4, 8, 16 and 32, ties to
the smaller. The bench runners run the sweep on a dispatch of `nightly-bench.yml` with `sweep`
ticked, in place of the harness (so the day's nightly result stays as it was), which leaves each
runner's file as a `thread-sweep-<platform>` artifact. A full dispatch (three trees, warm and
cold, a cache drop before every cold run) may not fit the job's 300 minutes on bench-linux:
dispatch one tree at a time with `trees`, T1 first, since the rule reads only its warm rows.
Above 8 the sweep samples counts rather than taking every one; the rule rounds to 4, 8, 16 or 32,
so the gaps would not change its answer. The author
commits the files, each platform provider's `default_threads` takes the rule's count and cites
the file, and `duscope/tests/thread_sweep.rs` fails when a committed file and the default
disagree. `--threads` and `DUSCOPE_THREADS` still override.

## The allocator

P1-BENCH-03 timed a warm T2 scan with the platform's own allocator and with mimalloc, three
alternating runs each, and kept mimalloc where it gains at least 2% (musl) or 10% (macOS);
`results/allocator.json` holds the runs. On the musl build (aarch64, a Debian 12 container on
an Apple M5 Pro) mimalloc took the p50 from 3.04 s to 0.41 s, 86.6% faster, so the Linux
archives and `.deb`s allocate with it, built with arena eager commit and transparent huge pages
off (`.cargo/config.toml`), which keeps a scan's peak with `--attrs full` at 120–135 bytes per
entry on T1–T3, within the RSS budget, where mimalloc's defaults reached 168–244. On macOS
(arm64, APFS) it gained 1.7% (4.16 s to 4.09 s), so macOS keeps the system allocator.

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
| `compare.py` | the regression check: a result file against the results before it on the same machine kind and tree |
| `test_compare.py` | unit tests of `compare.py`, run by `duscope/tests/nightly_bench.rs` |
| `testdata/` | a real hyperfine 2.0.0 export and the hyperfine stand-in the tests use |
| `gab_bench.c` | the per-entry cost of `getattrlistbulk` attribute sets on macOS |
| `sweep.py` | the thread sweep and the rule that sets a platform's default thread count |
| `results/clone-attributes-macos.json` | P1-BENCH-02's measurement and the `--clones` and `PRIVATESIZE` decisions |
| `results/allocator.json` | P1-BENCH-03's measurement: mimalloc on the musl builds, the system allocator on macOS |
