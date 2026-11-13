//! `clean`: the plan of what to move to the Trash or delete, and acting on
//! it.
//!
//! A plan is made against one saved scan. An item's facts come from it, as
//! `explain` reads them: its bytes, its links outside it and the command
//! that regenerates it. What deleting it frees is the plan's: its bytes less
//! those a link or clone member outside the plan keeps, so a file whose
//! other links lie elsewhere frees 0, and two links both in the plan free
//! their bytes once.
//!
//! The scan's figures stand only for an item that is still what the scan
//! saw. A file is compared with its row (kind, modification and change
//! times, apparent and allocated size, links) through the `lstat` the plan
//! takes anyway; a directory is walked, and the files, directories, apparent
//! and allocated bytes and newest modification and change times under it
//! compared with its rows, leaving out what lies under a directory the scan
//! listed without entering. The change time is what a program cannot set:
//! it moves when a file's time is restored after a write and when a link to
//! it goes elsewhere. An item that differs has changed since the scan
//! ([`Plan::changed_count`]): it is planned from the tree, as an item the
//! scan does not hold (made since) is, from a fresh `stat`, a directory with
//! its walk, whose bytes it frees at most (`upper_bound`); a directory the
//! scan did not enter is planned the same way, without having changed. A
//! file with other links that has changed frees its bytes at most, once
//! however many of its links the plan holds. An item from a handle that has
//! changed since the scan fails the plan ([`CoreError::PathChanged`]): the
//! handle signs what the scan saw.
//!
//! A directory that holds one the scan did not enter, with bytes under it,
//! keeps the scan's figures with those bytes added, and is an `upper_bound`:
//! moving it moves them too. A directory a walk could not read whole has no
//! figure at all (`frees_bytes` null, [`Plan::unread_count`]): what the walk
//! missed is not known, and a figure from what it reached would look like a
//! measurement, so no expectation is met by it.
//!
//! An item is refused, with its `reason`, when it is a symbolic link, a
//! firmlink, a mount point, the scan's root (or `/`), on a read-only volume,
//! or outside the scan's root. For the Trash a directory that holds a mount
//! point, at any depth, is refused as one (`mount_point`), and so is one
//! whose mounts the system cannot tell: a move renames the directory with
//! every volume mounted in it, which stays mounted in the Trash and is
//! emptied with it. The mount table says so, read for the plan and again
//! just before each move, and so does the walk that compares a directory
//! with its rows, which meets a mount it does not enter (a mount point
//! spelled otherwise than the table has it). A deletion never crosses a
//! mount, and stops at one. A refusal is a fact of the plan; whether it
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
//! touched. [`Plan::refuse_unexpected`] holds each item to it, for a caller
//! that names what each item should free (the MCP tool): an item off by
//! more than 1% is refused, a fact of the plan like any other refusal, and
//! so is a directory whose bytes no saved scan measured (made since the
//! scan, not entered by it, or holding what it did not enter), whatever its
//! figure: `explain` gives none for what it never read, and the walk that
//! prices it is the plan's own.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ffi::OsStr;
use std::fs::Metadata;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use duscope_walk::{
    CheckedDir, DirId, Flags, Kind, RemoveError, ScanEvent, Skip, TrashError, WalkOptions,
};

use crate::CoreError;
use crate::envelope::{CleanPlan, ErrorObject, Explain, PlanItem, name_fields};
use crate::index::Index;
use crate::index::writer::{EXCLUDED, EXCLUDED_CACHE, FIRMLINK};
use crate::query::{ExplainQuery, TreeView, explain, explain_unindexed, kind_name};

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
    /// Where each item's figures come from, in the plan's order.
    priced: Vec<Priced>,
}

/// Where an item's figures come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Priced {
    /// The saved scan: the item is what its rows say, or is refused (its
    /// figures move nothing).
    Scan,
    /// The saved scan, and a walk for what the scan did not read under the
    /// item: what its rows hold has not changed, but a directory under it
    /// the scan listed without entering holds bytes, or the walk could not
    /// read all of it.
    Partly,
    /// The tree, the scan having no figures for it: an item made since the
    /// scan, or a directory it listed without entering.
    Unscanned,
    /// The tree, the item having changed since the scan: it differs from
    /// its rows.
    Changed,
}

