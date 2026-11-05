//! `clean`: the plan of what to move to the Trash or delete, and acting on
//! it.
//!
//! A plan is made against one saved scan. An item's facts come from it, as
//! `explain` reads them: its bytes, its links outside it and the command
//! that regenerates it. What deleting it frees is the plan's: its bytes less
//! those a link or clone member outside the plan keeps, so a file whose
//! other links lie elsewhere frees 0, and two links both in the plan free
//! their bytes once. An item under the scan's root that the scan does not
//! hold (made since) is planned from a fresh `stat`, a directory with its
//! own walk; what it frees is not known.
//!
//! An item is refused, with its `reason`, when it is a symbolic link, a
//! firmlink, a mount point, the scan's root (or `/`), on a read-only volume,
//! or outside the scan's root. A refusal is a fact of the plan; whether it
//! stops the run (`--skip-refused`) and whether the plan is acted on
//! (`--yes`, the prompt) is the binary's to decide, which calls
//! [`clean_execute`] only then.
//!
//! A plan's [`Method`] is the Trash or `--permanent`. [`clean_execute`]
//! moves each item to the Trash through `duscope-walk`, after checking that
//! what is at its path is still what the plan saw (its device and inode),
//! and never falls back to removing it. With `--permanent` it deletes each
//! item through `duscope-walk`'s descriptor walk: every component of the
//! path opened without following a symbolic link, the item's device and
//! inode checked again before anything is removed, a directory emptied
//! bottom up, never across a mount; only then is `freed_bytes` counted, as
//! the bytes on disk of each inode whose last link went.
//!
//! [`within_one_percent`] is `--expect-bytes`' test, made before anything is
//! touched.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ffi::OsStr;
use std::fs::Metadata;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use duscope_walk::{CheckedDir, Flags, Kind, RemoveError, ScanEvent, TrashError, WalkOptions};

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

/// How a plan acts on its items.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// Move them to the Trash, which keeps them until it is emptied.
    Trash,
    /// Delete them (`--permanent`).
    Permanent,
}

impl Method {
    /// Its name in the plan: `method`, and the `action` of each item not
    /// refused.
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Trash => "trash",
            Method::Permanent => "permanent",
        }
    }
}

/// A plan: the envelope's, and what each item was when planned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// The plan as the envelope prints it.
    pub plan: CleanPlan,
    /// How it acts.
    method: Method,
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

    /// The items the plan would move or delete: those not refused.
    pub fn acting_count(&self) -> usize {
        self.plan
            .items
            .iter()
            .filter(|item| item.action != "refused")
            .count()
    }

    /// How the plan acts.
    pub fn method(&self) -> Method {
        self.method
    }

    /// Whether what the plan frees, its `total_frees_bytes`, lies within
    /// 1% of `expected` (`--expect-bytes`).
    pub fn meets(&self, expected: u64) -> bool {
        within_one_percent(self.plan.total_frees_bytes, expected)
    }
}

/// Whether `bytes` lies within 1% of `expected`, either way, the bounds
/// included: `|bytes - expected| <= expected / 100`, in exact arithmetic.
pub fn within_one_percent(bytes: u64, expected: u64) -> bool {
    100 * u128::from(bytes.abs_diff(expected)) <= u128::from(expected)
}

/// What acting on a plan did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CleanOutcome {
    /// The items moved to the Trash.
    pub trashed_count: u64,
    /// Their bytes on disk.
    pub trashed_bytes: u64,
    /// The items deleted, each with everything under it.
    pub deleted_count: u64,
    /// The bytes on disk deleting freed: each inode whose last link went,
    /// and each directory, as `du` counts them; a deletion that failed part
    /// way counts what it removed.
    pub freed_bytes: u64,
    /// The items that could not be moved or deleted, or not wholly.
    pub failed_count: u64,
}

