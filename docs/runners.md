# Benchmark runners

Two self-hosted GitHub Actions runners run the nightly benchmarks
([nightly-bench.yml](../.github/workflows/nightly-bench.yml)): an Apple-silicon
Mac with the label `bench-macos` and a Linux x86_64 box with `bench-linux`. The
phase gates are judged on their numbers, never on GitHub's shared runners, which
are too noisy for absolute targets. This page describes the machines, how a
runner is set up, and what keeps a public repository's code off them.

Every night at 03:17 Pacific the workflow runs `bench/run.sh` on both machines
(the three trees, the pinned competitors, warm and cold caches) and pushes the
result files to the `bench-results` branch as `<date>-<platform>.json`.
[bench/README.md](../bench/README.md) documents the harness and the result file;
`bench/compare.py` reads the files as baselines.

## The machines

A result is comparable only with results from the same machine, so a change to
anything in this table is a new row in the [change log](#change-log).

| | `bench-macos` | `bench-linux` |
| --- | --- | --- |
| Machine | the author's MacBook Pro (Mac17,8), [below](#bench-macos-the-authors-macbook-pro) | VM `bench-linux` (VMID 101) on the author's Proxmox VE 9.2 host, [below](#bench-linux-a-vm-on-proxmox) |
| CPU and cores | Apple M5 Pro, 18 cores | 10 vCPUs of an Intel Core i9-10900K (10 cores, 20 threads), one per core, CPU type `host`, shared with the host's other VMs |
| Memory | 48 GiB, shared with the author's own work | 8 GiB, fixed (no ballooning) |
| OS and kernel | macOS 26.7.2 (25G330), Darwin 25.6.0 | Ubuntu 24.04.5 LTS, Linux 6.8.0-147-generic |
| Disk holding `BENCH_HOME` | the internal SSD | a 64 GB virtio-scsi disk (`ssd=1`, `discard=on`, no host cache) on LVM-thin storage on the host's Sabrent NVMe SSD |
| Filesystem of `BENCH_HOME` | APFS, the Data volume; the trees on an APFS sparse-bundle disk image inside it ([below](#bench-macos-the-authors-macbook-pro)) | ext4, the root filesystem (`relatime,discard`) |
| Free space and inodes | 471 GiB; APFS allocates inodes as needed | 58 GB and 8.1 million inodes, with the tools installed and no trees yet |
| Network | Wi-Fi on the home LAN; not filtered ([Network](#network)) | bridged to the home LAN, DHCP; outbound to the internet only ([Network](#network)) |
| Power and location | on AC power at the author's home, woken at 03:10 every day | the Proxmox host, in the author's home |

### bench-macos: the author's MacBook Pro

The macOS runner is the author's own laptop, not a dedicated machine, so the
nightly run competes with nothing only when the laptop is idle, and runs at all
only when it's plugged in and awake: a night it's asleep, unplugged or away is
skipped with a notice. `pmset` wakes it at 03:10 every day, and the nightly job
runs the harness under `caffeinate -is`, which keeps the laptop awake until the
harness exits. What keeps the runner apart from the author's work:

- `runner` has a group of its own instead of `staff`, so it shares no group with
  the author, and the author's home folder is owner-only (`chmod 700 ~`).
- A launchd daemon in the system domain runs the runner as `runner` at boot.
  GitHub's `svc.sh` installs a LaunchAgent instead, which runs only while
  `runner` is logged in.

The trees live on an APFS disk image, `BENCH_HOME/trees.sparsebundle`, attached
at `BENCH_HOME/trees-volume`, not on the Data volume itself. `purge` leaves
APFS's metadata caches warm, so a cold run there measured little more than a
warm one (`du` on T1: 2.94 s cold, 2.85 s warm); the cold command detaches the
image, purges, and attaches it again (bench/README.md, "Cold runs"). The nightly
job creates the image the first night it runs and attaches it after a reboot;
`runner` owns it, and `hdiutil` needs no root for that. Trees built before the
image, in `BENCH_HOME/trees`, are no longer read: `sudo rm -rf
/Users/runner/duscope-bench/trees` frees their 3.6 GiB.

### bench-linux: a VM on Proxmox

The Linux runner is a VM on the author's Proxmox host, not a machine of its own,
so other VMs share its CPU and disk; ratios within a run hold up better than
absolute times between nights. It has 10 vCPUs, one per core of the host, so
P1-BENCH-01's thread sweep can choose a default above 4 (with 4 it could choose
nothing else); `qm set 101 --cores 10` on the host, then a reboot of the VM,
set it. It was created with
[claude-on-proxmox](https://github.com/kawadeomkar/claude-on-proxmox)'s
provision stage alone, with a tag of its own, so that project's configuration
(Claude Code, cloned repositories, sign-ins) never reaches it and its everyday
commands do not see it:

```sh
# in the claude-on-proxmox checkout
ANSIBLE_ARGS='-e claude_vm_tag=duscope-bench -e vm_cores=10 -e vm_memory_mb=8192 -e vm_disk_size=64G' \
  just deploy --only provision bench-linux
# on the Proxmox host: the disk is NVMe-backed, so the guest is told it is an SSD
qm set 101 --scsi0 local-lvm:vm-101-disk-0,discard=on,size=64G,ssd=1 && qm reboot 101
```

The same `ANSIBLE_ARGS='-e claude_vm_tag=duscope-bench'` before `just list` or
`just destroy bench-linux` reaches it. The rest of its setup is this page's: the
cloud-init user `dev` (the author's SSH key, passwordless sudo) administers it,
and the runner runs as `runner`. Besides the tools below it has
`build-essential`, `pkg-config`, `jq` and `nftables` from apt.

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
# macOS: a hidden standard account with a group of its own, not staff; your own
# home folder becomes owner-only, so runner can't read it
sudo sysadminctl -addUser runner -fullName "duscope bench runner" -password "$(openssl rand -base64 30)" -home /Users/runner
sudo createhomedir -c -u runner
sudo dscl . -create /Users/runner IsHidden 1
sudo dseditgroup -o create -r "duscope bench runner" runner
sudo dscl . -create /Users/runner PrimaryGroupID "$(dscl . -read /Groups/runner PrimaryGroupID | awk '{print $2}')"
sudo chown runner:runner /Users/runner && sudo chmod 750 /Users/runner
chmod 700 ~
# Linux
sudo useradd --create-home --shell /bin/bash runner
```

`sudo` keeps the caller's `PATH`, so a script of the author's named like a
system tool (`install`, say) would run as root in its place; prefix the root
commands with `env PATH=/usr/bin:/bin:/usr/sbin:/sbin` when that is a risk.

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
(`cargo install --locked hyperfine@2.0.0`, the version `just setup` pins). On
bench-linux also `musl-tools` (`sudo apt-get install musl-tools`, done by the
`dev` user): each night builds duscope for the musl target, the binary the Linux
packages ship, and the job adds the target to the toolchain itself. The
harness installs the competitors itself, under `BENCH_HOME`. After the harness,
each bench job builds the fixture tree and fails when it takes 30 s or more
(P0-TEST-01's budget, which shared CI runners only report): rustup installs the
workspace's pinned toolchain on the first night, and cargo keeps its build under
`BENCH_HOME/target`.

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
echo "$HOME/.cargo/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin" > .path
```

The runner reads `.env` and `.path` into every job: `.env` holds `NAME=value`
lines, and `BENCH_HOME` tells the workflow where the trees live; `.path` holds
the value of `PATH` itself, with no `PATH=` in front, and puts hyperfine and
cargo on it.
Then install it as a service that starts at boot:

```sh
# macOS: a launchd daemon that runs the runner as runner at boot, with no login
cp bin/runsvc.sh runsvc.sh && chmod u+x runsvc.sh                 # as runner
sudo install -d -o runner -g runner ~runner/Library/Logs/actions.runner.kawadeomkar-duscope.bench-macos
sudo tee /Library/LaunchDaemons/actions.runner.kawadeomkar-duscope.bench-macos.plist   # the plist below
sudo launchctl bootstrap system /Library/LaunchDaemons/actions.runner.kawadeomkar-duscope.bench-macos.plist
sudo pmset repeat wakeorpoweron MTWRFSU 03:10:00                   # awake for the 03:17 run
# Linux: a systemd unit that runs as runner
sudo ./svc.sh install runner && sudo ./svc.sh start
```

The macOS daemon is GitHub's own plist template with `UserName` and
`GroupName` set and `KeepAlive` on, loaded into the system domain:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
  <dict>
    <key>Label</key><string>actions.runner.kawadeomkar-duscope.bench-macos</string>
    <key>ProgramArguments</key><array><string>/Users/runner/actions-runner/runsvc.sh</string></array>
    <key>UserName</key><string>runner</string>
    <key>GroupName</key><string>runner</string>
    <key>WorkingDirectory</key><string>/Users/runner/actions-runner</string>
    <key>RunAtLoad</key><true/>
    <key>KeepAlive</key><true/>
    <key>StandardOutPath</key><string>/Users/runner/Library/Logs/actions.runner.kawadeomkar-duscope.bench-macos/stdout.log</string>
    <key>StandardErrorPath</key><string>/Users/runner/Library/Logs/actions.runner.kawadeomkar-duscope.bench-macos/stderr.log</string>
    <key>EnvironmentVariables</key><dict><key>ACTIONS_RUNNER_SVC</key><string>1</string></dict>
  </dict>
</plist>
```

Check that GitHub sees it online, then start a smoke run (T1 at 1% scale, warm
caches, `du` alone) and read its result on the `bench-results` branch:

```sh
gh api repos/kawadeomkar/duscope/actions/runners --jq '.runners[] | {name, status, labels: [.labels[].name]}'
gh workflow run nightly-bench.yml --repo kawadeomkar/duscope -f trees=T1 -f competitors=none -f mode=warm -f scale=0.01
```

## The tokens and the results branch

`BENCH_RUNNERS_TOKEN` is a fine-grained personal access token of the author's,
for the one repository `kawadeomkar/duscope`, with one permission:
Administration read. The workflow's `probe` job uses it to list the runners and
their status, which the run's own `GITHUB_TOKEN` cannot; it can change nothing.
It expires after a year and is rotated with the other secrets at the first
release after each anniversary (plan/SPEC.md, "Release checklist").

```sh
gh secret set BENCH_RUNNERS_TOKEN --repo kawadeomkar/duscope
```

The results are pushed by the `publish` job, on a GitHub-hosted runner, with the
run's own `GITHUB_TOKEN`, which that job alone is given `contents: write` for. It
expires with the run and needs no rotation, and branch protection, which binds
it too, keeps it off `main`; the job pushes nowhere but `bench-results`. The
bench jobs on these machines get no secret, and their `GITHUB_TOKEN` is
read-only.

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
downloads, crates.io and static.crates.io, and to static.rust-lang.org for the
pinned toolchain. They accept no inbound connection
the runner needs; it polls GitHub. A wired connection is preferred, since a
dropped connection fails the job mid-run.

A runner on a home network must not reach the rest of it: a competitor built
from crates.io runs its build script on the machine. `bench-linux` refuses new
outbound connections to private addresses, except DNS, DHCP and NTP, with this
`/etc/nftables.conf` (owned by root, so the `runner` user cannot change it), and
has IPv6 turned off in `/etc/sysctl.d/60-duscope-bench.conf`, since the LAN's
hosts also have global IPv6 addresses that no private-range rule would catch:

```text
flush ruleset

table inet duscope_egress {
  set private4 {
    type ipv4_addr
    flags interval
    elements = { 10.0.0.0/8, 100.64.0.0/10, 169.254.0.0/16, 172.16.0.0/12, 192.168.0.0/16 }
  }

  chain output {
    type filter hook output priority filter; policy accept;
    oif "lo" accept
    ct state established,related accept
    udp dport { 53, 67, 123 } accept
    tcp dport 53 accept
    ip daddr @private4 counter reject with icmpx admin-prohibited
    meta nfproto ipv6 counter reject with icmpx admin-prohibited
  }
}
```

SSH from the LAN still works, since replies belong to a connection the VM did
not start. Loading the rules over SSH freezes that one session, whose replies
conntrack first sees after the rules are in place; open a new one.

`bench-macos` is not filtered: a build script running as `runner` can reach the
LAN. Holding it to the internet would take a `pf` anchor that blocks `runner`'s
outbound connections to private addresses.

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
- The runner user is unprivileged, with `sudo` for the cache drop alone.
  `bench-linux` holds nothing else of value. `bench-macos` is the author's own
  laptop: `runner` shares no group with the author and can't read the author's
  home folder, but it can reach the LAN.

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
issue `nightly-bench offline` and pins it, or updates its description, which
notifies no one, when it is open; the next run with both runners online closes it. A release needs a nightly result
at most 72 hours old.

To look at a runner: the `gh api .../runners` command above shows its status;
on the machine, `./svc.sh status` (Linux) or `sudo launchctl print
system/actions.runner.kawadeomkar-duscope.bench-macos` (macOS) shows the
service, and `~/actions-runner/_diag/` holds the runner's logs.

## Re-registering a runner

After a reinstall, a move to new hardware, or a runner that stays offline:

```sh
gh api -X POST repos/kawadeomkar/duscope/actions/runners/remove-token --jq .token   # the author
cd ~/actions-runner && sudo ./svc.sh stop && sudo ./svc.sh uninstall   # Linux, as runner
sudo launchctl bootout system /Library/LaunchDaemons/actions.runner.kawadeomkar-duscope.bench-macos.plist   # macOS
./config.sh remove --token <token>
```

Then register again as above, with the same name and label. New hardware is a
new row in the change log, and its results are not compared with the old
machine's by eye; `bench/compare.py` compares only results of the same
platform and architecture.

## Change log

| Date | Runner | Change |
| --- | --- | --- |
| 2026-10-25 | `bench-linux` | Registered: a VM on the author's Proxmox host, runner 2.338.1, hyperfine 2.0.0, rustc 1.99.1 |
| 2026-10-25 | `bench-macos` | Registered: the author's MacBook Pro (M5 Pro, 18 cores, 48 GiB, macOS 26.7.2), runner 2.338.1, hyperfine 2.0.0, rustc 1.99.1; a launchd daemon in the system domain; a daily wake at 03:10 |
| 2026-10-25 | `bench-linux` | 10 vCPUs, one per core of the host (was 4), for the thread sweep |
| 2026-10-25 | `bench-macos` | the trees on an APFS disk image, which the cold command remounts around each purge |
