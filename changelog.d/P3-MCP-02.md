### Changed
- `duscope mcp` answers a `ping` that names protocol version 2026-07-28 in its `_meta` with `-32601` (method not found), since that revision has no `ping`. A `ping` in a 2025-11-25 session, or ahead of its `initialize`, is answered as before.
### Fixed
- `duscope mcp`'s `instructions` end with a whole line of the agent briefing, the last that fits in 4,096 characters, where they stopped at the 4,096th character, inside a row of the briefing's table.
