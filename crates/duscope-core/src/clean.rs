//! `clean`: the plan of what to move to the Trash, and moving it.
//!
//! An item's facts come from the saved scan that holds it, as `explain`
//! reads them: its bytes, what deleting it frees now (its bytes less those
//! a link or clone member outside it keeps, so a file whose other links lie
//! elsewhere frees 0), its links outside it and the command that
//! regenerates it. An item no saved scan holds is planned from a fresh
//! `stat`, as `explain` answers it.
//!
//! An item is refused, with its `reason`, when it is a symbolic link, a
//! mount point, a firmlink, the scan's root (or `/`), outside the scan's
//! root, or on a read-only volume. A refusal is a fact of the plan; whether
//! it stops the run (`--skip-refused`) and whether the plan is acted on
//! (`--yes`, the prompt) is the binary's to decide, which calls
//! [`clean_execute`] only then.
//!
//! Nothing here deletes: [`clean_execute`] moves each item to the Trash
//! through `duscope-walk`, after checking that what is at its path is still
//! what the plan saw (its device and inode), and never falls back to
//! removing it.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use duscope_walk::{Flags, TrashError};

use crate::CoreError;
use crate::envelope::{CleanPlan, ErrorObject, Explain, PlanItem, name_fields};
use crate::index::Index;
use crate::index::writer::FIRMLINK;
use crate::query::{ExplainQuery, TreeView, explain, explain_unindexed};

/// An item to plan: its path, absolute, with every symbolic link above its
/// last component resolved and the last one kept, and, for an item from a
/// handle, the device and inode the handle names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanItem {
    /// The path.
    pub path: PathBuf,
    /// `(dev, ino)` from a verified handle: what must still be at the path.
    pub handle: Option<(u64, u64)>,
}

/// A plan: the envelope's, and what each item was when planned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// The plan as the envelope prints it.
    pub plan: CleanPlan,
    /// Each item's `(dev, ino)` when planned, in the plan's order.
    idents: Vec<(u64, u64)>,
}

impl Plan {
    /// The refused items.
    pub fn refused_count(&self) -> usize {
        self.plan
            .items
            .iter()
            .filter(|item| item.action == "refused")
            .count()
    }

    /// The items the plan would move.
    pub fn trash_count(&self) -> usize {
        self.plan
            .items
            .iter()
            .filter(|item| item.action == "trash")
            .count()
    }
}

/// What moving a plan did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CleanOutcome {
    /// The items moved to the Trash.
    pub trashed_count: u64,
    /// Their bytes on disk.
    pub trashed_bytes: u64,
    /// The items that could not be moved.
    pub failed_count: u64,
}

/// The plan for `items`, from `index` (the saved scan that holds them) where
/// there is one. A path that does not exist, or whose device and inode are
/// not those of its handle, fails the plan ([`CoreError::ScanNotFound`],
/// [`CoreError::PathChanged`]), as does an item named twice or inside
/// another ([`CoreError::CleanOverlap`]): `clean` acts on all or none of a
/// list.
pub fn clean_plan(
    items: &[CleanItem],
    index: Option<&Index>,
    q: &ExplainQuery<'_>,
) -> Result<Plan, CoreError> {
    overlap(items)?;
    let mut planned = Vec::with_capacity(items.len());
    let mut idents = Vec::with_capacity(items.len());
    for item in items {
        let meta = std::fs::symlink_metadata(&item.path).map_err(|e| {
            CoreError::ScanNotFound(if e.kind() == std::io::ErrorKind::NotFound {
                format!("{} does not exist", item.path.display())
            } else {
                format!("cannot stat {}: {e}", item.path.display())
            })
        })?;
        let ident = (meta.dev(), meta.ino());
        if item.handle.is_some_and(|signed| signed != ident) {
            return Err(CoreError::PathChanged(item.path.display().to_string()));
        }
        let id = index.and_then(|ix| ix.lookup(ix.below_root(&item.path)?));
        let facts = match (index, id) {
            (Some(ix), Some(id)) => explain(ix, id, q)?,
            _ => explain_unindexed(&item.path, q)?,
        };
        let reason = refusal(&item.path, &meta, index, id);
        planned.push(plan_item(&item.path, &meta, &facts, reason));
        idents.push(ident);
    }
    let moving = planned.iter().filter(|item| item.action == "trash");
    let total_allocated_bytes = moving.clone().map(|item| item.allocated_bytes).sum();
    let total_frees_bytes = moving.map(|item| item.frees_bytes.unwrap_or(0)).sum();
    Ok(Plan {
        plan: CleanPlan {
            items: planned,
            total_allocated_bytes,
            total_frees_bytes,
            method: "trash",
            confirmation_required: true,
        },
        idents,
    })
}

/// Fails when an item is named twice, or lies inside another item: the
/// other would move it, and its bytes would count twice.
fn overlap(items: &[CleanItem]) -> Result<(), CoreError> {
    for (i, item) in items.iter().enumerate() {
        for (j, other) in items.iter().enumerate() {
            if i == j {
                continue;
            }
            if item.path == other.path && i < j {
                return Err(CoreError::CleanOverlap(format!(
                    "{} is named twice",
                    item.path.display()
                )));
            }
            if item.path != other.path && item.path.starts_with(&other.path) {
                return Err(CoreError::CleanOverlap(format!(
                    "{} lies inside {}, which the plan moves already",
                    item.path.display(),
                    other.path.display()
                )));
            }
        }
    }
    Ok(())
}

