# MCP tool mapping

How each MCP tool maps onto its verb: the verb's flags the tool exposes, the ones
it renames, and the ones it excludes, with the reason. `<tool>.input.json` beside
this file is the tool's hand-written input schema and `<tool>.output.json` its
output schema. A test compares each input schema with the clap definition of its
verb through this file, and fails on a flag that is neither exposed nor listed as
excluded.

Each package owns one section, so the two can be edited side by side.

## Scan group (P3-MCP-04)

`duscope_scan`, `duscope_scans`, `duscope_summary`, `duscope_top`,
`duscope_by_type`, `duscope_stale`: not yet written.

## Analysis group (P3-MCP-07)

`duscope_caches`, `duscope_explain`, `duscope_query`, `duscope_schema`,
`duscope_diff`: not yet written.
