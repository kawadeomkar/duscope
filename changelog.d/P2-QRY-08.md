### Added
- `schema [path]` prints the index's layout for `query --sql`: each table with its columns, their types, whether they may be null and the platforms that fill them, and its rows in the newest saved scan that holds the path (or the one `--scan` names). People get the DDL with each table's rows; `--json` gets `tables[] {name, columns[] {name, type, nullable, platforms}, rows}`.
- `schema --envelope <verb>` prints that verb's envelope as JSON Schema 2020-12; `schema --envelope` alone lists the envelopes.
