#!/usr/bin/env bash
# The ten-task agent eval (P3-AGT-03); bench/README.md, "The agent eval",
# documents it (`bench/eval/run.sh --help` lists the subcommands). It runs
# Claude Code, which calls the model on the caller's account: only a person
# starts a real run. `--fake right|wrong` runs a stub that calls nothing.
#
# The interpreter is chosen as bench/run.sh chooses it: $PYTHON when set, else
# /usr/bin/python3 when it exists, else python3 from PATH.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
python=${PYTHON:-}
if [[ -z $python ]]; then
  if [[ -x /usr/bin/python3 ]]; then python=/usr/bin/python3; else python=python3; fi
fi
# -B writes no bytecode into the checkout; -E and -s keep PYTHON* variables and
# user site-packages out, so only the standard library and bench/eval/ are imported.
exec "$python" -B -E -s "$here/agent_eval.py" "$@"
