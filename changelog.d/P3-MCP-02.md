### Changed
- `duscope mcp` answers a `ping` that names protocol version 2026-07-28 in its `_meta` with `-32601` (method not found), since that revision has no `ping`. A `ping` in a 2025-11-25 session, or ahead of its `initialize`, is answered as before.
