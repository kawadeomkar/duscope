#!/usr/bin/env bash
# Drop the filesystem caches before a cold run: the harness passes this script
# to hyperfine's --prepare. It needs root, or passwordless sudo for exactly the
# command it runs (bench/README.md, "Cold runs"):
#   macOS  /usr/sbin/purge
#   Linux  /usr/sbin/sysctl -q vm.drop_caches=3   (the same as echo 3 > /proc/sys/vm/drop_caches)
# sudo runs with -n, so a missing rule fails at once instead of asking for a password.
#
# macOS, with the trees on a disk image:
#   drop-caches.sh --remount IMAGE MOUNTPOINT
# detaches IMAGE from MOUNTPOINT (when attached), purges, and attaches it there
# again. purge drops the unified buffer cache but not APFS's metadata caches;
# detaching the image's volume drops those, and the purge in between drops the
# image's own blocks from the cache of the volume it is stored on. hdiutil needs
# no root for an image its user owns.
set -euo pipefail

usage() { echo "usage: drop-caches.sh [--remount IMAGE MOUNTPOINT]" >&2; exit 2; }
image="" mountpoint=""
if [[ ${1:-} == --remount ]]; then
  [[ $# -eq 3 ]] || usage
  image=$2 mountpoint=$3
elif [[ $# -gt 0 ]]; then
  usage
fi

sync
case "$(uname -s)" in
  Darwin)
    purge() { if [[ $(id -u) -eq 0 ]]; then /usr/sbin/purge; else sudo -n /usr/sbin/purge; fi; }
    if [[ -z $image ]]; then
      purge
      exit 0
    fi
    mkdir -p "$mountpoint"
    if [[ $(mount) == *" on $(cd "$mountpoint" && pwd -P) ("* ]]; then
      hdiutil detach -quiet "$mountpoint"
    fi
    purge
    hdiutil attach -quiet -nobrowse -noverify -noautoopen -mountpoint "$mountpoint" "$image" ;;
  Linux)
    if [[ -n $image ]]; then
      echo "drop-caches: --remount is for a macOS disk image; Linux drops every cache" >&2
      exit 2
    fi
    if [[ $(id -u) -eq 0 ]]; then
      echo 3 > /proc/sys/vm/drop_caches
    else
      sudo -n /usr/sbin/sysctl -q vm.drop_caches=3
    fi ;;
  *)
    echo "drop-caches: unsupported platform $(uname -s)" >&2
    exit 1 ;;
esac
