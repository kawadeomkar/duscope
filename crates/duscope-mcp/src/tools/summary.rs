//! `duscope_summary`: a saved scan's totals under a path (`summary`).

use serde_json::{Map, Value};

use super::{Annotations, Context, Invocation, READ_ONLY, Tool, read_only};
use crate::roots::Refusal;

/// `summary --no-scan [--scan ID] PATH`.
pub struct Summary;

impl Tool for Summary {
    fn name(&self) -> &'static str {
        "duscope_summary"
    }

    fn description(&self) -> &'static str {
        "Totals under a path in a saved scan: bytes on disk and apparent, files, directories, \
         errors, the volumes, and the bytes no entry accounts for."
    }

    fn input_schema(&self) -> &'static str {
        include_str!("schemas/duscope_summary.input.json")
    }

    fn verb(&self) -> Option<&'static str> {
        Some("summary")
    }

    fn annotations(&self) -> Annotations {
        READ_ONLY
    }

    fn invocation(
        &self,
        arguments: &Map<String, Value>,
        context: &Context<'_>,
    ) -> Result<Invocation, Refusal> {
        read_only("summary", arguments, context, |_| {})
    }

    fn summary(&self, envelope: &Value) -> String {
        let result = &envelope["result"];
        format!(
            "{} bytes on disk in {} files and {} directories under {}",
            result["allocated_bytes"],
            result["file_count"],
            result["dir_count"],
            result["path"].as_str().unwrap_or("the path")
        )
    }
}