impl Plan {
    /// The items that changed since the saved scan, whose figures come from
    /// the tree: the `changed_since_scan` warning's count.
    pub fn changed_count(&self) -> usize {
        self.priced
            .iter()
            .filter(|&&priced| priced == Priced::Changed)
            .count()
    }

    /// The items not refused that a walk could not read whole, whose
    /// `frees_bytes` is null: what they free is not known.
    pub fn unread_count(&self) -> usize {
        self.plan
            .items
            .iter()
            .filter(|item| item.action != "refused" && item.frees_bytes.is_none())
            .count()
    }

    /// Whether item `at` is a directory whose bytes no saved scan measured,
    /// or not all of them: one made since the scan, one it listed without
    /// entering, or one that holds such a directory with bytes under it.
    /// Some of its figures come from a walk at plan time, and `explain`
    /// gives none for what the scan never read.
    pub fn unmeasured(&self, at: usize) -> bool {
        matches!(
            self.priced.get(at),
            Some(Priced::Unscanned | Priced::Partly)
        ) && self
            .plan
            .items
            .get(at)
            .is_some_and(|item| item.kind == "dir")
    }

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
    /// 1% of `expected` (`--expect-bytes`). A plan with an item whose
    /// figure is not known meets no figure: its total leaves that item out.
    pub fn meets(&self, expected: u64) -> bool {
        self.unread_count() == 0 && within_one_percent(self.plan.total_frees_bytes, expected)
    }

    /// Refuses, with [`UNEXPECTED`], each item not refused already whose
    /// `frees_bytes` lies outside 1% of its figure in `expected`, given in
    /// the plan's order, or is not known, or is a directory no saved scan
    /// measured ([`Plan::unmeasured`]): its bytes come from the plan's own
    /// walk, not from the scan a figure is read from, so no figure is held
    /// to account by it. The plan's totals then count what is left.
    pub fn refuse_unexpected(&mut self, expected: &[u64]) {
        let unmeasured: Vec<bool> = (0..self.plan.items.len())
            .map(|at| self.unmeasured(at))
            .collect();
        for ((item, &expected), unmeasured) in
            self.plan.items.iter_mut().zip(expected).zip(unmeasured)
        {
            if item.action != "refused"
                && (unmeasured
                    || !item
                        .frees_bytes
                        .is_some_and(|frees| within_one_percent(frees, expected)))
            {
                item.action = "refused";
                item.reason = Some(UNEXPECTED);
            }
        }
        let (allocated, frees) = totals(&self.plan.items);
        self.plan.total_allocated_bytes = allocated;
        self.plan.total_frees_bytes = frees;
    }
}

/// The `reason` of an item [`Plan::refuse_unexpected`] refuses.
pub const UNEXPECTED: &str = "expectation_mismatch";

