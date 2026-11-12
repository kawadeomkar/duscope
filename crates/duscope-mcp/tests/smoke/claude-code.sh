#!/usr/bin/env bash
# P3-MCP-06: the Claude Code smoke test. Claude Code registers `duscope mcp`
# with the command its documentation gives for a local stdio server
# (https://code.claude.com/docs/en/mcp, "Add a local stdio server"; the
# default scope, local, keyed to the working directory), then makes the
# three tool calls in print mode, with those three tools allowed and no
# other. Reads ANTHROPIC_API_KEY. See common.sh for the options and the log.

# The version the smoke test installs (npm @anthropic-ai/claude-code); a
# change of it is a change to this file.
CLAUDE_CODE_VERSION=2.1.319

here=$(cd "$(dirname "$0")" && pwd)
# shellcheck source-path=SCRIPTDIR source=common.sh
. "$here/common.sh"

smoke_init "Claude Code" claude-code "$@"
need_key ANTHROPIC_API_KEY
install_client @anthropic-ai/claude-code "$CLAUDE_CODE_VERSION" claude

section "Registration"
step claude mcp add --transport stdio duscope -- duscope mcp --roots "$TREE" ||
    fail "claude mcp add failed"
step claude mcp list || fail "claude mcp list failed"

section "Prompt"
printf '%s\n' "$PROMPT" >>"$LOG"

allowed=mcp__duscope__duscope_scan,mcp__duscope__duscope_top,mcp__duscope__duscope_summary
ask claude -p "$PROMPT" --output-format json --max-turns 10 --allowedTools "$allowed" ||
    fail "claude -p failed"
# The JSON result holds the final answer and the usage, not the tools'
# results: an answer naming the directory shows the client read them.
((DRY_RUN)) || cp "$CLIENT_OUT" "$ANSWER"

# The delete phase: the same server with --allow-delete, its one extra tool
# not allowed, so print mode is expected not to grant it, and the output's
# permission_denials to record the prompt left unanswered (unobserved until
# the first real run).
section "Delete registration"
step claude mcp add --transport stdio duscope-delete -- duscope mcp --roots "$TREE" --allow-delete ||
    fail "claude mcp add failed"
ask_delete claude -p "$DELETE_PROMPT" --output-format json --max-turns 6 \
    --allowedTools mcp__duscope-delete__duscope_explain
verify_delete
verify