/// Why `path` is refused, the first that applies; `None` when it is not. `id` is its entry in `index`, which holds it.
fn refusal(
    path: &Path,
    meta: &std::fs::Metadata,
    index: Option<&Index>,
    id: Option<u32>,
) -> Option<&'static str> {
    if meta.file_type().is_symlink() {
        return Some("symlink");
    }
    let parent_dev = path
        .parent()
        .and_then(|parent| std::fs::metadata(parent).ok())
        .map(|parent| parent.dev());
    let indexed_flags = || -> u32 {
        match (index, id) {
            (Some(ix), Some(id)) if ix.node(id).is_dir() => ix
                .db()
                .query_row("SELECT flags FROM dirs WHERE id = ?1", [id], |row| {
                    row.get(0)
                })
                .unwrap_or(0),
            _ => 0,
        }
    };
    let flags = indexed_flags();
    // Before the mount point: a firmlink leads into the Data volume, so its
    // device is not its parent's either.
    if duscope_walk::is_firmlink(path) || flags & FIRMLINK != 0 {
        return Some("firmlink");
    }
    if parent_dev.is_some_and(|dev| dev != meta.dev()) || flags & Flags::MOUNT_ROOT.bits() != 0 {
        return Some("mount_point");
    }
    if path.parent().is_none() || id == Some(0) {
        return Some("root");
    }
    if index.is_some() && id.is_none() {
        return Some("outside_root");
    }
    if duscope_walk::read_only_volume(path).unwrap_or(false) {
        return Some("read_only_volume");
    }
    None
}

/// The plan's row for `path`: its facts, and its action.
fn plan_item(
    path: &Path,
    meta: &std::fs::Metadata,
    facts: &Explain,
    reason: Option<&'static str>,
) -> PlanItem {
    let path_bytes = path.as_os_str().as_bytes().to_vec();
    let name = path.file_name().unwrap_or(OsStr::new("/")).as_bytes();
    let (name, name_raw) = name_fields(name);
    let kind = if meta.is_dir() {
        "dir"
    } else if meta.is_file() {
        "file"
    } else if meta.file_type().is_symlink() {
        "symlink"
    } else {
        "other"
    };
    PlanItem {
        path: String::from_utf8_lossy(&path_bytes).into_owned(),
        path_bytes,
        name,
        name_raw,
        kind,
        allocated_bytes: facts.allocated_bytes,
        frees_bytes: facts.freeable_now_bytes,
        frees_kind: facts.freeable_kind,
        nlink: facts.nlink,
        links_outside_subtree: facts.links_outside_subtree,
        regenerable_by: facts.regenerable_by,
        action: if reason.is_some() { "refused" } else { "trash" },
        reason,
        outcome: None,
        error: None,
    }
}

/// Moves each item the plan would move to the Trash, recording its outcome:
/// `trashed`, or `failed` with why. An item whose path no longer holds what
/// the plan saw (gone, or another device and inode) is `path_changed` and
/// left alone; one whose volume has no Trash is `trash_unavailable` and left
/// where it is: nothing is ever removed in its place.
pub fn clean_execute(plan: &mut Plan) -> CleanOutcome {
    let mut outcome = CleanOutcome::default();
    for (item, &ident) in plan.plan.items.iter_mut().zip(&plan.idents) {
        if item.action != "trash" {
            continue;
        }
        let path = Path::new(OsStr::from_bytes(&item.path_bytes));
        let now = std::fs::symlink_metadata(path).map(|m| (m.dev(), m.ino()));
        let moved = match now {
            Ok(now) if now == ident => duscope_walk::move_to_trash(path).map_err(|e| match e {
                TrashError::Unavailable(message) => error_object(
                    "trash_unavailable",
                    message,
                    Some("Delete it yourself with rm if it should go: its volume has no Trash"),
                    false,
                    "user",
                ),
                TrashError::Failed(message) => {
                    error_object("io_error", message, None, false, "user")
                }
            }),
            _ => Err(error_object(
                "path_changed",
                format!(
                    "{} is not what the plan saw: it has gone or been replaced",
                    item.path
                ),
                Some("Run duscope clean again to plan what is there now"),
                true,
                "agent",
            )),
        };
        match moved {
            Ok(()) => {
                item.outcome = Some("trashed");
                outcome.trashed_count += 1;
                outcome.trashed_bytes += item.allocated_bytes;
            }
            Err(error) => {
                item.outcome = Some("failed");
                item.error = Some(error);
                outcome.failed_count += 1;
            }
        }
    }
    outcome
}

/// An error object for an item.
fn error_object(
    code: &'static str,
    message: impl Into<String>,
    hint: Option<&str>,
    retryable: bool,
    fixable_by: &'static str,
) -> ErrorObject {
    ErrorObject {
        code,
        message: message.into(),
        hint: hint.map(str::to_owned),
        retryable,
        fixable_by,
    }
}

#[cfg(test)]
mod tests;
