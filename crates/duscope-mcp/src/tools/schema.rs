//! `duscope_schema`: the index's tables and columns, with each table's
//! rows in a saved scan (`schema --scan`), or a verb's envelope as JSON
//! Schema (`schema --envelope`). The rows count the whole scan, so the scan
//! is chosen as `duscope_query` chooses it.

use serde_json::{Map, Value};

use super::query::whole_scan;
use super::{Annotations, CommandLine, Context, Invocation, READ_ONLY, Tool};
use crate::roots::Refusal;

/// `schema --scan ID PATH`, or `schema --envelope VERB`.
pub struct Schema;

impl Tool for Schema {
    fn name(&self) -> &'static str {
        "duscope_schema"
    }

    fn description(&self) -> &'static str {
        "The index's tables and columns, for duscope_query, with each table's rows in a saved \
         scan; or, with envelope, a verb's output as JSON Schema."
    }

    fn input_schema(&self) -> &'static str {
        include_str!("schemas/duscope_schema.input.json")
    }

    fn verb(&self) -> Option<&'static str> {
        Some("schema")
    }

    fn annotations(&self) -> Annotations {
        READ_ONLY
    }

    fn invocation(
        &self,
        arguments: &Map<String, Value>,
        context: &Context<'_>,
    ) -> Result<Invocation, Refusal> {
        let mut line = CommandLine::new(arguments);
        // An envelope's schema reads no scan.
        if arguments.contains_key("envelope") {
            line.value("envelope", "--envelope");
            return Ok(line.invocation("schema"));
        }
        let (id, path) = whole_scan(self.name(), arguments, context)?;
        line.push("--scan").push(id).push(path);
        Ok(line.invocation("schema"))
    }

    fn summary(&self, envelope: &Value) -> String {
        // `--envelope` prints the JSON Schema itself.
        if let Some(id) = envelope["$id"].as_str() {
            return format!("The JSON Schema {id}");
        }
        let result = &envelope["result"];
        format!(
            "{} tables of index version {}, with their rows in scan {}",
            result["tables"].as_array().map_or(0, Vec::len),
            result["index_version"].as_str().unwrap_or("?"),
            envelope["scan"]["id"].as_str().unwrap_or("?")
        )
    }
}
