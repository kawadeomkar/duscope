//! `duscope_diff`: what changed between two saved scans of one tree
//! (`diff`), always under a path inside the roots, so that a scan whose
//! root lies above a root is compared at that root.

use std::ffi::OsString;

use serde_json::{Map, Value};

use super::{
    Annotations, CommandLine, Context, Invocation, READ_ONLY, Tool, path_argument, scan_argument,
};
use crate::roots::Refusal;

/// `diff OLD NEW --under PATH [--min-delta SIZE] [--depth N] [--size KIND]
/// [--limit N]`.
pub struct Diff;

impl Tool for Diff {
    fn name(&self) -> &'static str {
        "duscope_diff"
    }

    fn description(&self) -> &'static str {
        "What changed between two saved scans of one tree, under a path: the entries added, \
         removed, and grown or shrunk, largest change first, with the totals."
    }

    fn input_schema(&self) -> &'static str {
        include_str!("schemas/duscope_diff.input.json")
    }

    fn annotations(&self) -> Annotations {
        READ_ONLY
    }

    fn invocation(
        &self,
        arguments: &Map<String, Value>,
        context: &Context<'_>,
    ) -> Result<Invocation, Refusal> {
        let old = scan_argument(arguments, "old", context)?;
        let new = scan_argument(arguments, "new", context)?;
        // The older scan reads a relative path and gives the default one,
        // unless it is latest, which names no one scan.
        let base = [old.as_ref(), new.as_ref()]
            .into_iter()
            .flatten()
            .find(|scan| scan.root.is_some());
        let path = path_argument(arguments, "path", base, context.roots)?;
        let mut under = OsString::from("--under=");
        under.push(&path);
        let mut line = CommandLine::new(arguments);
        for scan in [old, new].into_iter().flatten() {
            line.push(scan.selector);
        }
        line.push(under)
            .value("min_delta", "--min-delta")
            .value("depth", "--depth")
            .value("size", "--size")
            .value("limit", "--limit");
        Ok(line.invocation("diff"))
    }

    fn summary(&self, envelope: &Value) -> String {
        let diff = &envelope["diff"];
        format!(
            "{} bytes net from scan {} to {} under {}: {} entries added, {} removed, {} changed",
            diff["net_bytes"],
            diff["old_scan"].as_str().unwrap_or("?"),
            diff["new_scan"].as_str().unwrap_or("?"),
            envelope["query"]["path"].as_str().unwrap_or("the path"),
            diff["added_count"],
            diff["removed_count"],
            diff["changed_count"]
        )
    }
}