/// The plan's totals over the items it acts on: their bytes on disk and
/// what it frees (`frees_bytes`, 0 where it is not known).
fn totals(items: &[PlanItem]) -> (u64, u64) {
    let acting = items.iter().filter(|item| item.action != "refused");
    (
        acting.clone().map(|item| item.allocated_bytes).sum(),
        acting.map(|item| item.frees_bytes.unwrap_or(0)).sum(),
    )
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
/// item from a handle that has changed since the scan
/// ([`CoreError::PathChanged`]) and an item named twice or inside another
/// ([`CoreError::CleanOverlap`]): `clean` acts on all or none of a list.
pub fn clean_plan(
    items: &[CleanItem],
    index: &Index,
    q: &ExplainQuery<'_>,
    method: Method,
) -> Result<Plan, CoreError> {
    clean_plan_until(items, index, q, method, &Arc::default())
}

/// [`clean_plan`], stopped when `stop` is set: between items, and within
/// one directory per walk worker, it then fails
/// ([`CoreError::PlanStopped`]), so a plan a caller has given up on never
/// reads as made from a walk cut short.
pub fn clean_plan_until(
    items: &[CleanItem],
    index: &Index,
    q: &ExplainQuery<'_>,
    method: Method,
    stop: &Arc<AtomicBool>,
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
            return Err(CoreError::PathChanged(format!(
                "{} is not the entry its handle names: it has been replaced since explain read it",
                item.path.display()
            )));
        }
        metas.push(meta);
    }
    overlap(items, &metas)?;
    let mut planned = Vec::with_capacity(items.len());
    let mut ids = Vec::with_capacity(items.len());
    let mut priced = Vec::with_capacity(items.len());
    let mut unentered_bytes = Vec::with_capacity(items.len());
    let mut linked = HashSet::new();
    for (item, meta) in items.iter().zip(&metas) {
        if stop.load(Ordering::Relaxed) {
            return Err(CoreError::PlanStopped);
        }
        let below = index.below_root(&item.path);
        let under_root = below.is_some();
        let id = below.and_then(|rel| index.lookup(rel));
        let (reason, from, walk) = judged(item, meta, index, (id, under_root), method, stop)?;
        if from == Priced::Changed && item.handle.is_some() {
            return Err(CoreError::PathChanged(format!(
                "{} has changed since the saved scan its handle was read from: it is not what \
                 explain examined",
                item.path.display()
            )));
        }
        // Priced from the tree: kept out of the scan's figures, its id kept
        // for the refusal only.
        let from_scan = matches!(from, Priced::Scan | Priced::Partly);
        let mut facts = match id {
            Some(id) if from_scan => explain(index, id, q)?,
            _ => explain_unindexed(&item.path, q)?,
        };
        if let Some(walk) = &walk
            && matches!(from, Priced::Unscanned | Priced::Changed)
        {
            facts.allocated_bytes = walk.allocated;
            facts.freeable_now_bytes = Some(walk.allocated);
            facts.freeable_kind = Some("upper_bound");
        }
        // Its other links may be in this plan, or gone: it frees its bytes
        // at most, and two links of one inode free them once.
        if from == Priced::Changed && !meta.is_dir() && meta.nlink() > 1 {
            let first = linked.insert((meta.dev(), meta.ino()));
            facts.freeable_now_bytes = Some(if first { facts.allocated_bytes } else { 0 });
            facts.freeable_kind = Some("upper_bound");
        }
        if walk.as_ref().is_some_and(|walk| walk.failed) {
            facts.freeable_now_bytes = None;
            facts.freeable_kind = None;
        }
        planned.push(plan_item(&item.path, meta, &facts, reason, method));
        ids.push(id.filter(|_| from_scan));
        priced.push(from);
        unentered_bytes.push(
            walk.filter(|_| from == Priced::Partly)
                .map_or(0, |walk| walk.unentered),
        );
    }
    if stop.load(Ordering::Relaxed) {
        return Err(CoreError::PlanStopped);
    }
    plan_frees(index, &ids, &mut planned)?;
    add_unentered(&mut planned, &unentered_bytes);
    let (total_allocated_bytes, total_frees_bytes) = totals(&planned);
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
        priced,
    })
}

/// Why `item`, found with `meta`, is refused, if it is, where its figures
/// come from, and the walk that measured it, if one did. `id` is its entry
/// in `index`, and `under_root` whether the scan's root holds it.
fn judged(
    item: &CleanItem,
    meta: &Metadata,
    index: &Index,
    (id, under_root): (Option<u32>, bool),
    method: Method,
    stop: &Arc<AtomicBool>,
) -> Result<(Option<&'static str>, Priced, Option<Walked>), CoreError> {
    let reason = refusal(&item.path, meta, index, id, under_root, method);
    let (from, walk) = match id {
        // Made since the scan: a directory with what is under it, not its
        // inode alone.
        None => (
            Priced::Unscanned,
            (under_root && meta.is_dir()).then(|| walked(&item.path, &HashSet::new(), stop)),
        ),
        // A refused item moves nothing: the scan's figures stand.
        Some(_) if reason.is_some() => (Priced::Scan, None),
        Some(id) => compared(index, id, &item.path, meta, stop)?,
    };
    // A mount the walk met under it, which the mount table missed: by
    // another spelling of its path, say. Refused as one the table lists,
    // its figures moving nothing.
    if method == Method::Trash && reason.is_none() && walk.as_ref().is_some_and(|w| w.mount) {
        return Ok(match id {
            Some(_) => (Some("mount_point"), Priced::Scan, None),
            None => (Some("mount_point"), from, walk),
        });
    }
    Ok((reason, from, walk))
}