/// The plan for `items` against `index`, the saved scan whose root they
/// should lie under, acting by `method`. A path that does not exist, or
/// whose device and inode are not those of its handle, fails the plan
/// ([`CoreError::ScanNotFound`], [`CoreError::PathChanged`]), as does an
/// item named twice or inside another ([`CoreError::CleanOverlap`]): `clean`
/// acts on all or none of a list.
pub fn clean_plan(
    items: &[CleanItem],
    index: &Index,
    q: &ExplainQuery<'_>,
    method: Method,
) -> Result<Plan, CoreError> {
    let mut metas = Vec::with_capacity(items.len());
    for item in items {
        let meta = std::fs::symlink_metadata(&item.path).map_err(|e| {
            CoreError::ScanNotFound(if e.kind() == std::io::ErrorKind::NotFound {
                format!("{} does not exist", item.path.display())
            } else {
                format!("cannot stat {}: {e}", item.path.display())
            })
        })?;
        if item
            .handle
            .is_some_and(|signed| signed != (meta.dev(), meta.ino()))
        {
            return Err(CoreError::PathChanged(item.path.display().to_string()));
        }
        metas.push(meta);
    }
    overlap(items, &metas)?;
    let mut planned = Vec::with_capacity(items.len());
    let mut ids = Vec::with_capacity(items.len());
    for (item, meta) in items.iter().zip(&metas) {
        let below = index.below_root(&item.path);
        let under_root = below.is_some();
        let id = below.and_then(|rel| index.lookup(rel));
        let mut facts = match id {
            Some(id) => explain(index, id, q)?,
            None => explain_unindexed(&item.path, q)?,
        };
        // A directory made since the scan: what is under it, not its inode
        // alone.
        if id.is_none() && under_root && meta.is_dir() {
            facts.allocated_bytes = walked(&item.path);
        }
        let reason = refusal(&item.path, meta, index, id, under_root);
        planned.push(plan_item(&item.path, meta, &facts, reason, method));
        ids.push(id);
    }
    plan_frees(index, &ids, &mut planned)?;
    let acting = planned.iter().filter(|item| item.action != "refused");
    let total_allocated_bytes = acting.clone().map(|item| item.allocated_bytes).sum();
    let total_frees_bytes = acting.map(|item| item.frees_bytes.unwrap_or(0)).sum();
    Ok(Plan {
        plan: CleanPlan {
            items: planned,
            total_allocated_bytes,
            total_frees_bytes,
            method: method.as_str(),
            confirmation_required: true,
        },
        method,
        idents: metas.iter().map(|m| (m.dev(), m.ino())).collect(),
    })
}

/// Fails when an item is named twice, or lies inside another item: the
/// other would move it, and its bytes would count twice. A directory is
/// known by its device and inode, so another path to it (a firmlink, a name
/// in another case) is the same item; anything else by its path.
fn overlap(items: &[CleanItem], metas: &[Metadata]) -> Result<(), CoreError> {
    let mut dirs: HashMap<(u64, u64), &Path> = HashMap::new();
    let mut others: HashSet<&Path> = HashSet::new();
    for (item, meta) in items.iter().zip(metas) {
        let path = item.path.as_path();
        let first = if meta.is_dir() {
            dirs.insert((meta.dev(), meta.ino()), path)
        } else {
            (!others.insert(path)).then_some(path)
        };
        if let Some(first) = first {
            return Err(CoreError::CleanOverlap(if first == path {
                format!("{} is named twice", path.display())
            } else {
                format!(
                    "{} and {} are one directory",
                    first.display(),
                    path.display()
                )
            }));
        }
    }
    for item in items {
        for ancestor in item.path.ancestors().skip(1) {
            let Ok(meta) = std::fs::metadata(ancestor) else {
                continue;
            };
            if let Some(outer) = dirs.get(&(meta.dev(), meta.ino())) {
                return Err(CoreError::CleanOverlap(format!(
                    "{} lies inside {}, which the plan moves already",
                    item.path.display(),
                    outer.display()
                )));
            }
        }
    }
    Ok(())
}

/// The bytes on disk of the directory `path` and what is under it on its
/// volume, each inode once.
fn walked(path: &Path) -> u64 {
    let opts = WalkOptions {
        one_file_system: true,
        clones: false,
        ..WalkOptions::default()
    };
    let mut seen = HashSet::new();
    let mut total = 0u64;
    // A walk that fails part way leaves what it reached: the figure is the
    // plan's, never what is moved.
    let _ = duscope_walk::walk(path, &opts, |event| {
        if let ScanEvent::Entry(entry) = event {
            if entry.kind == Kind::Dir || entry.nlink <= 1 || seen.insert((entry.dev, entry.ino)) {
                total = total.saturating_add(entry.allocated);
            }
        }
    });
    total
}

/// Why `path` is refused, the first that applies; `None` when it is not.
/// `id` is its entry in `index`, and `under_root` whether the scan's root
/// holds it.
fn refusal(
    path: &Path,
    meta: &Metadata,
    index: &Index,
    id: Option<u32>,
    under_root: bool,
) -> Option<&'static str> {
    if meta.file_type().is_symlink() {
        return Some("symlink");
    }
    let parent_dev = path
        .parent()
        .and_then(|parent| std::fs::metadata(parent).ok())
        .map(|parent| parent.dev());
    let flags: u32 = match id {
        Some(id) if index.node(id).is_dir() => index
            .db()
            .query_row("SELECT flags FROM dirs WHERE id = ?1", [id], |row| {
                row.get(0)
            })
            .unwrap_or(0),
        _ => 0,
    };
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
    if duscope_walk::read_only_volume(path).unwrap_or(false) {
        return Some("read_only_volume");
    }
    if !under_root {
        return Some("outside_root");
    }
    None
}

