//! `duscope_caches`: the regenerable caches under a path in a saved scan
//! (`caches`).

use serde_json::{Map, Value};

use super::{Annotations, Context, Invocation, READ_ONLY, Tool, read_only, rows_text};
use crate::roots::Refusal;

/// `caches --no-scan [--scan ID] [--by-project] [--loose] [--min-size SIZE]
/// [--size KIND] [--fields F] [--limit N] [--cursor C] PATH`.
pub struct Caches;

impl Tool for Caches {
    fn name(&self) -> &'static str {
        "duscope_caches"
    }

    fn description(&self) -> &'static str {
        "The regenerable caches under a path in a saved scan (node_modules, target, .venv, \
         build caches, …), largest first, with what regenerates each, or one row per project; \
         paged with cursor."
    }

    fn input_schema(&self) -> &'static str {
        include_str!("schemas/duscope_caches.input.json")
    }

    fn annotations(&self) -> Annotations {
        READ_ONLY
    }

    fn invocation(
        &self,
        arguments: &Map<String, Value>,
        context: &Context<'_>,
    ) -> Result<Invocation, Refusal> {
        read_only("caches", arguments, context, |line| {
            line.switch("by_project", "--by-project")
                .switch("loose", "--loose")
                .value("min_size", "--min-size")
                .list();
        })
    }

    fn summary(&self, envelope: &Value) -> String {
        rows_text(envelope)
    }
}