/// Adds to each item the bytes a walk found under the directories the scan
/// did not enter: they go with it, and are what deleting it frees beyond the
/// scan's figures, at most (a link between the two parts counts twice).
fn add_unentered(planned: &mut [PlanItem], unentered: &[u64]) {
    for (item, &bytes) in planned.iter_mut().zip(unentered) {
        if bytes > 0 {
            item.allocated_bytes = item.allocated_bytes.saturating_add(bytes);
            if let Some(frees) = &mut item.frees_bytes {
                *frees = frees.saturating_add(bytes);
                item.frees_kind = Some("upper_bound");
            }
        }
    }
}

/// Whether the indexed entry `id`, found at `path` with `meta`, is still
/// what the scan saw, and the walk that prices it where the scan's figures
/// do not cover it. A file (any entry not a directory) is compared with its
/// row: its kind, modification and change times, apparent and allocated
/// size and links. A directory is walked, and what lies under it on its
/// volume compared with its rows: how many files and directories, their
/// apparent and allocated bytes (each link and clone member as the rows
/// count it, in full) and the newest modification and change times, what
/// lies under a directory the scan listed without entering left out of
/// both; a directory the scan did not enter itself has no rows to compare
/// with. An entry whose kind changed has changed. A false difference costs
/// an `upper_bound` and a warning; a missed one is a stale figure.
fn compared(
    index: &Index,
    id: u32,
    path: &Path,
    meta: &Metadata,
    stop: &Arc<AtomicBool>,
) -> Result<(Priced, Option<Walked>), CoreError> {
    let node = index.node(id);
    if node.is_dir() != meta.is_dir() {
        let walk = meta.is_dir().then(|| walked(path, &HashSet::new(), stop));
        return Ok((Priced::Changed, walk));
    }
    if !node.is_dir() {
        // As the walk reads them: devices, sockets and pipes hold no
        // blocks of their own.
        let allocated = if kind_of(meta) == "other" {
            0
        } else {
            meta.blocks().saturating_mul(512)
        };
        let same = kind_name(node.kind) == kind_of(meta)
            && node.mtime_ns == nanos(meta.mtime(), meta.mtime_nsec())
            && node.ctime_ns == nanos(meta.ctime(), meta.ctime_nsec())
            && node.apparent == meta.len()
            && node.allocated == allocated
            && u64::from(node.nlink) == meta.nlink();
        return Ok((if same { Priced::Scan } else { Priced::Changed }, None));
    }
    if !index.entered(id) {
        return Ok((Priced::Unscanned, Some(walked(path, &HashSet::new(), stop))));
    }
    let skipped = unentered(index, id, node.last_id)?;
    let below: HashSet<Vec<u8>> = skipped.iter().map(|dir| dir.below.clone()).collect();
    let walk = walked(path, &below, stop);
    if walk.tally != rows(index, id, node.last_id, &skipped)? {
        Ok((Priced::Changed, Some(walk)))
    } else if walk.failed || walk.unentered > 0 {
        Ok((Priced::Partly, Some(walk)))
    } else {
        // Priced from the scan: the walk is kept for the mounts it met.
        Ok((Priced::Scan, Some(walk)))
    }
}

/// A time in Unix nanoseconds, as the walk and the index hold it.
fn nanos(secs: i64, nsec: i64) -> i64 {
    secs.saturating_mul(1_000_000_000).saturating_add(nsec)
}

/// The kind of entry `meta` describes, by its name in the plan and the
/// index: `dir`, `file`, `symlink` or `other`.
fn kind_of(meta: &Metadata) -> &'static str {
    let file_type = meta.file_type();
    if file_type.is_dir() {
        "dir"
    } else if file_type.is_file() {
        "file"
    } else if file_type.is_symlink() {
        "symlink"
    } else {
        "other"
    }
}

