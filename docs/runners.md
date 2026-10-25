# Benchmark runners

Two self-hosted GitHub Actions runners run the nightly benchmarks
([nightly-bench.yml](../.github/workflows/nightly-bench.yml)): an Apple-silicon
Mac with the label `bench-macos` and a Linux x86_64 box with `bench-linux`. The
phase gates are judged on their numbers, never on GitHub's shared runners, which
are too noisy for absolute targets. This page describes the machines, how a
runner is set up, and what keeps a public repository's code off them.

Every night at 03:00 Pacific the workflow runs `bench/run.sh` on both machines
(the three trees, the pinned competitors, warm and cold caches) and pushes the
result files to the `bench-results` branch as `<date>-<platform>.json`.
[bench/README.md](../bench/README.md) documents the harness and the result file;
`bench/compare.py` reads the files as baselines.

## The machines

A result is comparable only with results from the same machine, so a change to
anything in this table is a new row in the [change log](#change-log).

| | `bench-macos` | `bench-linux` |
| --- | --- | --- |
| Machine | to be recorded at registration | to be recorded at registration |
| CPU and cores | | |
| Memory | | |
| OS and kernel | | |
| Disk holding `BENCH_HOME` | | |
| Filesystem of `BENCH_HOME` | | |
| Free space and inodes | | |
| Network | | |
| Power and location | | |

The commands that print these facts, run as the runner user:

```sh
# macOS
sysctl -n machdep.cpu.brand_string hw.ncpu hw.memsize; sw_vers; uname -r
diskutil info "$(df "$BENCH_HOME" | awk 'NR==2 {print $1}')" | grep -E 'Device / Media Name|File System Personality|Solid State|Protocol'
df -h "$BENCH_HOME"; df -i "$BENCH_HOME"

# Linux
lscpu | grep -E '^(Model name|CPU\(s\))'; free -h; grep PRETTY_NAME /etc/os-release; uname -r
findmnt -no SOURCE,FSTYPE,OPTIONS -T "$BENCH_HOME"; lsblk -d -o NAME,MODEL,ROTA,SIZE
df -h "$BENCH_HOME"; df -i "$BENCH_HOME"
```

## Setting up a runner

Each step is done once per machine, in this order. The commands for the Mac and
the Linux box differ only where marked.

### The runner user

The runner runs as a dedicated, unprivileged user named `runner`, which owns
nothing outside its home directory and is not an administrator.

```sh
# macOS (an account that does not log in by password over the network)
sudo sysadminctl -addUser runner -fullName "duscope bench runner" -password -
# Linux
sudo useradd --create-home --shell /bin/bash runner
```

Its only privilege is the cache drop that cold runs need (bench/README.md,
"Cold runs"): passwordless `sudo` for exactly one command. Install the line for
the machine's OS, and only that one, in `/etc/sudoers.d/duscope-bench`:

```text
runner ALL=(root) NOPASSWD: /usr/sbin/purge
runner ALL=(root) NOPASSWD: /usr/sbin/sysctl -q vm.drop_caches=3
```

```sh
sudo visudo -cf /etc/sudoers.d/duscope-bench   # checks the file before sudo reads it
sudo -u runner sudo -n -l                       # lists the one command
```

The tools the harness needs, installed as `runner`: git, curl, tar and Python
3.9 or later (the system's `/usr/bin/python3`; on macOS from the Xcode command
line tools, `xcode-select --install`), rustup with the stable toolchain
(`curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
--profile minimal`) for the competitors built from source, and hyperfine 2.0.0
(`cargo install --locked hyperfine@2.0.0`, the version `just setup` pins). The
harness installs the competitors itself, under `BENCH_HOME`.

### Disks and filesystems

`BENCH_HOME` (default `~runner/duscope-bench`) holds the trees and the
competitors, outside the runner's work directory so that a clean checkout keeps
them. It must be on a local disk, not a network share, on the filesystem whose
numbers the gates are about: APFS on the Mac, ext4 on the Linux box.

The trees take 3.6 GiB on APFS (bench/README.md, "The trees") and about 2.5
million inodes; T1's files are sparse, and the harness refuses a filesystem that
would write the holes out. ext4 gives one inode per 16 KiB by default, so it needs
a filesystem of 40 GiB or more, or more inodes at `mkfs` time. The competitors
built from source and cargo's cache take about 2 GiB more. Keep 20 GiB and 3
million inodes free (`df -h`, `df -i`). Nothing else should write to that disk
during the nightly window.

### Registering

The registration token comes from the API and lasts an hour; the author fetches
it:

```sh
gh api -X POST repos/kawadeomkar/duscope/actions/runners/registration-token --jq .token
```

As `runner`, download the runner application with the commands that
Settings → Actions → Runners → New self-hosted runner shows (they check the
tarball's SHA-256), then configure it with the machine's label:

```sh
cd ~/actions-runner
./config.sh --unattended --url https://github.com/kawadeomkar/duscope \
  --token <token> --name bench-macos --labels bench-macos   # bench-linux on the Linux box
echo "BENCH_HOME=$HOME/duscope-bench" >> .env
echo "PATH=$HOME/.cargo/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin" > .path
```

The runner reads `.env` and `.path` into every job: `BENCH_HOME` tells the
workflow where the trees live, and `.path` puts hyperfine and cargo on `PATH`.
Then install it as a service that starts at boot:

```sh
# macOS: a launchd agent of the runner user, so that user logs in automatically
# (System Settings → Users & Groups → Automatically log in as: runner)
./svc.sh install && ./svc.sh start
# Linux: a systemd unit that runs as runner
sudo ./svc.sh install runner && sudo ./svc.sh start
```

Check that GitHub sees it online, then start a smoke run (T1 at 1% scale, warm
caches, `du` alone) and read its result on the `bench-results` branch:

```sh
gh api repos/kawadeomkar/duscope/actions/runners --jq '.runners[] | {name, status, labels: [.labels[].name]}'
gh workflow run nightly-bench.yml --repo kawadeomkar/duscope -f trees=T1 -f competitors=none -f mode=warm -f scale=0.01
```

## The token and the results branch

`BENCH_RESULTS_TOKEN` is a fine-grained personal access token of the author's,
for the one repository `kawadeomkar/duscope`, with two permissions: Contents
read and write (the publish job pushes to `bench-results`) and Administration
read (the probe lists the runners and their status, which `GITHUB_TOKEN` cannot).
It expires after a year and is rotated with the other secrets at the first
release after each anniversary (plan/SPEC.md, "Release checklist").

```sh
gh secret set BENCH_RESULTS_TOKEN --repo kawadeomkar/duscope
```

Only the two GitHub-hosted jobs, `probe` and `publish`, read it; the bench jobs
on these machines get no secret. A token can push wherever its owner can, and
branch protection, administrators included, keeps it off `main`; the workflow
pushes nowhere but `bench-results`.

The first publish creates `bench-results` as a branch of its own history,
holding the result files and a README. A ruleset keeps it from being deleted or
rewritten, with no bypass:

```sh
gh api -X POST repos/kawadeomkar/duscope/rulesets --input - <<'EOF'
{
  "name": "bench-results",
  "target": "branch",
  "enforcement": "active",
  "conditions": { "ref_name": { "include": ["refs/heads/bench-results"], "exclude": [] } },
  "rules": [{ "type": "deletion" }, { "type": "non_fast_forward" }]
}
EOF
```

## Network

Both machines need outbound HTTPS only: to github.com (the runner, the
checkout, artifact uploads) and, for the competitors, to GitHub release
downloads, crates.io and static.crates.io. They accept no inbound connection;
the runner polls GitHub. A wired connection is preferred, since a run downloads
nothing large after the first night, but a dropped connection fails the job
mid-run.

## Security

GitHub advises against self-hosted runners on public repositories, because a
pull request from a fork could run its code on the machine. These rules keep that
from happening, and `duscope/tests/nightly_bench.rs` checks every workflow for
the first two:

- A workflow with a job on a self-hosted label is triggered by `schedule` and
  `workflow_dispatch` only, never by `pull_request`, `pull_request_target` or
  `workflow_run`.
- Every such job also carries `if: github.event_name != 'pull_request'`.
- Only `main`'s copy of the workflow reaches the runners: the probe stops a
  dispatch of any other branch. Dispatching needs write access to the
  repository.
- Workflows from fork pull requests wait for approval (docs/github-settings.md).
- No secret reaches these machines, and their jobs' `GITHUB_TOKEN` is
  read-only.
- The runner user is unprivileged, with `sudo` for the cache drop alone. The
  machines hold nothing else of value; the Mac is not anyone's daily machine.

The runners are not ephemeral: a job could leave files behind for the next one.
That is acceptable only because nothing but `main`'s code runs on them.

## Updates

- The runner application updates itself; a runner too old to update stops
  taking jobs and shows offline.
- OS updates are applied monthly, outside the nightly window. Each result file
  records `host.os` and `host.kernel`, so an update shows in the results; a
  major version goes in the change log.
- hyperfine stays at 2.0.0 and the competitors at the versions `bench/tools.py`
  pins; a change to either is a pull request.

## When a runner is offline

The run never fails because of it. The workflow's `probe` job, on a
GitHub-hosted runner, asks the API which runners are online. The bench job of a
runner that is offline (or missing, or when the API cannot be read) is skipped
with a notice. When a platform is skipped twice in a row, the probe opens the
issue `nightly-bench offline` and pins it, or comments on it when it is open;
the next run with both runners online closes it. A release needs a nightly result
at most 72 hours old.

To look at a runner: the `gh api .../runners` command above shows its status;
on the machine, `./svc.sh status` shows the service, and `~/actions-runner/_diag/`
holds the runner's logs.

## Re-registering a runner

After a reinstall, a move to new hardware, or a runner that stays offline:

```sh
gh api -X POST repos/kawadeomkar/duscope/actions/runners/remove-token --jq .token   # the author
cd ~/actions-runner && ./svc.sh stop && sudo ./svc.sh uninstall   # as runner; plain ./svc.sh uninstall on macOS
./config.sh remove --token <token>
```

Then register again as above, with the same name and label. New hardware is a
new row in the change log, and its results are not compared with the old
machine's by eye; `bench/compare.py` compares only results of the same
platform and architecture.

## Change log

| Date | Runner | Change |
| --- | --- | --- |
