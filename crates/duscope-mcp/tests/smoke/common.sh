# shellcheck shell=bash
# P3-MCP-06: what the three client smoke scripts share. Sourced by
# claude-code.sh, codex.sh and gemini.sh, never run on its own.
#
# Each script registers `duscope mcp` in one client with the command that
# client's documentation gives, asks it for three tool calls (duscope_scan,
# duscope_top, duscope_summary) on a small tree, and checks that the three
# reached the binary and were answered. It writes what happened to
# <log dir>/<client>.log: the versions, the registration and the client's
# listing of it, the prompt, the client's output, and the binary's own log.
#
# Everything a client writes stays in a temporary directory: HOME and the
# XDG directories point into it, so a run never reads or changes the real
# home's configuration, and the client is installed there from npm at the
# version its script pins. `duscope` on the client's PATH is a shim that runs
# the binary given with --duscope as the client asked, adding only
# `--log-level debug` with the binary's log going to a file: the registered
# command line is the documented one, and the log shows what the binary
# received and answered.
#
# Options (each script):
#   --duscope PATH  the binary to register (default: duscope on PATH)
#   --log-dir DIR   where the log goes (default: this directory)
#   --dry-run       install and run no client and call no vendor: print the
#                   commands, then play the client by sending the binary the
#                   three calls the prompt asks for, and check them the same
#                   way; the log is <client>.dry-run.log
#
# Exit status: 0 when the three calls were answered and the answer names the
# largest directory; 1 when anything failed; 2 for a usage error or a
# missing API key, which stops the script before it installs anything.

set -o pipefail

smoke_here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

# The three calls, in the order the prompt asks for them, by the verb each
# runs.
smoke_verbs="scan top summary"