/// What a walk and a subtree's rows are compared by: how many files (every
/// entry not a directory) and directories, the directory itself included,
/// their apparent and allocated bytes, the newest modification and change
/// times, and the sum of the change times. Each entry counts in full: a
/// later link and a clone member hold their bytes in their rows as they do
/// in a walk, whichever member the scan charged them to.
///
/// The sum sees a change the newest time hides: one under an entry whose
/// change time lies in the future (a clock set back, a volume whose change
/// time is its modification time). It is kept as whole seconds and the
/// nanoseconds past them, summed apart in 64 bits, so neither overflows on
/// a tree of up to a billion entries, whatever their times: a time held in
/// 64 bits of nanoseconds is under 2^63 / 10^9 whole seconds either way of
/// 1970, and its nanoseconds past the second are under 10^9.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Tally {
    files: u64,
    dirs: u64,
    apparent: u64,
    allocated: u64,
    max_mtime_ns: i64,
    max_ctime_ns: i64,
    ctime_secs: i64,
    ctime_nanos: i64,
}

impl Tally {
    /// The tally of nothing.
    fn new() -> Tally {
        Tally {
            files: 0,
            dirs: 0,
            apparent: 0,
            allocated: 0,
            max_mtime_ns: i64::MIN,
            max_ctime_ns: i64::MIN,
            ctime_secs: 0,
            ctime_nanos: 0,
        }
    }

    /// Adds a change time, in Unix nanoseconds, to the sum: its whole
    /// seconds and the nanoseconds past them, as the index's rows are summed
    /// (integer `/` and `%` in SQL, truncating toward zero, as Rust's do).
    fn add_ctime(&mut self, ctime_ns: i64) {
        self.ctime_secs = self.ctime_secs.saturating_add(ctime_ns / NANOS);
        self.ctime_nanos = self.ctime_nanos.saturating_add(ctime_ns % NANOS);
    }
}

/// Nanoseconds in a second.
const NANOS: i64 = 1_000_000_000;

/// The tally of the rows `id..=last` of `index`, the directory `id` and
/// everything the scan recorded under it, but for what lies under each of
/// the directories in `skipped` (in id order): a directory whose reading
/// failed part way holds what was read before, which a walk leaves out
/// with the rest.
fn rows(index: &Index, id: u32, last: u32, skipped: &[Unentered]) -> Result<Tally, CoreError> {
    let corrupt = |e: rusqlite::Error| CoreError::IndexCorrupt {
        file: index.file().display().to_string(),
        message: e.to_string(),
    };
    // The ranges of ids to count: `id..=last` without each skipped
    // directory's `dir + 1..=last`, a range inside another counted once.
    let mut ranges = Vec::new();
    let mut from = id;
    for dir in skipped {
        if dir.id >= from && dir.last > dir.id {
            ranges.push((from, dir.id));
            from = dir.last.saturating_add(1);
        }
    }
    if from <= last {
        ranges.push((from, last));
    }
    let mut tally = Tally::new();
    for table in ["files", "dirs"] {
        let mut stmt = index
            .db()
            .prepare(&format!(
                "SELECT COUNT(*), COALESCE(SUM(apparent), 0), COALESCE(SUM(allocated), 0), \
                 MAX(mtime), MAX(ctime), COALESCE(SUM(ctime / {NANOS}), 0), \
                 COALESCE(SUM(ctime % {NANOS}), 0) FROM {table} WHERE id BETWEEN ?1 AND ?2"
            ))
            .map_err(corrupt)?;
        for &(first, end) in &ranges {
            let (n, apparent, allocated, newest, changed, secs, nanos): (
                i64,
                i64,
                i64,
                Option<i64>,
                Option<i64>,
                i64,
                i64,
            ) = stmt
                .query_row([first, end], |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                    ))
                })
                .map_err(corrupt)?;
            let n = u64::try_from(n).unwrap_or(0);
            if table == "files" {
                tally.files += n;
            } else {
                tally.dirs += n;
            }
            tally.apparent = tally
                .apparent
                .saturating_add(u64::try_from(apparent).unwrap_or(0));
            tally.allocated = tally
                .allocated
                .saturating_add(u64::try_from(allocated).unwrap_or(0));
            tally.max_mtime_ns = tally.max_mtime_ns.max(newest.unwrap_or(i64::MIN));
            tally.max_ctime_ns = tally.max_ctime_ns.max(changed.unwrap_or(i64::MIN));
            tally.ctime_secs = tally.ctime_secs.saturating_add(secs);
            tally.ctime_nanos = tally.ctime_nanos.saturating_add(nanos);
        }
    }
    Ok(tally)
}

