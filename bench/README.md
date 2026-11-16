# Benchmark harness

The harness times disk-usage tools with [hyperfine](https://github.com/sharkdp/hyperfine) on
three seeded trees and writes one JSON result file per run. `just bench` runs it on developer
machines; the nightly job (`nightly-bench.yml`, on the `bench-linux` self-hosted runner, and
on `bench-macos` when it is not paused, [docs/runners.md](../docs/runners.md#paused)) runs the
full matrix, and the `bench-regression` CI job runs it on T1. Both
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
| `index` | measure a saved scan's write, queries and size, and the D34 comparison ([Index measurements](#index-measurements)) |
| `redraw` | measure the viewer's redraw at 120x40 on T2 ([The redraw budget](#the-redraw-budget)) |
| `acceptance` | the 1.0 acceptance run on this machine: the harness, the index measurements and the rescans, in one file ([The 1.0 acceptance run](#the-10-acceptance-run)) |
| `judge [FILE...]` | each goal-1 and goal-2 target against the 1.0 files, and their summary ([Judging it](#judging-it)) |

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
ticked (and `macos` for bench-macos while it is paused), in place of the harness (so the day's nightly result stays as it was), which leaves each
runner's file as a `thread-sweep-<platform>` artifact. A full dispatch (three trees, warm and
cold, a cache drop before every cold run) may not fit the job's 300 minutes on bench-linux:
dispatch one tree at a time with `trees`, T1 first, since the rule reads only its warm rows.
Above 8 the sweep samples counts rather than taking every one; the rule rounds to 4, 8, 16 or 32,
so the gaps would not change its answer. The author
commits the files, each platform provider's `default_threads` takes the rule's count and cites
the file, and `duscope/tests/thread_sweep.rs` fails when a committed file and the default
disagree. Linux is the exception (DEV-54): its count stays twice the logical CPUs, clamped to
4–32, since Linux machines span 2 to 64 cores, and bench-linux's file holds that count to the
rule's bound instead. On bench-linux's 10 CPUs it is 20 threads, 0.243 s on warm T1 against the
best 0.235 s at 12. The rule's fixed count, 8, was 13% slower. `--threads` and `DUSCOPE_THREADS`
still override.

## Index measurements

`bench/run.sh index --duscope target/release/duscope` measures what Gate 2 → 3 asks of a saved
scan (P2-BENCH-01, plan/PROPOSAL.md's 1.0 acceptance targets 3, 4, 6 and 7), on T2 unless
`--trees` names others, warm and cold by default, 20 runs of each measurement after two warm-up
runs:

- **The write:** `scan --save -v` prints the write's seconds and microseconds per entry. Warm runs
  give the cost per entry. Cold runs, with the caches dropped before each, also give the write's
  share of the scan: its time over the walk's and the roll-up's in the same run. Each index is
  deleted once read.
- **The queries:** each runs against the saved scan (`--scan <id>`) and is timed end to end, the
  process's start included, warm, for p50 and p95:
  - `top`;
  - `stale`;
  - `explain` of the scan's largest file;
  - `by-type` over the whole scan and over its largest directory.
- **The filtered shapes:** `top --files` and `stale` with the filters the index answers
  (P2-QRY-10), timed as the queries are: `--depth 3`; `--min-size 1M`, which keeps nothing on T2;
  `--size apparent`, alone and with `--min-size 1K`; `--since` a date that splits T2's
  modification times, which the generator sets; and `stale --min-size 1 --max-depth 4
  --older-than 1d`, since the change times are the tree's build and the default 180 days would
  keep nothing. Each is recorded per tree under `shapes` with the rows it matched
  (`total_count`), and holds no gate.
- **The first query after a cache drop:** `top --scan`, cold, against a scan saved beside the
  cold trees.
- **The index's size:** bytes per entry of a basic index and of an `--attrs full` one.
- **D34:** `top --scan` in interleaved pairs, as `run` pairs tools, with two competitors that
  load their whole snapshot:
  - `dua interactive --import --once`, reading the snapshot that `dua i --once --export` wrote.
    dua's interactive mode starts by asking the terminal where the cursor is and gives up when
    no answer comes, so both commands run on `bench/ptyrun.py`, a pseudo-terminal that answers
    it. Its start adds about 20 ms to each of dua's runs on bench-linux, as `script`'s did.
  - `fdu --stale-ok`, after `fdu --cache=on` kept a scan. fdu runs with `HOME` and
    `XDG_CACHE_HOME` in the run's work directory, so its scan is removed with it.

  Each competitor's speed-up is the median, over rounds, of its time over duscope's in the same
  round. `--competitors none` leaves this step out, and `--tool-path dua=…` or `fdu=…` and
  `--tools-dir` mean what they mean in `run`.

`--cold-trees-dir` gives the cold runs trees of their own, as in `run`. Each mode's saved scans
live in a temporary directory inside its trees directory, on the trees' filesystem, so a cold
run reads them cold as it reads the trees. bench-macos measures warm on the internal disk and
cold on its disk image, for the reason [Cold runs](#cold-runs) gives.

The result file is `<date>-<platform>-index.json` in `--results-dir` (`…-index-x<scale>.json`
on scaled trees). It holds:

- the usual header;
- the runs, as `results` rows with a `kind`: `query`, `first_query` or `d34`;
- for each tree, its two scans, `index_size`, `write` (the runs and their percentiles),
  `query_targets`, `shapes` (each shape's row, as `results` holds a query's, with `kind`
  `shape` and its `total_count`) and `speedups`;
- `gates`: each line of the targets with the value measured, its limit, the platform it binds,
  and whether it holds, read from T2 (`gates_tree`); without T2, `gates_tree` and the values
  are null and the run says each line is not judged.

The gates follow the targets:

- **The write:** at most 2.75 µs per entry warm, on Linux alone (DEV-107). A cold run's share of
  the scan, the write's time over the walk's and roll-up's, is in the result file and holds no
  line (DEV-108): the write starts after the output, which
  `p2_idx_02_the_write_starts_after_the_output` checks, and on a fast disk a cold walk costs less
  per entry than any write.
- **The queries:** `top`, `stale` and `explain` at p50 ≤ 20 ms and p95 ≤ 50 ms. `by-type` at
  p50 ≤ 50 ms over the whole scan, and over a subtree ≤ 50 ms per 100,000 of its files.
- **The first query after a cache drop:** p50 ≤ 250 ms.
- **The index's size:** at most 110 bytes per entry basic and 140 full.
- **D34:** `top --scan` at least 20 times faster than each competitor.

A part that cannot be measured is recorded in `failures` and the rest is still measured. Exit
status 1 means something could not be measured, not that a gate failed.

`nightly-bench.yml` runs it on each bench runner after the harness, and publishes the file to
`bench-results` beside the harness's.

## The redraw budget

`bench/run.sh redraw --duscope target/release/duscope` measures the two numbers Gate 4 → 5 asks
for (P4-TEST-01): how long the viewer takes to redraw at 120x40 on T2, from a saved scan. The viewer
runs on a pseudo-terminal of that size, in a true-colour terminal's environment; what it writes is
read and dropped, not parsed, so the terminal's own drawing is not in the time. One redraw is from
the write of a key (or the change of the terminal's size) to the last byte of the frame it caused,
the end of a frame being 50 ms of silence after its first byte. Bytes that only erase the screen
are not a frame: on a change of size ratatui writes and flushes the erase by itself, then lays the
frame out, and that silence is the work being timed. A key that changes nothing on the screen, a
move at a block's edge, writes no frame and is counted, not timed; `--frame-wait` (default 5 s) is
how long a press waits for a frame's first byte before it counts as one of those, longer than T2's
slowest frame, so that a slow frame is not read by the next press.

The kinds, each in a session of its own, `--rounds` cycles (default 30), and the class of their
frames:

| Kind | Keys | What it asks of the viewer | Class |
| --- | --- | --- | --- |
| `move` | `l j h k` on the map's top row | the cursor's outline and the panes | navigation |
| `descend` | `⌫ ⏎` from the file the viewer opens on | the panes and the breadcrumbs | navigation |
| `zoom` | `z Z l` from the map's top row | a folder's map the first time it is zoomed, kept after | recompute (first-time) the first time, then navigation |
| `repaint` | `c p` | the legend and every cell of the map again, each time | recompute |
| `focus` | `⇥` | the panes' borders | navigation |
| `views` | `2 1 3 1 4 1` | each table's rows the first time, kept after, and the map again | recompute (first-time) the first time, then navigation |
| `resize` | two sizes, 100x30 and 120x40 | every layout again, at another size | recompute |

On T3 (`--trees T3`), one folder of a million empty files, the map draws no tile and there is no
folder under the root, so its kinds are `move` (`⇥`, then `j k` in the siblings pane, along the
folder's whole list) and `page` (`⇥`, then `PgDn PgUp`), both navigation, and `resize`. The gates
are read from T2 whatever else is measured, and a run without T2 judges none.

**Why two classes.** A frame the viewer can draw from what it has kept is a frame of navigation,
and 16 ms is one frame at 60 Hz. A frame that works the whole map out, or builds a table, is a wait
for a piece of work: on a million entries it cannot fit a frame, and it happens rarely, so a
percentile over many presses would not see it (at 30 rounds, two first zooms among 61 frames lie
above the 95th percentile of their kind). Frame-budget benchmarks read steady frames as percentiles
and keep first and long frames apart, as a count or a maximum over a threshold: Android tracks
initial and screen-change frames separately from slow rendering ("No frames in your app should
ever take longer than 700ms to render"), the browser reports each long task, and Interaction to
Next Paint reports nearly a page's worst interaction.

**The two lines.** `gates`, read from T2 (`gates_tree`); without T2, `gates_tree` and the values
are null and the run says each line is not judged (a gate failing is no failure of the run):

- `redraw_p95_ms_at_120x40 <= 16`: the largest p95 of the navigation rows.
- `redraw_recompute_max_ms <= 1000`: the slowest frame of the recompute rows, the first-time
  frames and both sizes of `resize` among them. 1 s is the published limit for a response that
  keeps the user's flow of thought with no sign of progress (Nielsen's 0.1 s / 1 s / 10 s, after
  Miller 1968 and Card, Robertson and Mackinlay 1991; RAIL has users losing focus beyond it), and
  the viewer shows no sign of progress while it works. It is a design limit like the query budgets,
  not a figure taken from a run; it can be tightened from the nightly's numbers as the size and
  build budgets were.

The result file (`<date>-<platform>-redraw.json` in `--results-dir`, or `--out`) holds the usual
header, `trees` (each tree's two figures, the frames over each limit, and how many first-time frames
there were and the slowest), `gates`, one row per tree, kind and class (`limit_ms`, `samples`,
`no_frame`, `first`, `over_limit`, p50, p95, mean, min, max and the times in milliseconds, the key
of each time, the mean bytes of a frame), and `sessions`, each press of each kind in order as `[key,
ms, bytes, first]`, `ms` being null for one that drew nothing, so that a later classification can
be worked out from the file. A part that cannot be measured is recorded in `failures` and exit
status 1 means something could not be measured.

`nightly-bench.yml` runs it on T2 and T3 on each bench runner after the index measurements and
publishes the file to `bench-results` beside the others. `--note` keeps free text in the result
file; `--tree-path` measures a tree that is already there, read-only and neither built nor verified.
`results/redraw-macos-arm64-provisional.json` is one such run, on an Apple M5 Pro against a
read-only `bench/.trees/T2`, kept until the first nightly number exists; it is provisional and
macOS-local, and it is not the nightly result. It was measured before the frames were split and is
reclassified from its own samples: its `zoom` and `resize` rows could not be, keep their old
figures, marked `unclassified`, and are in neither line's value (its `reclassified` field says why).

## The 1.0 acceptance run

P5-BENCH-01 publishes the numbers 1.0 is judged on in `results/1.0/`, one file per platform, from
a quiet machine of each: a dedicated Mac with the `bench-macos` layout (DEV-52), and a Linux
machine off the Proxmox host whose memory is suspect (DEV-157). `bench/run.sh acceptance` is the
whole run on one of them:

```sh
# macOS, as bench-macos keeps its trees: warm on the internal disk, cold on the disk image
cargo build --release --locked -p duscope
home=~/duscope-bench
cold="$PWD/bench/drop-caches.sh --remount $home/trees.sparsebundle $home/trees-volume"
caffeinate -is bench/run.sh acceptance --platform macos --duscope target/release/duscope \
  --trees-dir "$home/trees" --cold-trees-dir "$home/trees-volume/trees" --tools-dir "$home/tools" \
  --cold-cmd "$cold"

# Linux: the musl build the archives and .debs ship, as bench-linux's nightly builds it
cargo build --release --locked -p duscope --target x86_64-unknown-linux-musl
bench/run.sh acceptance --platform linux --duscope target/x86_64-unknown-linux-musl/release/duscope

bench/run.sh judge --summary bench/results/1.0/README.md
```

It runs three parts, in this order, and writes `results/1.0/<platform>.json` (`--out-dir`,
`--out`; a run on scaled trees writes `<platform>-x<scale>.json`, which is not the acceptance run):

- **`run`**: the harness with duscope as the subject, on T1–T3, warm and cold, paired with every
  pinned competitor (`--competitors`, `--cold-competitors`, `--runs`, `--warmup`, and the trees,
  tools and cold options mean what they mean in `run`), as `bench/run.sh run` writes it. Goal 1 is
  judged on it. The defaults are the full matrix: on bench-macos all eight tools cold on the disk
  image take more than the nightly job's 300 minutes (DEV-51); `--cold-competitors dua,du`
  measures what the macOS cold target reads, as the nightly does.
- **`index`**: the index measurements on T2 (`--index-runs`, 20 by default), as `bench/run.sh
  index` writes them, with dua and fdu for D34 when `--competitors` names them. Goal 2 is judged
  on its warm write. Left out when `--trees` leaves out T2.
- **`rescan`**: the incremental rescan against a full one, warm, on each tree. A round creates one
  4 KiB file, `.duscope-bench-rescan`, in each of up to `--changed-dirs` directories (100, and at
  most 1% of the tree's), spread over the tree in its build order; waits `--settle` seconds (2) for
  fseventsd to record the change; then times `rescan --incremental` and `rescan --full` of the
  same tree from the same saved scan, the incremental one first on even rounds and second on odd
  ones, `--runs` rounds after one unrecorded incremental rescan. The next round starts from the
  full rescan's scan. Both write a whole new index, so both times include the write, which `-v`
  splits out (`write_p50_s`). After each round the files are removed and the directories' times
  put back, so the tree still matches its stamp, which the part checks at the end. The harness
  times each rescan around the process rather than through hyperfine, since each answer names the
  scan the next round reads. The rows are `results` rows, `rescan --full` paired with `rescan
  --incremental`, so its `subject_ratio_p50` is the incremental rescan's time over the full one's;
  each tree also records the directories each incremental rescan read again, and a round that read
  fewer than its change touched is a failure (the history had not recorded the change: raise
  `--settle`). What the platform refuses is recorded with the binary's own message, and the tree is
  not measured:
  - Linux keeps no change history, so `rescan --incremental` is refused on every tree and every
    rescan reads the whole tree (plan/PROPOSAL.md, "Incremental rescans"): the Linux file says so
    for each tree, and measures no incremental rescan.
  - T3 has no directory below its root, so any change touches every directory it has, over the 15%
    an incremental rescan reads at most (`too_many_changes`): no platform reads it incrementally.
  - The cold runs' disk image on bench-macos is attached with `-nobrowse`, and fseventsd keeps no
    history for it, so the part reads the warm trees alone (`--trees-dir`).

The file holds the usual header (`host`, `harness.commit`, the duscope `version`), `run`, `index`
and `rescan` (each null when the part could not run), and `failures`, what could not be measured.
The command ends by judging its own platform's targets on stderr, and exits 1 when something
could not be measured, never because a target was missed.

### Judging it

`bench/run.sh judge` reads `results/1.0/linux.json` and `results/1.0/macos.json` (or the files it
is given) and prints each target with its number, `met` or `MISSED`, or `not measured` and why. It
exits 0 when every target is met, 1 when one is missed or not measured, and 2 when a file is not
an acceptance file. The targets are plan/PROPOSAL.md's goals 1 and 2, as its 1.0 acceptance targets
state them:

| Goal | Target | Read from | Source |
| --- | --- | --- | --- |
| 1 | Linux warm p50 ≤ 1.2× diskus, on T1–T3 | `run`: diskus's `subject_ratio_p50` | goal 1; target (1); Gate 1 → 2 |
| 1 | Linux warm p50 ≤ 1.15× the faster of pdu and fdu | the larger of the two ratios | goal 1; target (1); Gate 1 → 2 |
| 1 | Linux cold p50 ≤ 1.15× diskus | diskus's cold ratio | goal 1; target (1); Gate 1 → 2 |
| 1 | macOS warm p50 ≤ 1.0× diskus, ≤ 1.2× dua, ≤ 1.3× dumac | each one's warm ratio | goal 1; target (2); deferred from Gate 1 → 2 (DEV-52) |
| 1 | macOS cold p50 ≤ 1.2× dua | dua's cold ratio | goal 1; target (2); deferred from Gate 1 → 2 (DEV-52) |
| 2 | Linux warm index write ≤ 2.75 µs per entry, on T2 | `index`: `write_us_per_entry_warm_p50` | goal 2; target (3); DEV-107; Gate 2 → 3 |

The ratio is the harness's paired one (`subject_ratio_p50`, [Reading results](#reading-results)):
the median over the pair's rounds of duscope's time over the competitor's. A competitor that was
not measured leaves its line `not measured` with the tool's reason; for the faster of pdu and fdu
the one measured is judged, and the line says the other was not. The judge also says when a file
was not measured as the targets name it: a scaled run, Linux trees off ext4, macOS trees off APFS
or a Mac off Apple silicon. Goal 2's other half, that the write starts only after the treemap or
JSON is out, is no number: `p2_idx_02_the_write_starts_after_the_output` holds it on every
platform. The write's share of a cold scan is printed and holds no target (DEV-108). A missed
target is a Deviation entry's to explain, not the judge's.

`--summary FILE` also writes the Markdown summary of the files, every figure taken from them:
the machines, a table per goal with each target's source, and the rescans. The committed one is
`results/1.0/README.md`, for the top-level README to link to; it is generated, never edited, and
`bench/test_acceptance.py` fails when it differs from what the committed files give.

## The allocator

P1-BENCH-03 timed a warm T2 scan with the platform's own allocator and with mimalloc, three
alternating runs each, and kept mimalloc where it gains at least 2% (musl) or 10% (macOS);
`results/allocator.json` holds the runs. On the x86_64 musl build on bench-linux mimalloc took
the p50 from 1.02 s to 0.39 s, 62.1% faster. Both binaries were built on a GitHub-hosted
runner, because from-scratch builds on the bench host crashed rustc. A first measurement, of the
aarch64 build in a Debian 12 container on an Apple M5 Pro, went from 3.04 s to 0.41 s, 86.6%
faster. So the Linux archives and `.deb`s allocate with mimalloc, built with arena eager commit
and transparent huge pages off (`.cargo/config.toml`), which keeps a scan's peak with
`--attrs full` at 120–135 bytes per entry on T1–T3, within the RSS budget, where mimalloc's
defaults reached 168–244. On macOS
(arm64, APFS) it gained 1.7% (4.16 s to 4.09 s), so macOS keeps the system allocator.

## The agent eval

`bench/eval/` runs the ten disk-hygiene tasks of P3-AGT-03 through Claude Code, headless, and
decides the Gate 3 → 4 lines that wait on it: whether the skill `duscope agent --skill` writes
is recommended, whether `plain` stays the non-TTY default (D27), and whether `--jq` stays out
(D36). The runs call the model on the author's account, so only the author starts them; the
harness's own tests run a stub in Claude Code's place and call nothing.

```sh
cargo build --release -p duscope
npm install --global --prefix ~/.cache/duscope-eval @anthropic-ai/claude-code@2.1.319
export CLAUDE_CODE_OAUTH_TOKEN=...      # from `claude setup-token`; or ANTHROPIC_API_KEY
bench/eval/run.sh plan                  # the tasks, the cells, the run count and the limits
bench/eval/run.sh run --claude ~/.cache/duscope-eval/bin/claude --jobs 2
bench/eval/run.sh summary bench/eval/results/2026-11-20.json
bench/eval/run.sh run --fake right --runs 1     # the harness alone, with the stub; calls nothing
```

`bench/eval/run.sh` picks the Python as `bench/run.sh` does. Its subcommands are `run` (the
default), `plan`, `summary FILE`, and `tree DIR`, which builds the tree in DIR and prints each
task's answer.

**What a run is.** `tasks.toml` lists the tasks in the row's order: the prompt (a preamble that
names the tree and says to use `duscope` and change nothing, the question, and the shape of the
`ANSWER:` line of JSON the reply must end with), the fixture, how the answer is worked out and how
the reply is checked. `config.toml` pins the model (`claude-sonnet-5-5`), the Claude Code release
(2.1.319, the one the MCP smoke test installs; the runner refuses another), the tools, the cells
and the limits. Each run has a directory of its own in the system's temporary directory
(`--work-dir` elsewhere), removed after it (`--keep-work` keeps it):

- a scratch `HOME` and XDG directories, so the author's Claude Code settings, memory, MCP servers
  and saved scans are not read, and the run's scans go to its own index directory; in the cells
  with the skill, `duscope agent --skill --dir $HOME` writes it there first. Claude Code's user
  settings there (`.claude/settings.json`) set `syncClaudeAiSkills` to false, so the skills a
  Claude account syncs from claude.ai are not loaded. The skills Claude Code bundles still are,
  and they are the same in every cell: each run's `skills_listed` names them. The runs of
  2026-11-16 were made without the setting, and listed the same 17 skills besides duscope's in
  every cell;
- the tree (`evaltree.py`): a small home directory, 220 MiB in about 460 files, built fresh, every size
  and age fixed, each age counted back from the build, so that each task's answer stands clear of
  the next candidate (`bench/test_eval.py` checks the margins). Every change time is the build's,
  which `duscope stale` also requires to be old, so the questions about files unused for a year
  ask about modification time and `stale` lists nothing on this tree. The diff task's tree also
  has two saved scans, one before and one after a few changes;
- `PATH` with a `duscope` shim that runs the binary under test (`--duscope`, default
  `target/release/duscope`), copied once when the eval starts, so that a build made meanwhile does
  not reach its runs, and linked into the run's directory, so that nothing the agent is shown names
  the checkout, where `evaltree.py` holds every answer; that logs each call; and that refuses
  `clean --yes` and `--allow-delete` (with which `mcp` offers its client the tool that moves paths
  to the Trash), so that no duscope call moves anything to a Trash (on macOS the Trash is the
  account's own). `clean` without `--yes`, with `--permanent` or not, only shows its plan, and
  passes;
- `claude -p PROMPT --output-format stream-json --verbose --model MODEL --max-turns 30
  --allowedTools ... --disallowedTools ... --strict-mcp-config`, in an empty working directory.
  Only the variables `config.toml`'s `pass_env` names reach it, and the credential:
  `ANTHROPIC_API_KEY`, or `CLAUDE_CODE_OAUTH_TOKEN` for a Claude subscription. With neither set
  the runner stops before it runs anything. Print mode cannot ask, so a tool outside
  `allowed_tools` (duscope, `jq` after a pipe, Read, Grep, Glob and Skill) is denied and recorded.

Past the shim, what holds a run is Claude Code's matching of `Bash(duscope:*)` and nothing else:
there is no sandbox, the network is not cut off, and Read (with Grep and Glob) is not limited to
the tree, so it reaches any file the account can read. The runs of 2026-11-16 show that it refuses
a command with a part outside `allowed_tools`, and one that sets or reads a shell variable: of 152
refusals, 57 were a `cd` before `duscope`, 51 a variable ("A variable in this command can't be
checked before it runs"), and the rest `sed`, `head`, `cat`, `grep`, `python3` or `ls` beside it.
Each was recorded as a permission denial, and every run went on to complete. A run that changes the
tree fails as `tree changed`, and its transcript keeps the commands.

A run is completed when its answer is right, it stayed within 600 s and 2,000,000 tokens, it left
the tree as it was built, and no deletion was refused. The tree is the same when the same entries
are under its root, each still a directory or not and, if not, of the same size and modification
time (`lstat`); a directory's times, every access and change time, modes, and
anything outside the tree are not compared. The answer is the JSON object after the reply's last
`ANSWER:` line (or `Final ANSWER:`, in any case) that one follows, so a line of prose after it is
passed over. Paths are compared after `realpath`, and one that is relative or starts with `~` is
read under the tree's root, which the prompt calls a home directory. Sizes are compared within the
task's tolerance (5%), and a size may be written in bytes or with a unit, or with its bytes in
parentheses after another figure (`63 MiB (66,246,656 bytes)`), which reads as those bytes. Most
truths come from the tree itself, walked with `lstat`; two are duscope's own figures: the diff
task's net growth (the difference of the totals duscope gave the two saved scans) and which
directories are regenerable caches (`duscope caches --json`, run by the harness in an index
directory the agent never sees). A duscope bug there passes both the truth and the answer, which
suits an eval of agents and not of duscope's figures.

A run that measured the infrastructure rather than the agent fails as `infrastructure`, with the
reason beside it: Claude Code ended without a result message, with an error result (`is_error`: a
rate limit, an expired token, a failed request), or with another exit status than 0; the stream
names a model other than the pinned one, or none; a run of a cell with the skill lists no
`duscope` skill; or the stream has a shape the harness cannot read, whose transcript is kept for a
look. Events Claude Code streams besides messages, such as a notice whose `message` is text, are
passed over. A run that used all its turns is the agent's own failure, though Claude Code marks
it an error; a timeout, a changed tree and a refused deletion stay the agent's too. Such a run is
recorded and counted apart, and left out of every figure but the cost: completion, tokens and the
decisions read the other runs alone. Three in a row stop the eval as Ctrl-C does, so an account
that cannot reach the model does not spend the queue; `--resume` makes each one again.

The cells are with and without the skill, crossed with the output format duscope gives when the
agent passes none (`DUSCOPE_FORMAT`): `json`, what the agent detection gives under Claude Code,
and `plain`, the non-TTY default. An agent's own `--json` still wins, and the result counts how
often it passed one. Three runs of each task in each cell are 120 runs, made in interleaved
order: every cell runs a task before the next task, and every task runs once before any runs
again.

**What it costs.** 120 runs, each up to 30 turns. With a Claude subscription's token
(`CLAUDE_CODE_OAUTH_TOKEN`) the runs count against the plan's usage limits, and are billed only past
them where the plan buys usage on demand; with an API key they are billed per token. Headless Claude
Code reads its system prompt and tools from the prompt cache on every turn, so most of a run's
tokens are cache reads. Measured on 2026-11-16 (`results/2026-11-16.json`): about 100,000–160,000
tokens and 6–9 turns a run on average by cell, which Claude Code prices at $0.07 a run at Sonnet
5.5's API rates ($2 a million input tokens, $10 output, $0.20 cache reads), $8.71 for the 120 runs.
`total_cost_usd` in each record is that price, not a bill, and `summary` sums it per cell. A run
took 14 seconds on average, and the eval 15 minutes at `--jobs 2`; `--jobs N` runs N at once,
within the account's rate limits. Ctrl-C stops the eval: the runs not started are cancelled and the running agents ended
(each runs in a session of its own, which the terminal's Ctrl-C does not reach), and the eval
exits with status 130. A run cut short is not recorded. `--resume FILE` continues an interrupted
run: the runs in FILE are kept and not made again, but for those that failed for the
infrastructure, provided `config.toml`, `tasks.toml`, the Claude Code release, the commit and
duscope's version are unchanged, so that one file measures one binary.
The eval exits with status 0 when every run is recorded and none failed for the infrastructure,
1 when some are left for `--resume`, and 2 on a problem with its configuration or setup.

**What it records.** `bench/eval/results/<date>.json` (`--out` elsewhere), rewritten after every
run and marked `complete` at the end. A second start refuses a result file that exists, so a second
run on the same UTC day needs `--resume` that file or another `--out`; it also refuses a
transcripts directory that holds another run's. The file holds the host, the commit, the hashes of
both files, the Claude Code and duscope versions, the configuration, each task's prompt (`{root}`
for the tree, as everywhere in the file, and `{work}` for the run's own directory: its scratch
`HOME` and the folder the tree is in, so the file names no path of the account that ran it), and
one record per run with its verdict and the reasons, the expected and the given
answer, the models the stream names, tokens (`total`: the pinned model's input, cache creation,
cache reads and output, from `modelUsage`, else the result's `usage`, else the sum of the
messages'; `all_models` with any other model Claude Code called), the cost, turns, tool calls,
failed calls and whether a later one worked (`recovered`), permission denials, whether the skill
was loaded, the duscope calls (verbs, the tier they belong to, formats asked for, `--jq`, the
briefing read with `duscope agent`), and the commands it ran. A field the stream lacks is null,
never 0 or false, and so is a token total that needs it. The `summary` holds each cell's figures,
its runs that failed for the infrastructure, each task's completion per cell, and the decisions.
Each run's transcript stays in `bench/eval/.runs/<date>/`, which git ignores. The fake agent's
files are `<date>-fake-right.json` and `<date>-fake-wrong.json`, also ignored.

**The decisions.** The criterion is the row's (DEV-185): the cells with the skill against those
without, both formats pooled. The skill is recommended when its arm completed at least three runs
more than the other (`completion_margin_runs` in `config.toml`: 5 points of 60; one run is 1.7
points, and a single rate-limited call can be that run). Within the margin the arms count as equal,
and the skill is recommended when the mean tokens of a completed run are at least 20% lower;
otherwise it stays optional and its description is rewritten, as PROPOSAL's Agent workflows says.
The mean is over completed runs alone, since a timeout's partial count or a failed call's zero is
not "tokens at equal completion". Arms of unequal size (a run still left for `--resume`) compare
as completion rates, the margin taken at their mean size. The same rule, json against plain with
and without the skill pooled, revises D27 when json wins and confirms `plain` otherwise; since an
agent's own `--json` or `--plain` wins over the cell's format, each cell also counts the runs whose
duscope calls passed no format (`default_format_runs`, from each run's `duscope.formats`), the runs
that read the cell's format alone. A run that passes `--jq` to duscope counts as reaching for it,
and one such run revisits D36. When no run completed anything the decisions are `undecided`.

The decisions hold for the pinned model, `claude-sonnet-5-5`, which the summary's first line and
both checklist lines name; when both arms complete 58 of 60 runs or more, the skill line adds that
the eval measured tokens alone. `run.sh summary FILE` prints the criterion with its margin, the
table and the checklist's two lines, filled, with a line for `--jq`; the author pastes them into
plan/SPEC.md's Gate 3 → 4 checklist, records a changed default as a Deviation entry, and commits
the result file.

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
| `indexbench.py` | the index measurements (`index`) |
| `redraw.py` | the viewer's redraw budget (`redraw`) |
| `test_redraw.py` | its unit tests, run with `test_bench.py`; `duscope/tests/pty/nav_redraw.rs` runs it whole |
| `eval/run.sh`, `eval/agent_eval.py` | the agent eval: the command line, a run's setup, the result file and the decisions |
| `eval/tasks.toml`, `eval/config.toml` | the ten tasks; the model, the agent, the cells and the limits |
| `eval/evaltree.py`, `eval/evalchecks.py` | the eval's tree; each task's truth and the checks of a reply |
| `eval/evaltranscript.py`, `eval/fake_claude.py` | a run's transcript and duscope calls read; the stub agent |
| `eval/minitoml.py` | the TOML reader for Pythons without `tomllib` |
| `test_eval.py` | the eval's unit and end-to-end tests (with the stub), run with `test_bench.py` |
| `test_indexbench.py` | its unit tests, run with `test_bench.py`; `duscope/tests/index_bench.rs` runs it whole |
| `acceptance.py` | the 1.0 acceptance run (`acceptance`), the goal-1 and goal-2 targets, the judgement and the summary (`judge`) |
| `rescanbench.py` | the acceptance run's incremental rescans against full ones |
| `test_acceptance.py` | their unit tests, run with `test_bench.py`; `duscope/tests/acceptance_bench.rs` runs `acceptance` whole |
| `results/1.0/` | the acceptance run's files, one per platform, and their generated summary, `README.md` |
| `compare.py` | the regression check: a result file against the results before it on the same machine and tree |
| `test_compare.py` | unit tests of `compare.py`, run by `duscope/tests/nightly_bench.rs` |
| `testdata/` | a real hyperfine 2.0.0 export and the hyperfine stand-in the tests use |
| `gab_bench.c` | the per-entry cost of `getattrlistbulk` attribute sets on macOS |
| `sweep.py` | the thread sweep and the rule that sets a platform's default thread count |
| `results/clone-attributes-macos.json` | P1-BENCH-02's measurement and the `--clones` and `PRIVATESIZE` decisions |
| `results/redraw-macos-arm64-provisional.json` | P4-TEST-01's provisional macOS-local redraw number, until the nightly measures it |
| `results/allocator.json` | P1-BENCH-03's measurement: mimalloc on the musl builds, the system allocator on macOS |
