//! `duscope_explain`: the facts about one file or directory (`explain`),
//! from a saved scan, or the path as it is now.

use serde_json::{Map, Value};

use super::{
    Annotations, CommandLine, Context, Invocation, READ_ONLY, Tool, path_argument, scan_argument,
};
use crate::roots::Refusal;

/// `explain [--scan ID] PATH`.
pub struct Explain;

impl Tool for Explain {
    fn name(&self) -> &'static str {
        "duscope_explain"
    }

    fn description(&self) -> &'static str {
        "The facts about one file or directory: its kind and sizes, the bytes deleting it would \
         free now (hard links and clones that keep data elsewhere left out), the cache rule it \
         matches, and whether git ignores it; from a saved scan, or the path as it is now."
    }

    fn input_schema(&self) -> &'static str {
        include_str!("schemas/duscope_explain.input.json")
    }

    fn verb(&self) -> Option<&'static str> {
        Some("explain")
    }

    fn annotations(&self) -> Annotations {
        READ_ONLY
    }

    fn invocation(
        &self,
        arguments: &Map<String, Value>,
        context: &Context<'_>,
    ) -> Result<Invocation, Refusal> {
        let scan = scan_argument(arguments, "scan_id", context)?;
        let path = path_argument(arguments, "path", scan.as_ref(), context.roots)?;
        let mut line = CommandLine::new(arguments);
        if let Some(scan) = scan {
            line.push("--scan").push(scan.selector);
        }
        line.push(path);
        Ok(line.invocation("explain"))
    }

    fn summary(&self, envelope: &Value) -> String {
        let result = &envelope["result"];
        let freeable = result["freeable_now_bytes"]
            .as_u64()
            .map_or_else(String::new, |n| format!(", {n} freeable now"));
        format!(
            "{} ({}): {} bytes on disk{freeable}",
            result["path"].as_str().unwrap_or("the path"),
            result["kind"].as_str().unwrap_or("entry"),
            result["allocated_bytes"]
        )
    }
}
