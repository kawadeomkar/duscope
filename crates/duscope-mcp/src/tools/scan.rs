//! `duscope_scan`: scans a directory under the roots and saves the scan
//! (`scan --save`), or returns a saved scan of it younger than `max_age`
//! (`--reuse`).

use std::time::Duration;

use serde_json::{Map, Value};

use super::{Annotations, CommandLine, Context, Invocation, Tool, path_argument};
use crate::roots::Refusal;
use crate::worker;

/// `scan --save [--attrs SET] [--clones | --no-clones] [--one-file-system]
/// [--reuse AGE] ROOT`.
pub struct Scan;

impl Tool for Scan {
    fn name(&self) -> &'static str {
        "duscope_scan"
    }

    fn description(&self) -> &'static str {
        "Scan a directory under the roots and save the scan: its scan_id, totals and largest \
         directories. The other tools read saved scans, so call this first; with max_age, a \
         recent scan of the directory is returned instead of scanning again."
    }

    fn input_schema(&self) -> &'static str {
        include_str!("schemas/duscope_scan.input.json")
    }

    fn verb(&self) -> Option<&'static str> {
        Some("scan")
    }

    fn annotations(&self) -> Annotations {
        // Each call writes a new index, unless max_age finds one.
        Annotations {
            read_only: false,
            destructive: false,
            idempotent: false,
        }
    }

    fn deadline(&self) -> Duration {
        worker::SCAN_DEADLINE
    }

    fn invocation(
        &self,
        arguments: &Map<String, Value>,
        context: &Context<'_>,
    ) -> Result<Invocation, Refusal> {
        let root = path_argument(arguments, "root", None, context.roots)?;
        let mut line = CommandLine::new(arguments);
        line.push("--save").value("attrs", "--attrs");
        // Either way the flag is given, over a configuration's default.
        match arguments.get("clones").and_then(Value::as_bool) {
            Some(true) => {
                line.push("--clones");
            }
            Some(false) => {
                line.push("--no-clones");
            }
            None => {}
        }
        line.switch("one_file_system", "--one-file-system")
            .value("max_age", "--reuse")
            .push(root);
        Ok(line.invocation("scan"))
    }

    fn summary(&self, envelope: &Value) -> String {
        let scan = &envelope["scan"];
        let id = scan["id"].as_str().unwrap_or("?");
        format!(
            "Saved scan {id} of {}: {} bytes on disk in {} files and {} directories; pass \
             scan_id {id} to the other tools",
            scan["root"].as_str().unwrap_or("?"),
            scan["total_allocated_bytes"],
            scan["file_count"],
            scan["dir_count"]
        )
    }
}