/// A directory under a planned one that the scan listed without entering.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Unentered {
    /// Its id.
    id: u32,
    /// The last id under it.
    last: u32,
    /// Its path below the planned directory.
    below: Vec<u8>,
}

/// The directories under `id` (to `last`) the scan listed without entering
/// them (an exclude, a cache rule, a container, an unreadable directory, a
/// mount it did not cross), in id order: the scan holds nothing under them
/// to compare a walk with.
fn unentered(index: &Index, id: u32, last: u32) -> Result<Vec<Unentered>, CoreError> {
    let corrupt = |e: rusqlite::Error| CoreError::IndexCorrupt {
        file: index.file().display().to_string(),
        message: e.to_string(),
    };
    let mut stmt = index
        .db()
        .prepare(
            "SELECT id, last_id FROM dirs WHERE id > ?1 AND id <= ?2 \
             AND (flags & ?3 != 0 OR errors IS NOT NULL) ORDER BY id",
        )
        .map_err(corrupt)?;
    let skipped = EXCLUDED | EXCLUDED_CACHE | Flags::MOUNT_ROOT.bits() | FIRMLINK;
    let candidates: Vec<(u32, u32)> = stmt
        .query_map([id, last, skipped], |row| Ok((row.get(0)?, row.get(1)?)))
        .map_err(corrupt)?
        .collect::<Result<_, _>>()
        .map_err(corrupt)?;
    let base = index.path(id);
    Ok(candidates
        .into_iter()
        .filter(|&(dir, _)| !index.entered(dir))
        .filter_map(|(dir, last)| {
            let path = index.path(dir);
            let below = if base.is_empty() {
                &path[..]
            } else {
                path.strip_prefix(base.as_slice())?.strip_prefix(b"/")?
            };
            Some(Unentered {
                id: dir,
                last,
                below: below.to_vec(),
            })
        })
        .collect())
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

/// What a walk of a directory found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Walked {
    /// The bytes on disk of the directory and what is under it on its
    /// volume, each inode once: what the plan prices it at.
    allocated: u64,
    /// Those of them under the directories it was told the scan did not
    /// enter: what no row of the scan holds.
    unentered: u64,
    /// What it is compared with the scan's rows by, what lies under the
    /// directories it was told the scan did not enter left out.
    tally: Tally,
    /// Whether something could not be read: a directory, an entry, or the
    /// walk itself. Its bytes then say nothing of what was missed.
    failed: bool,
    /// Whether it met a mount under the directory, which it does not enter:
    /// another volume, or a bind mount where the system tells one.
    mount: bool,
    /// The most directories it kept at once, to find the unentered ones and
    /// leave out what lies under them.
    #[cfg(test)]
    kept_peak: usize,
}

/// The paths below a walk's root that lead to one of the `unentered`
/// directories (paths below it too), the root's own (empty) included: only
/// a directory at one of them can.
fn leading(unentered: &HashSet<Vec<u8>>) -> HashSet<&[u8]> {
    unentered
        .iter()
        .flat_map(|below| {
            let ends = below.iter().enumerate().filter(|&(_, &b)| b == b'/');
            std::iter::once(0)
                .chain(ends.map(|(at, _)| at))
                .map(move |end| &below[..end])
        })
        .collect()
}

