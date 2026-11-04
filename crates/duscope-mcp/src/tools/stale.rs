//! `duscope_stale`: large files under a path in a saved scan left
//! unmodified for a long time (`stale`).

use serde_json::{Map, Value};

use super::{Annotations, Context, Invocation, READ_ONLY, Tool, read_only, rows_text};
use crate::roots::Refusal;

/// `stale --no-scan [--scan ID] [--older-than AGE] [--min-size SIZE]
/// [--atime --attrs full] [--glob G]… [--exclude G]… [--size KIND]
/// [--fields F] [--limit N] [--cursor C] PATH`.
pub struct Stale;

impl Tool for Stale {
    fn name(&self) -> &'static str {
        "duscope_stale"
    }

    fn description(&self) -> &'static str {
        "Large files under a path in a saved scan left unmodified for a long time (by default \
         10 MB and 180 days), largest first; paged with cursor."
    }

    fn input_schema(&self) -> &'static str {
        include_str!("schemas/duscope_stale.input.json")
    }

    fn annotations(&self) -> Annotations {
        READ_ONLY
    }

    fn invocation(
        &self,
        arguments: &Map<String, Value>,
        context: &Context<'_>,
    ) -> Result<Invocation, Refusal> {
        read_only("stale", arguments, context, |line| {
            line.value("older_than", "--older-than")
                .value("min_size", "--min-size")
                .switch("atime", "--atime");
            // Access times are in a scan taken with --attrs full, which this
            // makes the one read when the call names none.
            if arguments.get("atime").and_then(Value::as_bool) == Some(true) {
                line.push("--attrs=full");
            }
            line.each("glob", "--glob")
                .each("exclude", "--exclude")
                .list();
        })
    }

    fn summary(&self, envelope: &Value) -> String {
        rows_text(envelope)
    }
}
