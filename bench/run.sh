#!/usr/bin/env bash
# duscope's benchmark harness; `just bench` runs it. bench/README.md documents
# the subcommands and options (`bench/run.sh --help` lists them).
#
# The harness is Python 3.9+ standard library only. The interpreter is $PYTHON
# when set, else /usr/bin/python3 when it exists (the system Python on macOS and
# Linux), else python3 from PATH.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
python=${PYTHON:-}
if [[ -z $python ]]; then
  if [[ -x /usr/bin/python3 ]]; then python=/usr/bin/python3; else python=python3; fi
fi
# -B writes no bytecode into the checkout; -E and -s keep PYTHON* variables and
# user site-packages out, so only the standard library and bench/ are imported.
exec "$python" -B -E -s "$here/harness.py" "$@"