/// Walks the directory `path` and what is under it on its volume. The
/// tally leaves out what lies under each directory named in `unentered`
/// (paths below `path`, as [`unentered`] gives them); the bytes on disk
/// count everything. A walk stopped by `stop` has failed, as one that
/// could not read part of the tree has.
fn walked(path: &Path, unentered: &HashSet<Vec<u8>>, stop: &Arc<AtomicBool>) -> Walked {
    let opts = WalkOptions {
        one_file_system: true,
        clones: false,
        cancel: Arc::clone(stop),
        ..WalkOptions::default()
    };
    let mut seen = HashSet::new();
    let mut allocated = 0u64;
    let mut under_unentered = 0u64;
    let mut failed = false;
    let mut mount = false;
    let mut tally = Tally::new();
    let leading = leading(unentered);
    // The path below `path` of each directory that leads to an unentered
    // one, and the directories under an unentered one, each kept while it
    // is read: the walk delivers what a directory holds before it closes it.
    let mut paths: HashMap<DirId, Vec<u8>> = HashMap::new();
    let mut left_out: HashSet<DirId> = HashSet::new();
    #[cfg(test)]
    let mut kept_peak = 0;
    let walk = duscope_walk::walk(path, &opts, |event| {
        let entry = match event {
            ScanEvent::Entry(entry) => entry,
            ScanEvent::DirClosed { dir, errno } => {
                failed |= errno.is_some();
                paths.remove(&dir);
                left_out.remove(&dir);
                return;
            }
            ScanEvent::Error(_) => {
                failed = true;
                return;
            }
            // A walk that stays on its volume lists a mount without entering
            // it; nothing else here is skipped, but by a rule of a scan.
            ScanEvent::Skipped { reason, .. } => {
                mount |= !matches!(
                    reason,
                    Skip::Excluded | Skip::ExcludedCache | Skip::Containers
                );
                return;
            }
            _ => return,
        };
        let counted =
            entry.kind == Kind::Dir || entry.nlink <= 1 || seen.insert((entry.dev, entry.ino));
        if counted {
            allocated = allocated.saturating_add(entry.allocated);
        }
        if left_out.contains(&entry.parent) {
            if counted {
                under_unentered = under_unentered.saturating_add(entry.allocated);
            }
            if let Some(dir) = entry.dir {
                left_out.insert(dir);
            }
            return;
        }
        if entry.kind == Kind::Dir {
            tally.dirs += 1;
        } else {
            tally.files += 1;
        }
        tally.apparent = tally.apparent.saturating_add(entry.apparent);
        tally.allocated = tally.allocated.saturating_add(entry.allocated);
        tally.max_mtime_ns = tally.max_mtime_ns.max(entry.mtime_ns);
        tally.max_ctime_ns = tally.max_ctime_ns.max(entry.ctime_ns);
        tally.add_ctime(entry.ctime_ns);
        if let Some(dir) = entry.dir.filter(|_| !unentered.is_empty()) {
            // The walk's root is its own parent; a directory whose parent
            // leads to no unentered one leads to none either.
            let below = if dir == entry.parent {
                Some(Vec::new())
            } else {
                paths.get(&entry.parent).map(|up| {
                    let mut below = up.clone();
                    if !below.is_empty() {
                        below.push(b'/');
                    }
                    below.extend_from_slice(entry.name.as_bytes());
                    below
                })
            };
            match below {
                Some(below) if unentered.contains(&below) => {
                    left_out.insert(dir);
                }
                Some(below) if leading.contains(below.as_slice()) => {
                    paths.insert(dir, below);
                }
                _ => {}
            }
        }
        #[cfg(test)]
        {
            kept_peak = std::cmp::max(kept_peak, paths.len() + left_out.len());
        }
    });
    Walked {
        allocated,
        unentered: under_unentered,
        tally,
        failed: failed || walk.is_err(),
        mount,
        #[cfg(test)]
        kept_peak,
    }
}

