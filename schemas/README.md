# Schemas

The JSON Schema 2020-12 documents that duscope's machine output is checked
against, and the DDL of the index.

| Path | Contents | Added by |
| --- | --- | --- |
| `envelope/<verb>.schema.json` | the `--json` envelope of each verb | P2-CLI-01; until then `placeholder.schema.json` |
| `envelope/history/<schema_version>/` | every published minor of the envelope, for the compatibility tests | P2-CLI-01 |
| `mcp/<tool>.input.json`, `mcp/<tool>.output.json` | the input and output schema of each MCP tool | P3-MCP-04, P3-MCP-07 |
| `mcp/MAPPING.md` | per tool: the verb flags exposed, renamed and excluded | P3-MCP-04, P3-MCP-07 |
| `index/v2.sql` | the DDL of the index, `index_version` 2 | P2-IDX-01 |

## Rules

Every `.json` file here:

- declares `"$schema": "https://json-schema.org/draft/2020-12/schema"` and
  passes that meta-schema
- has an `$id` equal to its file name
- names its properties with `[A-Za-z0-9_.-]{1,64}`, the pattern MCP clients
  enforce
- keeps every `$ref` inside the file
- marks additions and deprecations with `x-since` and `x-deprecated`, whose
  value is a `schema_version` string

## Checks

- `duscope/tests/schemas.rs` applies the rules above to every file.
- `crates/duscope-core/src/schema_drift.rs` is the drift test. Each
  `envelope/*.schema.json` file must equal the schema `schemars` derives from
  its Rust type, with `x-*` keywords stripped from both. Each
  `mcp/duscope_<tool>.output.json` must equal its verb's envelope schema apart
  from `$id`, because a tool's `structuredContent` is byte-identical to
  `duscope <verb> --json`; the verb is the tool name with `-` for `_`
  (`duscope_by_type` is `by-type`). It is a unit test because the types derive
  `JsonSchema` only under `cfg(test)`, which keeps `schemars` out of the binary.
- `crates/duscope-core/tests/schema_drift.rs` checks that `schemars` and
  `jsonschema` stay dev-dependencies.

The `schemas` CI job runs all three, and `just test` includes them.

## Changing a schema

The files are maintained by hand. When a type changes, the drift test fails and
prints the schema derived from it. Copy the change into the file, keep the
file's `x-*` keywords, and review the diff. A new envelope file also gets an
entry in `derived_schemas()` in `schema_drift.rs`; a new MCP output file needs
none, since the test finds it and checks it against its verb's envelope.
