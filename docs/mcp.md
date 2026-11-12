# duscope over MCP

`duscope mcp` is a [Model Context Protocol](https://modelcontextprotocol.io)
server on stdin and stdout: an AI client can scan a directory and ask what is in
it. It answers from the same saved scans, in the same JSON envelope, as the
command line, and, like the rest of the binary, opens no network socket.

Install the binary first ([README](../README.md#install)). The tools below
arrive with v0.3; v0.2.0's `duscope mcp` lists none.

## The `--roots` rule

`--roots` is required, and every recipe below passes it. It names the
directories the client may reach, and it can be given several:
`--roots ~/dev ~/Downloads`. The recipes use `~`; narrower is safer.

- A path outside the roots is refused with `refused_path`, as is any path
  holding `..`.
- A path is resolved as the file system resolves it before it is compared with
  the roots, so a link inside a root that leads out of it is refused too.
- A saved scan of a directory under a root is visible as it is; one of a
  directory above a root is read at that root; any other is invisible.

## Register it

Each client takes one line.

### Claude Code

```sh
claude mcp add --transport stdio duscope -- duscope mcp --roots ~
```

### Codex CLI

```sh
codex mcp add duscope -- duscope mcp --roots ~
```

This writes `[mcp_servers.duscope]` to `~/.codex/config.toml`.

### Gemini CLI

```sh
gemini mcp add --scope user duscope duscope mcp -- --roots ~
```

## The tools

The server lists them in this order; a test holds this list to its `tools/list`.

- `duscope_by_type`: The bytes under a path in a saved scan by kind of file (video, archives, code, …), largest first, with each kind's largest files.
- `duscope_caches`: The regenerable caches under a path in a saved scan (node_modules, target, .venv, build caches, …), largest first, with what regenerates each, or one row per project; paged with cursor.
- `duscope_diff`: What changed between two saved scans of one tree, under a path: the entries added, removed, and grown or shrunk, largest change first, with the totals.
- `duscope_explain`: The facts about one file or directory: its kind and sizes, the bytes deleting it would free now (hard links and clones that keep data elsewhere left out), the cache rule it matches, and whether git ignores it; from a saved scan, or the path as it is now.
- `duscope_query`: One read-only SQL SELECT over a saved scan's tables, which duscope_schema lists, in the command line's sandbox: 25 rows without a LIMIT, 10000 at most, 5 seconds. Reads a scan of a root or of a directory under one.
- `duscope_scan`: Scan a directory under the roots and save the scan: its scan_id, totals and largest directories. The other tools read saved scans, so call this first; with max_age, a recent scan of the directory is returned instead of scanning again.
- `duscope_scans`: The saved scans of directories under the roots, newest first: each one's scan_id, root, times, attributes, entry count and totals.
- `duscope_schema`: The index's tables and columns, for duscope_query, with each table's rows in a saved scan; or, with envelope, a verb's output as JSON Schema.
- `duscope_stale`: Large files under a path in a saved scan left unmodified for a long time (by default 10 MB and 180 days), largest first; paged with cursor.
- `duscope_summary`: Totals under a path in a saved scan: bytes on disk and apparent, files, directories, errors, the volumes, and the bytes no entry accounts for.
- `duscope_top`: The largest directories, or files, under a path in a saved scan, largest first; narrowed by depth, size and glob, and paged with cursor.

## Ask before `duscope clean`

The MCP tools above read and scan. An agent with a shell can still run
`duscope clean --yes` itself. This [Claude Code
hook](https://code.claude.com/docs/en/hooks) asks you first whenever a Bash
command starts with `duscope clean`. It goes in `~/.claude/settings.json`, or
in a project's `.claude/settings.json`:

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Bash",
        "hooks": [
          {
            "type": "command",
            "if": "Bash(duscope clean *)",
            "command": "echo '{\"hookSpecificOutput\":{\"hookEventName\":\"PreToolUse\",\"permissionDecision\":\"ask\",\"permissionDecisionReason\":\"duscope clean removes files\"}}'"
          }
        ]
      }
    ]
  }
}
```

The hook matches the command's text, so it is a prompt, not a guarantee: it
does not see `duscope clean` behind a global flag (`duscope -q clean …`), a
pipe, a script or another name for the binary. An agent that must never delete
files should not have a shell that can run `duscope clean --yes` at all.

## The registry manifest

[`server.json`](../server.json) describes the server to the
[MCP Registry](https://registry.modelcontextprotocol.io) under the name
`io.github.kawadeomkar/duscope`: the `duscope` crate from crates.io, run as
`duscope mcp --roots <DIR>` over stdio. The server is not listed in the registry
yet.
