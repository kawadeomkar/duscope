#!/usr/bin/env bash
# Drop the filesystem caches before a cold run: the harness passes this script
# to hyperfine's --prepare. It needs root, or passwordless sudo for exactly the
# command it runs (bench/README.md, "Cold runs"):
#   macOS  /usr/sbin/purge
#   Linux  /usr/sbin/sysctl -q vm.drop_caches=3   (the same as echo 3 > /proc/sys/vm/drop_caches)
# sudo runs with -n, so a missing rule fails at once instead of asking for a password.
set -euo pipefail

sync
case "$(uname -s)" in
  Darwin)
    if [[ $(id -u) -eq 0 ]]; then /usr/sbin/purge; else sudo -n /usr/sbin/purge; fi ;;
  Linux)
    if [[ $(id -u) -eq 0 ]]; then
      echo 3 > /proc/sys/vm/drop_caches
    else
      sudo -n /usr/sbin/sysctl -q vm.drop_caches=3
    fi ;;
  *)
    echo "drop-caches: unsupported platform $(uname -s)" >&2
    exit 1 ;;
esac
