# Schemas

The JSON Schema 2020-12 documents that duscope's machine output is checked
against, and the DDL of the index.

| Path | Contents | Added by |
| --- | --- | --- |
| `envelope/<verb>.schema.json` | the `--json` envelope of each verb; `error.schema.json` for a failure, `spilled.schema.json` for the stub `--max-chars` prints | P2-CLI-01 |
| `envelope/history/<schema_version>/` | the envelope as the release that published that `schema_version` printed it, for the compatibility tests: `0.2/` alone until 1.0 | P2-CLI-01 |
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
- marks additions with `x-since` and deprecations with `x-deprecated`; an
  `x-since` value names a release of the binary, not a `schema_version`
  (below)

## The envelope's version

`schema_version` is `0.2` (`SCHEMA_VERSION` in
`crates/duscope-core/src/envelope.rs`), frozen by P2-CLI-01 and published with
v0.2.0, and it stays `0.2` until the 1.0.0 release makes it `1.0`. Until then
the envelope changes additively only (a new field, a new verb, a new value of a
field whose values grow), each change a deviation entry in the specification,
and no change moves `schema_version`.

An `x-since` value is the `major.minor` of the binary's release that first
prints the field, never a `schema_version`. The root of each file carries the
release that added it, `0.2` for every file v0.2.0 had; `"x-since": "0.3"`
marks what v0.3.0 is the first release to print, though its envelopes still
say `"schema_version": "0.2"`. So the binary's version (`duscope --version`)
tells a reader which marked fields to expect, and `schema_version` does not.
An addition is marked with the next minor release after the latest tag, the
first that can ship it, since a patch release carries fixes alone.

`history/0.2/` keeps the envelope as v0.2.0 printed it: each file as it stood
at the tag, apart from `clean.schema.json`, which v0.2.0 had and which has no
copy there. The copies never change: an addition edits the file in `envelope/`
alone. Before 1.0 no other copy is made, since `schema_version` does not move;
when it becomes `1.0`, the tests ask for `history/1.0/`.

`duscope schema --envelope <verb>` prints a verb's file; the binary carries a
copy of each (`duscope/src/verbs/schema/`), which a test holds equal to this
directory's.

The schemas list the values of the enumerated fields, from `WARNING_CODES`,
`ERROR_CODES` and `envelope::values` in `crates/duscope-core/src/envelope.rs`:

- a field whose values are fixed (`kind`, `size_kind`, `attrs`, `change`,
  `confidence`, `freeable_kind`, `fixable_by`) is a closed `enum`;
- a field whose values grow (the warning codes, the error codes, `flags`, the
  skip and exclude reasons, `regenerable_by`, what `scans --prune` removed) is
  `anyOf` its known values or any string, so a reader of one release's schema
  accepts a later release's output. A value a later release adds to a field
  that marks its values' releases (`fallback_reason`) is a branch of its own,
  `{"const": "<value>", "x-since": "<release>"}`, between the first release's
  values and any string.

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
  is a subset of its verb's schema now: nothing a published `schema_version`
  had is dropped or changes type, which is the additive rule.
- `duscope/tests/schemas.rs` also validates every golden envelope
  (`duscope/tests/golden/<verb>_json_*.snap`) against its verb's schema and
  against the verb's schema in every `history/` directory of the same major,
  and checks that every error and warning code the source writes is in the
  tables.

The `schemas` CI job runs all three, and `just test` includes them.

## Changing a schema

The files are maintained by hand. When a type changes, the drift test fails and
prints the schema derived from it. Copy the change into the file, keep the
file's `x-*` keywords, mark what the change adds with `x-since` (above), and
review the diff. A new envelope file also gets an entry in `derived_schemas()`
in `schema_drift.rs`; a new MCP output file needs none, since the test finds it
and checks it against its verb's envelope.
