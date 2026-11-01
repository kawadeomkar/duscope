# Schemas

The JSON Schema 2020-12 documents that duscope's machine output is checked
against, and the DDL of the index.

| Path | Contents | Added by |
| --- | --- | --- |
| `envelope/<verb>.schema.json` | the `--json` envelope of each verb; `error.schema.json` for a failure, `spilled.schema.json` for the stub `--max-chars` prints | P2-CLI-01 |
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

## The envelope's version

`schema_version` is `0.2`, frozen by P2-CLI-01. From there the envelope changes
additively only (a new field, a new verb, a new value of a field whose values
grow), each change a deviation entry in the specification, its fields marked
`x-since` with the minor that adds them. `history/<minor>/` keeps each minor as
published; `history/0.2/` is the minor in progress until v0.2.0 is tagged, so a
package before then that adds to the envelope updates it as well.

The schemas list the values of the enumerated fields, from `WARNING_CODES`,
`ERROR_CODES` and `envelope::values` in `crates/duscope-core/src/envelope.rs`:

- a field whose values are fixed (`kind`, `size_kind`, `attrs`, `change`,
  `confidence`, `freeable_kind`, `fixable_by`) is a closed `enum`;
- a field whose values grow (the warning codes, the error codes, `flags`, the
  skip and exclude reasons, `regenerable_by`, what `scans --prune` removed) is
  `anyOf` its known values or any string, so a reader of one minor accepts the
  next one's output.

A field only one platform fills is marked `x-platforms`.

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
  `jsonschema` stay dev-dependencies, and that every schema under `history/`
  is a subset of its verb's schema now: nothing a published minor had is
  dropped or changes type.
- `duscope/tests/schemas.rs` also validates every golden envelope
  (`duscope/tests/golden/<verb>_json_*.snap`) against its verb's schema and
  against the verb's schema in every published minor of the same major, and
  checks that every error and warning code the source writes is in the tables.

The `schemas` CI job runs all three, and `just test` includes them.

## Changing a schema

The files are maintained by hand. When a type changes, the drift test fails and
prints the schema derived from it. Copy the change into the file, keep the
file's `x-*` keywords, and review the diff. A new envelope file also gets an
entry in `derived_schemas()` in `schema_drift.rs`; a new MCP output file needs
none, since the test finds it and checks it against its verb's envelope.