/// What each item the plan moves frees: its bytes, less those of each link
/// group or clone group charged under it that has a link or member outside
/// the plan. `explain` counts a sharer outside the item; the plan counts
/// only those outside every item it moves, so two links both in the plan
/// free their bytes once. An upper bound stays one; a group with a sharer
/// outside the plan makes the figure a lower bound. A refused item, one the
/// scan does not hold and one whose contents it did not read keep
/// `explain`'s figures.
fn plan_frees(
    index: &Index,
    ids: &[Option<u32>],
    planned: &mut [PlanItem],
) -> Result<(), CoreError> {
    let corrupt = |e: rusqlite::Error| CoreError::IndexCorrupt {
        file: index.file().display().to_string(),
        message: e.to_string(),
    };
    let moving: Vec<(usize, u32, u32)> = ids
        .iter()
        .zip(planned.iter())
        .enumerate()
        .filter_map(|(i, (id, item))| {
            let id = (*id).filter(|_| item.action != "refused")?;
            Some((i, id, index.node(id).last_id))
        })
        .collect();
    let db = index.db();
    let mut members = db
        .prepare("SELECT link_first, clone_first FROM shared_members WHERE id BETWEEN ?1 AND ?2")
        .map_err(corrupt)?;
    // The items' ranges never overlap, so a group's links in the plan are
    // the sum of its links in each.
    let (mut links_in, mut clones_in) = (BTreeMap::<u32, u64>::new(), BTreeMap::<u32, u64>::new());
    let mut groups: Vec<BTreeSet<u32>> = Vec::with_capacity(moving.len());
    for &(_, first, last) in &moving {
        let mut under = BTreeSet::new();
        let rows = members
            .query_map([first, last], |row| {
                Ok((row.get::<_, Option<u32>>(0)?, row.get::<_, Option<u32>>(1)?))
            })
            .map_err(corrupt)?;
        for row in rows {
            let (link, clone) = row.map_err(corrupt)?;
            if let Some(g) = link {
                *links_in.entry(g).or_default() += 1;
                under.insert(g);
            }
            if let Some(g) = clone {
                *clones_in.entry(g).or_default() += 1;
                under.insert(g);
            }
        }
        groups.push(under);
    }
    let count = |sql: &str, g: u32| -> Result<u64, CoreError> {
        let n: i64 = db.query_row(sql, [g], |row| row.get(0)).map_err(corrupt)?;
        Ok(u64::try_from(n).unwrap_or(0))
    };
    let mut outside = BTreeSet::new();
    for (&g, &n) in &links_in {
        if count(
            "SELECT COALESCE(MAX(nlink), 0) FROM hardlinks WHERE first_id = ?1",
            g,
        )? > n
        {
            outside.insert(g);
        }
    }
    for (&g, &n) in &clones_in {
        if count(
            "SELECT COUNT(*) FROM shared_members WHERE clone_first = ?1",
            g,
        )? > n
        {
            outside.insert(g);
        }
    }
    for (&(i, first, last), under) in moving.iter().zip(&groups) {
        let item = &mut planned[i];
        if item.frees_bytes.is_none() {
            continue;
        }
        let node = index.node(first);
        let total = if node.is_dir() || node.charged {
            node.allocated
        } else {
            0
        };
        let kept: u64 = under
            .iter()
            .filter(|&&g| outside.contains(&g) && (first..=last).contains(&g))
            .map(|&g| index.node(g).allocated)
            .sum();
        item.frees_bytes = Some(total.saturating_sub(kept));
        item.frees_kind = Some(if item.frees_kind == Some("upper_bound") {
            "upper_bound"
        } else if under.iter().any(|g| outside.contains(g)) {
            "lower_bound"
        } else {
            "exact"
        });
    }
    Ok(())
}

/// The plan's row for `path`: its facts, and its action.
fn plan_item(
    path: &Path,
    meta: &std::fs::Metadata,
    facts: &Explain,
    reason: Option<&'static str>,
    method: Method,
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
        action: if reason.is_some() {
            "refused"
        } else {
            method.as_str()
        },
        reason,
        outcome: None,
        error: None,
    }
}

