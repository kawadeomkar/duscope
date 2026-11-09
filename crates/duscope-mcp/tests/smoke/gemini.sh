#!/usr/bin/env bash
# P3-MCP-06: the Gemini CLI smoke test. Gemini registers `duscope mcp` with
# the command its documentation gives for a stdio server
# (https://github.com/google-gemini/gemini-cli/blob/main/docs/tools/mcp-server.md:
# `gemini mcp add [options] <name> <command> [args...]`, the server's own
# dashed arguments after `--`; user scope, ~/.gemini/settings.json), then
# makes the three tool calls in headless mode. The temporary working
# directory is not a trusted folder, where Gemini starts no stdio server, so
# the run trusts it for the session (--skip-trust), and approves tool calls
# (--approval-mode=yolo) with duscope the only MCP server allowed. Reads
# GEMINI_API_KEY. See common.sh for the options and the log.

# The version the smoke test installs (npm @google/gemini-cli); a change of
# it is a change to this file.
GEMINI_CLI_VERSION=0.67.0

here=$(cd "$(dirname "$0")" && pwd)
# shellcheck source-path=SCRIPTDIR source=common.sh
. "$here/common.sh"

smoke_init "Gemini CLI" gemini "$@"
need_key GEMINI_API_KEY
install_client @google/gemini-cli "$GEMINI_CLI_VERSION" gemini

section "Registration"
step gemini mcp add --scope user duscope duscope mcp -- --roots "$TREE" ||
    fail "gemini mcp add failed"
step gemini mcp list || fail "gemini mcp list failed"

section "Prompt"
printf '%s\n' "$PROMPT" >>"$LOG"

ask gemini --skip-trust --approval-mode=yolo --allowed-mcp-server-names duscope \
    --output-format json -p "$PROMPT" || fail "gemini -p failed"
# The JSON holds the final answer and the statistics, not the tools'
# results: an answer naming the directory shows the client read them.
((DRY_RUN)) || cp "$CLIENT_OUT" "$ANSWER"

# The delete phase: the server registered a second time with --allow-delete,
# and the run without --approval-mode=yolo, so headless Gemini does not
# approve the destructive call; its output is that prompt left unanswered.
section "Delete registration"
step gemini mcp add --scope user duscope-delete duscope mcp -- --roots "$TREE" --allow-delete ||
    fail "gemini mcp add failed"
ask_delete gemini --skip-trust --allowed-mcp-server-names duscope-delete \
    --output-format json -p "$DELETE_PROMPT"
verify_delete
verify
