//! `duscope_top`: the largest directories or files under a path in a saved
//! scan (`top`).

use serde_json::{Map, Value};

use super::{Annotations, Context, Invocation, READ_ONLY, Tool, read_only, rows_text};
use crate::roots::Refusal;

/// `top --no-scan [--scan ID] [--files] [--depth N] [--min-size SIZE]
/// [--glob G]… [--exclude G]… [--size KIND] [--fields F] [--limit N]
/// [--cursor C] PATH`.
pub struct Top;

impl Tool for Top {
    fn name(&self) -> &'static str {
        "duscope_top"
    }

    fn description(&self) -> &'static str {
        "The largest directories, or files, under a path in a saved scan, largest first; \
         narrowed by depth, size and glob, and paged with cursor."
    }

    fn input_schema(&self) -> &'static str {
        include_str!("schemas/duscope_top.input.json")
    }

    fn verb(&self) -> Option<&'static str> {
        Some("top")
    }

    fn annotations(&self) -> Annotations {
        READ_ONLY
    }

    fn invocation(
        &self,
        arguments: &Map<String, Value>,
        context: &Context<'_>,
    ) -> Result<Invocation, Refusal> {
        read_only("top", arguments, context, |line| {
            line.switch("files", "--files")
                .value("depth", "--depth")
                .value("min_size", "--min-size")
                .each("glob", "--glob")
                .each("exclude", "--exclude")
                .list();
        })
    }

    fn summary(&self, envelope: &Value) -> String {
        rows_text(envelope)
    }
}