/// Acts on each item the plan does not refuse, by its method, recording its
/// outcome: `trashed` or `deleted`, or `failed` with why.
///
/// - **The Trash.** An item whose path no longer holds what the plan saw
///   (gone, or another device and inode) is `path_changed` and left alone;
///   one whose volume has no Trash is `trash_unavailable` and left where it
///   is: nothing is ever removed in its place.
/// - **`--permanent`.** The item is deleted through descriptors, by
///   `duscope-walk`'s [`unlink_checked`](duscope_walk::unlink_checked) and
///   [`remove_dir_checked`](duscope_walk::remove_dir_checked); a change on
///   the way to it or under it is `path_changed`, a mount point under it
///   `refused_path`, and what was deleted before a failure stays deleted and
///   counts in `freed_bytes`.
pub fn clean_execute(plan: &mut Plan) -> CleanOutcome {
    let mut outcome = CleanOutcome::default();
    let method = plan.method;
    for (item, &ident) in plan.plan.items.iter_mut().zip(&plan.idents) {
        if item.action == "refused" {
            continue;
        }
        let done = match method {
            Method::Trash => trash(item, ident),
            Method::Permanent => delete(item, ident, &mut outcome.freed_bytes),
        };
        match done {
            Ok(()) if method == Method::Trash => {
                item.outcome = Some("trashed");
                outcome.trashed_count += 1;
                outcome.trashed_bytes += item.allocated_bytes;
            }
            Ok(()) => {
                item.outcome = Some("deleted");
                outcome.deleted_count += 1;
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

/// Moves `item` to the Trash when its path still holds `ident`.
fn trash(item: &PlanItem, ident: (u64, u64)) -> Result<(), ErrorObject> {
    let path = Path::new(OsStr::from_bytes(&item.path_bytes));
    match std::fs::symlink_metadata(path).map(|m| (m.dev(), m.ino())) {
        Ok(now) if now == ident => duscope_walk::move_to_trash(path).map_err(|e| match e {
            TrashError::Unavailable(message) => error_object(
                "trash_unavailable",
                message,
                Some("Rerun duscope clean with --permanent to delete it: its volume has no Trash"),
                false,
                "user",
            ),
            TrashError::Failed(message) => error_object("io_error", message, None, false, "user"),
        }),
        _ => Err(changed(&item.path)),
    }
}

/// Deletes `item`, adding to `freed` the bytes on disk its deletion frees:
/// its parent directory opened from `/` one component at a time without
/// following a symbolic link, then the item itself, through that
/// directory's descriptor, only when its device and inode are still
/// `ident`, the plan's; a directory with everything under it, bottom up,
/// never through a link or across a mount.
fn delete(item: &PlanItem, ident: (u64, u64), freed: &mut u64) -> Result<(), ErrorObject> {
    let path = Path::new(OsStr::from_bytes(&item.path_bytes));
    // The plan refuses `/`, the one path without both.
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(changed(&item.path));
    };
    CheckedDir::open(parent)
        .and_then(|dir| {
            if item.kind == "dir" {
                duscope_walk::remove_dir_checked(&dir, name, ident, freed)
            } else {
                duscope_walk::unlink_checked(&dir, name, ident, freed)
            }
        })
        .map_err(|e| removal_error(path, &e))
}

/// The error object of a deletion of `item` that failed with `error`.
fn removal_error(item: &Path, error: &RemoveError) -> ErrorObject {
    match error {
        RemoveError::Changed { path } if path == item => changed(&item.to_string_lossy()),
        RemoveError::Changed { path } => {
            let message = if item.starts_with(path) {
                format!(
                    "{}, on the way to {}, is no longer the directory the plan saw, so nothing \
                     was deleted",
                    path.display(),
                    item.display()
                )
            } else {
                format!(
                    "{} changed while {} was being deleted, so the deletion stopped there",
                    path.display(),
                    item.display()
                )
            };
            error_object(
                "path_changed",
                message,
                Some("Run duscope clean again to plan what is there now"),
                true,
                "agent",
            )
        }
        RemoveError::Mount { path } => error_object(
            "refused_path",
            format!(
                "{} is a mount point or another volume, which clean never crosses: it and the \
                 directories above it in {} were left",
                path.display(),
                item.display()
            ),
            Some(&format!(
                "Run umount {} first, then duscope clean again",
                path.display()
            )),
            false,
            "user",
        ),
        RemoveError::Io { path, source } => error_object(
            "io_error",
            format!("cannot delete {}: {source}", path.display()),
            None,
            false,
            "user",
        ),
    }
}

/// `path_changed` for the item at `path`.
fn changed(path: &str) -> ErrorObject {
    error_object(
        "path_changed",
        format!("{path} is not what the plan saw: it has gone or been replaced"),
        Some("Run duscope clean again to plan what is there now"),
        true,
        "agent",
    )
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
