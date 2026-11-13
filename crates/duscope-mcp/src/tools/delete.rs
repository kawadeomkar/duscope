//! `duscope_delete`: moves entries to the Trash (`clean --from`), each named
//! by a handle from `duscope_explain` beside the bytes the caller expects it
//! to free.
//!
//! The server offers this tool only under `--allow-delete`; without the
//! flag it is not listed, and a call to it is an unknown tool. It reaches
//! the disk through `clean` and nothing else, so every check of that verb
//! holds. The tool only ever asks for the Trash: its command line carries
//! no way to say `--permanent`, and a value it passes on sits behind a `=`,
//! where it cannot be read as a flag.
//!
//! A call goes one of two ways. `dry_run` (the default) answers the plan
//! and moves nothing. `dry_run: false` is `--yes`. Either way the plan's
//! refusals stand: one item refused, or one whose `frees_bytes` is not
//! within 1% of its `expect_bytes` or is a directory no saved scan measured
//! (made since the scan, not entered by it, or holding what it did not
//! enter) or that a walk could not read whole, stops the whole call, as
//! `clean` stops a list it cannot wholly move; and an item that has changed
//! since the scan its handle was read from fails the call (`path_changed`).
//!
//! Each handle is verified here before the verb runs, and the path it names
//! must lie under the roots: a handle can have been issued by any `explain`
//! that shares the index directory's key, not only through this server.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use duscope_core::CoreError;
use duscope_core::handle::{Handle, Key};
use duscope_core::query::clock;
use serde_json::{Map, Value};

use super::{Annotations, CommandLine, Context, Invocation, Tool};
use crate::roots::Refusal;

/// `clean --from HANDLE --expect-item-bytes N … (--dry-run | --yes)`.
pub struct Delete;

impl Tool for Delete {
    fn name(&self) -> &'static str {
        "duscope_delete"
    }

    fn description(&self) -> &'static str {
        "Move entries to the Trash, each named by the handle duscope_explain gave it and the \
         bytes its deletion should free (the plan's frees_bytes). By default a dry run answers \
         the plan and moves nothing; set dry_run to false to move them. Trash only, never a \
         permanent delete, and a refused item or a size off by more than 1% stops the call."
    }

    fn input_schema(&self) -> &'static str {
        include_str!("schemas/duscope_delete.input.json")
    }

    fn verb(&self) -> Option<&'static str> {
        Some("clean")
    }

    fn annotations(&self) -> Annotations {
        Annotations {
            read_only: false,
            destructive: true,
            idempotent: false,
        }
    }

    fn requires_user_interaction(&self) -> bool {
        true
    }

    fn invocation(
        &self,
        arguments: &Map<String, Value>,
        context: &Context<'_>,
    ) -> Result<Invocation, Refusal> {
        let mut line = CommandLine::new(arguments);
        let items = arguments
            .get("items")
            .and_then(Value::as_array)
            .map_or(&[][..], Vec::as_slice);
        if !items.is_empty() {
            let key = Key::open(context.index_dir).map_err(|e| handle_refusal(&e))?;
            let now = clock::now();
            for item in items {
                let token = item["handle"].as_str().unwrap_or_default();
                // The schema makes it required; a figure that is not there is
                // never taken as one, least of all as 0.
                let Some(expected) = item["expect_bytes"].as_u64() else {
                    return Err(Refusal {
                        code: "usage",
                        message: "every item needs expect_bytes, the bytes it should free"
                            .to_owned(),
                        hint: "Call duscope_delete with expect_bytes set on each item, from \
                               duscope_explain's freeable_now_bytes or a dry run's frees_bytes"
                            .to_owned(),
                    });
                };
                let payload = Handle::verify(&key, token, now).map_err(|e| handle_refusal(&e))?;
                let path = Path::new(OsStr::from_bytes(&payload.path));
                context.roots.bound_item(path)?;
                line.push(format!("--from={token}"));
                line.push(format!("--expect-item-bytes={expected}"));
            }
        }
        if arguments.get("dry_run").and_then(Value::as_bool) == Some(false) {
            line.push("--yes");
        } else {
            line.push("--dry-run");
        }
        Ok(line.invocation("clean"))
    }

    fn summary(&self, envelope: &Value) -> String {
        let result = &envelope["result"];
        let plan = &result["plan"];
        let items = plan["items"].as_array().map_or(0, Vec::len);
        let plural = if items == 1 { "" } else { "s" };
        if result["trashed_count"].as_u64().is_some_and(|n| n > 0) {
            format!(
                "Moved {} item{plural} to the Trash, {} bytes on disk (freed once it is emptied); \
                 the saved scans still hold them",
                result["trashed_count"], result["trashed_bytes"]
            )
        } else {
            format!(
                "Dry run: {items} item{plural} would move to the Trash, {} bytes on disk, {} \
                 freed once it is emptied; nothing moved. Call duscope_delete with dry_run set \
                 to false to move them",
                plan["total_allocated_bytes"], plan["total_frees_bytes"]
            )
        }
    }
}

/// The refusal a handle that will not verify, or a key that will not open,
/// answers with: the code the verb would have used.
fn handle_refusal(error: &CoreError) -> Refusal {
    let (code, hint) = match error {
        CoreError::HandleExpired | CoreError::HandleInvalid(_) => (
            if matches!(error, CoreError::HandleExpired) {
                "handle_expired"
            } else {
                "handle_invalid"
            },
            "Call duscope_explain again for a fresh handle",
        ),
        _ => (
            "io_error",
            "Fix the index directory's handle.key, then call duscope_explain again",
        ),
    };
    Refusal {
        code,
        message: error.to_string(),
        hint: hint.to_owned(),
    }
}