# smoke_init CLIENT NAME ARGS...: parses the options and sets up the
# temporary home, the tree, the shim and the log.
smoke_init() {
    SMOKE_CLIENT=$1
    smoke_name=$2
    shift 2
    DRY_RUN=0
    smoke_duscope=
    smoke_log_dir=$smoke_here
    while (($#)); do
        case $1 in
            --dry-run) DRY_RUN=1 ;;
            --duscope)
                (($# >= 2)) || smoke_usage "--duscope needs a path"
                smoke_duscope=$2
                shift
                ;;
            --log-dir)
                (($# >= 2)) || smoke_usage "--log-dir needs a directory"
                smoke_log_dir=$2
                shift
                ;;
            *) smoke_usage "unknown argument: $1" ;;
        esac
        shift
    done
    if [[ -z $smoke_duscope ]]; then
        smoke_duscope=$(command -v duscope) || smoke_usage "no duscope on PATH: pass --duscope PATH"
    fi
    [[ -x $smoke_duscope ]] || smoke_usage "$smoke_duscope is not an executable"
    smoke_duscope=$(cd "$(dirname "$smoke_duscope")" && pwd)/$(basename "$smoke_duscope")
    mkdir -p "$smoke_log_dir" || smoke_usage "cannot create $smoke_log_dir"
    smoke_log_dir=$(cd "$smoke_log_dir" && pwd)
    if ((DRY_RUN)); then
        LOG=$smoke_log_dir/$smoke_name.dry-run.log
    else
        LOG=$smoke_log_dir/$smoke_name.log
    fi

    WORK=$(mktemp -d "${TMPDIR:-/tmp}/duscope-smoke.XXXXXX") || exit 1
    trap smoke_exit EXIT
    export HOME=$WORK/home
    export XDG_CONFIG_HOME=$HOME/.config XDG_CACHE_HOME=$HOME/.cache
    export XDG_DATA_HOME=$HOME/.local/share XDG_STATE_HOME=$HOME/.local/state
    mkdir -p "$HOME" "$XDG_CONFIG_HOME" "$XDG_CACHE_HOME" "$XDG_DATA_HOME" "$XDG_STATE_HOME" \
        "$WORK/bin" "$WORK/project"

    # The tree the calls read: one directory far larger than the rest.
    TREE=$WORK/tree
    mkdir -p "$TREE/largest-dir/sub" "$TREE/small"
    head -c 300000 /dev/zero >"$TREE/largest-dir/sub/blob"
    head -c 100000 /dev/zero >"$TREE/largest-dir/notes.txt"
    printf 'x\n' >"$TREE/small/file"

    SERVER_LOG=$WORK/server.log
    : >"$SERVER_LOG"
    local real log
    printf -v real '%q' "$smoke_duscope"
    printf -v log '%q' "$SERVER_LOG"
    cat >"$WORK/bin/duscope" <<EOF
#!/usr/bin/env bash
# The duscope the client starts: the binary under test, run as the client
# asked; \`duscope mcp\` also logs at debug level to the smoke test's file.
if [[ \${1:-} == mcp ]]; then
    printf 'argv: %s\n' "\$*" >>$log
    exec $real "\$@" --log-level debug 2>>$log
fi
exec $real "\$@"
EOF
    chmod +x "$WORK/bin/duscope"
    export PATH=$WORK/bin:$PATH
    cd "$WORK/project" || exit 1

    # Read by the scripts that source this file.
    # shellcheck disable=SC2034
    PROMPT="Use the tools of the MCP server named duscope, and no other tools. Make exactly \
three tool calls, one after another: first duscope_scan with root set to $TREE, then \
duscope_top with limit set to 3, then duscope_summary with no arguments. Then answer in one \
line with the path of the largest directory duscope_top listed."
    ANSWER=$WORK/answer.txt
    CLIENT_OUT=$WORK/client.out
    : >"$ANSWER"
    : >"$CLIENT_OUT"
    smoke_secrets=
    smoke_done=0

    {
        printf '# duscope MCP smoke test: %s\n\n' "$SMOKE_CLIENT"
        if ((DRY_RUN)); then
            printf 'DRY RUN: no client was installed or run, and no vendor was called.\n\n'
        fi
        printf 'date: %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
        printf 'duscope: %s\n' "$("$smoke_duscope" --version 2>&1)"
        printf 'platform: %s\n' "$(uname -srm)"
        printf 'node: %s\n' "$(node --version 2>/dev/null || echo none)"
        printf 'tree: %s\n' "$TREE"
    } >"$LOG"
}

smoke_usage() {
    printf 'mcp-smoke: %s\n' "$1" >&2
    printf 'usage: %s [--dry-run] [--duscope PATH] [--log-dir DIR]\n' "$(basename "$0")" >&2
    exit 2
}

# need_key NAME: stops the script, before anything is installed, unless the
# environment holds the API key NAME. A dry run needs none.
need_key() {
    local name=$1
    smoke_secrets="$smoke_secrets $name"
    ((DRY_RUN)) && return 0
    if [[ -z ${!name:-} ]]; then
        local message="$name is not set: $SMOKE_CLIENT cannot run without it (the repository secret of that name, docs/RELEASING.md)"
        [[ ${GITHUB_ACTIONS:-} == true ]] && printf '::error title=mcp-smoke::%s\n' "$message"
        printf 'mcp-smoke: %s\n' "$message" >&2
        printf '\nFAIL: %s\n' "$message" >>"$LOG"
        smoke_done=1
        exit 2
    fi
}

# section TITLE: starts a section of the log.
section() {
    printf '\n## %s\n\n' "$1" >>"$LOG"
}

# shown ARGS...: the command line ARGS, quoted as a shell would read it.
shown() {
    local word line=
    for word in "$@"; do
        printf -v word '%q' "$word"
        line="$line${line:+ }$word"
    done
    printf '%s' "$line"
}

# step ARGS...: logs the command ARGS and, unless this is a dry run, runs it
# with its output in the log: its status.
step() {
    printf '$ %s\n' "$(shown "$@")" >>"$LOG"
    ((DRY_RUN)) && return 0
    "$@" >>"$LOG" 2>&1
}

# ask ARGS...: logs the client's command line ARGS and runs it, its output
# in CLIENT_OUT and then in the log: its status. A dry run plays the client
# instead (play_client).
ask() {
    section "Client"
    printf '$ %s\n' "$(shown "$@")" >>"$LOG"
    if ((DRY_RUN)); then
        play_client
        return 0
    fi
    local status=0
    "$@" >"$CLIENT_OUT" 2>>"$LOG" || status=$?
    cat "$CLIENT_OUT" >>"$LOG"
    return "$status"
}

# install_client PACKAGE VERSION COMMAND: installs the client from npm into
# the temporary directory and puts COMMAND on PATH.
install_client() {
    section "Install"
    step npm install --global --prefix "$WORK/npm" --no-audit --no-fund "$1@$2" ||
        fail "npm could not install $1@$2"
    export PATH=$WORK/npm/bin:$PATH
    if ((!DRY_RUN)); then
        step "$3" --version || fail "$3 --version failed"
    fi
}

# play_client: in a dry run, what the client would do: start the command the
# registration names, as the client finds it on PATH, and send it the three
# calls the prompt asks for. The answer is the largest directory duscope_top
# listed.
play_client() {
    local meta='"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}'
    local out=$WORK/played.jsonl
    {
        printf '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{%s,"name":"duscope_scan","arguments":{"root":"%s"}}}\n' "$meta" "$TREE"
        printf '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{%s,"name":"duscope_top","arguments":{"limit":3}}}\n' "$meta"
        printf '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{%s,"name":"duscope_summary","arguments":{}}}\n' "$meta"
    } | duscope mcp --roots "$TREE" >"$out"
    printf '(the dry run sends the binary the three calls in place of the client)\n' >>"$LOG"
    cat "$out" >>"$LOG"
    grep -oF "$TREE/largest-dir" "$out" | head -n 1 >"$ANSWER"
}

# fail WHY: ends the script with WHY as its result.
fail() {
    printf '\nFAIL: %s\n' "$1" >>"$LOG"
    printf 'mcp-smoke: %s: FAIL: %s (log: %s)\n' "$SMOKE_CLIENT" "$1" "$LOG" >&2
    smoke_done=1
    exit 1
}

# verify: checks the binary's log and the answer, and ends the script.
verify() {
    section "The binary's log (duscope mcp --log-level debug)"
    cat "$SERVER_LOG" >>"$LOG"
    section "Answer"
    cat "$ANSWER" >>"$LOG"
    printf '\n' >>"$LOG"
    grep -qxF "argv: mcp --roots $TREE" "$SERVER_LOG" ||
        fail "the client never started duscope mcp --roots $TREE"
    local verb
    for verb in $smoke_verbs; do
        answered "$verb" || fail "no call ran duscope $verb and was answered without an error"
    done
    grep -qF "$TREE/largest-dir" "$ANSWER" ||
        fail "the answer does not name $TREE/largest-dir"
    printf '\nPASS: %s started duscope mcp, and duscope_scan, duscope_top and duscope_summary were each answered\n' \
        "$SMOKE_CLIENT" >>"$LOG"
    printf 'mcp-smoke: %s: PASS (log: %s)\n' "$SMOKE_CLIENT" "$LOG"
    smoke_done=1
    exit 0
}

# answered VERB: whether the binary's log shows a tool call that ran VERB and
# was answered without an error. The calls run one at a time, so each
# answer follows the verb it ran.
answered() {
    awk -v want="$1" '
        / running [a-z-]+ for a tool$/ { verb = $0; sub(/.* running /, "", verb); sub(/ for a tool$/, "", verb); next }
        / tools\/call [^ ]+ answered with an error$/ { verb = ""; next }
        / tools\/call [^ ]+ answered$/ { if (verb == want) found = 1; verb = ""; next }
        END { exit !found }
    ' "$SERVER_LOG"
}

# On exit: a script that ended early fails, and every API key it read is
# taken out of the log before the temporary directory goes.
smoke_exit() {
    local status=$?
    if ((!smoke_done)); then
        printf '\nFAIL: the script stopped early (status %s)\n' "$status" >>"$LOG"
    fi
    local name value content
    for name in $smoke_secrets; do
        value=${!name:-}
        if [[ -n $value && -f $LOG ]]; then
            content=$(<"$LOG")
            printf '%s\n' "${content//"$value"/[$name]}" >"$LOG"
        fi
    done
    cd / && rm -rf "$WORK"
}