/// Why `path` is refused, the first that applies; `None` when it is not.
/// `id` is its entry in `index`, and `under_root` whether the scan's root
/// holds it. For the Trash, a directory that holds a mount point is refused
/// as `mount_point` too, and so is one whose mounts the system cannot tell:
/// a move renames it with every volume mounted in it, which then stays
/// mounted in the Trash and goes when the Trash is emptied. A deletion
/// never crosses a mount, and stops at one ([`clean_execute`]).
fn refusal(
    path: &Path,
    meta: &Metadata,
    index: &Index,
    id: Option<u32>,
    under_root: bool,
    method: Method,
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
    if method == Method::Trash
        && meta.is_dir()
        && !duscope_walk::mount_point_under(path).is_ok_and(|point| point.is_none())
    {
        return Some("mount_point");
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
    PlanItem {
        path: String::from_utf8_lossy(&path_bytes).into_owned(),
        path_bytes,
        name,
        name_raw,
        kind: kind_of(meta),
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
///   a directory the mount table, read again, lists a mount point in, or
///   cannot be read for, is `refused_path` and left alone; one whose volume
///   has no Trash is `trash_unavailable` and left where it is: nothing is
///   ever removed in its place.
/// - **`--permanent`.** The item is deleted through descriptors, by
///   `duscope-walk`'s [`unlink_checked`](duscope_walk::unlink_checked) and
///   [`remove_dir_checked`](duscope_walk::remove_dir_checked); a change on
///   the way to it or under it is `path_changed`, a mount point under it,
///   or a directory whose mount the system cannot tell, `refused_path`, and
///   what was deleted before a failure stays deleted and counts in
///   `freed_bytes`.
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

/// Moves `item` to the Trash when, for a directory, the mount table, read
/// again, lists no mount point in it (one mounted since the plan would go
/// with it), and its path still holds `ident`: `duscope-walk` checks that
/// as it moves the item, through descriptors on Linux.
fn trash(item: &PlanItem, ident: (u64, u64)) -> Result<(), ErrorObject> {
    let path = Path::new(OsStr::from_bytes(&item.path_bytes));
    if item.kind == "dir" {
        no_mount_in(path)?;
    }
    duscope_walk::move_to_trash(path, ident).map_err(|e| trash_error(&item.path, e))
}

/// The error object of a move of the item at `path` to the Trash that
/// failed with `error`.
fn trash_error(path: &str, error: TrashError) -> ErrorObject {
    match error {
        TrashError::Changed { .. } => changed(path),
        TrashError::Unavailable(message) => error_object(
            "trash_unavailable",
            message,
            Some("Rerun duscope clean with --permanent to delete it: its volume has no Trash"),
            false,
            "user",
        ),
        TrashError::Failed(message) => error_object("io_error", message, None, false, "user"),
    }
}

/// `refused_path` when the mount table lists a mount point at the
/// directory `dir` or under it, or cannot be read: a move to the Trash
/// would take the volume along, mounted in the Trash.
fn no_mount_in(dir: &Path) -> Result<(), ErrorObject> {
    match duscope_walk::mount_point_under(dir) {
        Ok(None) => Ok(()),
        Ok(Some(point)) => Err(error_object(
            "refused_path",
            format!(
                "{} is a mount point in {}, and a move to the Trash would take its volume along, \
                 so it was not moved",
                point.display(),
                dir.display()
            ),
            Some(&format!(
                "Run umount {} first, then duscope clean again",
                point.display()
            )),
            false,
            "user",
        )),
        Err(e) => Err(error_object(
            "refused_path",
            format!(
                "cannot tell whether a volume is mounted in {} ({e}), and a move to the Trash \
                 would take one along, so it was not moved",
                dir.display()
            ),
            Some(UNKNOWN_MOUNTS),
            false,
            "user",
        )),
    }
}

/// The hint for a directory whose mounts the system cannot tell, which
/// neither the Trash nor a deletion acts on.
const UNKNOWN_MOUNTS: &str = "Run duscope clean again where the system's mount table can be read";

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
        RemoveError::MountUnknown { path, .. } => error_object(
            "refused_path",
            if path == item {
                format!("{error}, so nothing in it was deleted")
            } else {
                format!(
                    "{error}, so it and the directories above it in {} were left",
                    item.display()
                )
            },
            Some(UNKNOWN_MOUNTS),
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
