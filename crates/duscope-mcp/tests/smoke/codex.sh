#!/usr/bin/env bash
# P3-MCP-06: the Codex CLI smoke test. Codex registers `duscope mcp` with
# the command its documentation gives for a stdio server
# (https://developers.openai.com/codex/mcp: `codex mcp add <name> --
# <command>`, which writes [mcp_servers.duscope] to ~/.codex/config.toml),
# then makes the three tool calls with `codex exec`. A non-interactive run
# answers no approval prompt, so the run approves the server's tools
# (`default_tools_approval_mode`) and requires the server to start, as -c
# overrides of the registered entry. Reads OPENAI_API_KEY, which reaches
# Codex as CODEX_API_KEY for that one command. See common.sh for the options
# and the log.

# The version the smoke test installs (npm @openai/codex); a change of it is
# a change to this file.
CODEX_VERSION=0.167.0

here=$(cd "$(dirname "$0")" && pwd)
# shellcheck source-path=SCRIPTDIR source=common.sh
. "$here/common.sh"

smoke_init "Codex CLI" codex "$@"
need_key OPENAI_API_KEY
install_client @openai/codex "$CODEX_VERSION" codex

section "Registration"
step codex mcp add duscope -- duscope mcp --roots "$TREE" || fail "codex mcp add failed"
step codex mcp list || fail "codex mcp list failed"

section "Prompt"
printf '%s\n' "$PROMPT" >>"$LOG"

# The key is exported in a subshell for exec alone, so the log shows the
# command without it.
printf 'CODEX_API_KEY is set from OPENAI_API_KEY for this command alone.\n' >>"$LOG"
# Called through ask.
# shellcheck disable=SC2329
codex_exec() {
    (
        export CODEX_API_KEY=$OPENAI_API_KEY
        "$@"
    )
}
ask codex_exec codex exec --skip-git-repo-check --ephemeral --json --output-last-message "$ANSWER" \
    -c 'mcp_servers.duscope.default_tools_approval_mode="approve"' \
    -c 'mcp_servers.duscope.required=true' \
    "$PROMPT" || fail "codex exec failed"
verify
