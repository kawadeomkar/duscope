//! `duscope_scans`: the saved scans of the roots and of the directories
//! under them (`scans ROOT...`).

use serde_json::{Map, Value};

use super::{Annotations, CommandLine, Context, Invocation, READ_ONLY, Tool};
use crate::roots::Refusal;

/// `scans ROOT...`, every root the server has.
pub struct Scans;

impl Tool for Scans {
    fn name(&self) -> &'static str {
        "duscope_scans"
    }

    fn description(&self) -> &'static str {
        "The saved scans of directories under the roots, newest first: each one's scan_id, \
         root, times, attributes, entry count and totals."
    }

    fn input_schema(&self) -> &'static str {
        include_str!("schemas/duscope_scans.input.json")
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
        for root in context.roots.paths() {
            line.push(root);
        }
        Ok(line.invocation("scans"))
    }

    fn summary(&self, envelope: &Value) -> String {
        let total = &envelope["total_count"];
        let newest = &envelope["entries"][0];
        match (newest["id"].as_str(), newest["root"].as_str()) {
            (Some(id), Some(root)) => {
                format!("{total} saved scans under the roots; the newest is {id}, of {root}")
            }
            _ => "No saved scans under the roots: duscope_scan saves one".to_owned(),
        }
    }
}
