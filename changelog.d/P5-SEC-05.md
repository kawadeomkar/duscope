### Fixed
- `duscope mcp` no longer grows without limit when a client sends requests faster than they finish: at most 64 requests, holding 32 MiB between them once parsed, wait behind the running call, and a request past either is answered at once with the JSON-RPC error `-31000` and never runs. Cancellations, `ping`, `tools/list` and `server/discover` are still handled at once.
- `duscope mcp` refuses a path longer than 4096 bytes (`refused_path`) before resolving it, and starts a call's deadline before its arguments are checked, so a call whose checks are slow is answered `deadline_exceeded` on time.
- Messages and logs on stderr, clap's usage errors included, show control characters as escapes (`\x1b`), as tables do, so that a file name or argument cannot drive the terminal.
