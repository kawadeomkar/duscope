//! `duscope_by_type`: the bytes under a path in a saved scan by kind of
//! file (`by-type`).

use serde_json::{Map, Value};

use super::{Annotations, Context, Invocation, READ_ONLY, Tool, read_only};
use crate::roots::Refusal;

/// `by-type --no-scan [--scan ID] [--examples N] [--glob G]… [--exclude G]…
/// [--size KIND] [--fields F] [--limit N] PATH`.
pub struct ByType;

impl Tool for ByType {
    fn name(&self) -> &'static str {
        "duscope_by_type"
    }

    fn description(&self) -> &'static str {
        "The bytes under a path in a saved scan by kind of file (video, archives, code, …), \
         largest first, with each kind's largest files."
    }

    fn input_schema(&self) -> &'static str {
        include_str!("schemas/duscope_by_type.input.json")
    }

    fn annotations(&self) -> Annotations {
        READ_ONLY
    }

    fn invocation(
        &self,
        arguments: &Map<String, Value>,
        context: &Context<'_>,
    ) -> Result<Invocation, Refusal> {
        // by-type does not page: its schema has no cursor.
        read_only("by-type", arguments, context, |line| {
            line.value("examples", "--examples")
                .each("glob", "--glob")
                .each("exclude", "--exclude")
                .list();
        })
    }

    fn summary(&self, envelope: &Value) -> String {
        format!(
            "{} of {} kinds of file under {}",
            envelope["returned_count"],
            envelope["total_count"],
            envelope["query"]["path"].as_str().unwrap_or("the path")
        )
    }
}
