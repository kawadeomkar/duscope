//! `explain`: facts about one path, each with where it came from, and how
//! sure they are; never a verdict.
//!
//! From a saved scan: the entry's row; `nlink` and `links_outside_subtree`
//! from `hardlinks` and `shared_members` (the links of the path's inodes
//! that lie outside it); the clone facts from `clones` and
//! `shared_members`; `freeable_now_bytes`, the bytes deleting the path
//! frees now (its total less the bytes a link or clone member outside it
//! keeps); `regenerable_by` from the cache rules with their markers in the
//! path's ancestors, and a valid `CACHEDIR.TAG`; git's facts. A path no
//! saved scan holds is answered from a fresh `stat`, with low confidence.
//!
//! `confidence` is `high` when every fact is exact and any rule matched with
//! its marker; `medium` for a bound rather than an exact figure, or a cache
//! name without its marker; `low` for a fresh `stat`, or a git lookup that
//! failed.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use duscope_walk::Flags;

use super::{TreeView, abs_path, flag_names, kind_name, timegrammar::iso8601};
use crate::CoreError;
use crate::envelope::{Explain, ExplainGit, name_fields};
use crate::git;
use crate::index::Index;
use crate::rules::Caches;

/// What `explain` reads besides the scan: the cache rules, and the
/// environment git's global ignore file is found through.
pub struct ExplainQuery<'a> {
    /// The cache rules, the user's merged over the vendored ones.
    pub caches: &'a Caches,
    /// Reads a variable.
    pub env: &'a dyn Fn(&str) -> Option<OsString>,
}

impl std::fmt::Debug for ExplainQuery<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExplainQuery").finish_non_exhaustive()
    }
}

/// The facts of entry `id` of `index`.
pub fn explain(index: &Index, id: u32, q: &ExplainQuery<'_>) -> Result<Explain, CoreError> {
    let corrupt = |e: rusqlite::Error| CoreError::IndexCorrupt {
        file: index.file().display().to_string(),
        message: e.to_string(),
    };
    let node = index.node(id);
    // Clone facts where the scan recorded clone ids: it counted clones (the
    // default), on a filesystem that has them (APFS). Elsewhere it does not
    // know, rather than knows none.
    let options: serde_json::Value =
        serde_json::from_str(&index.meta().options).unwrap_or_default();
    let clones = !index.meta().clones_unavailable && options["clones"].as_bool() != Some(false);
    let path_bytes = abs_path(index, id);
    let path = PathBuf::from(OsStr::from_bytes(&path_bytes));
    let (name, name_raw) = name_fields(&index.name(id));
    let table = if node.is_dir() { "dirs" } else { "files" };
    let (uid, gid): (Option<u32>, Option<u32>) = index
        .db()
        .query_row(
            &format!("SELECT uid, gid FROM {table} WHERE id = ?1"),
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(corrupt)?;
    let mut evidence = vec![format!(
        "index: scan {}, {table} row {id}",
        index.meta().scan_id
    )];
    let shared = Shared::read(index, id, node.last_id).map_err(corrupt)?;
    if shared.links_outside > 0 || !shared.link_groups.is_empty() {
        evidence.push(format!(
            "hardlinks: links outside the path {}, of inodes under it {}",
            shared.links_outside,
            shared.link_groups.len()
        ));
    }
    if !shared.clone_groups.is_empty() {
        evidence.push(format!(
            "clones: members outside the path {}, of clone groups under it {} \
             (members outside the scan are not known)",
            shared.clone_outside,
            shared.clone_groups.len()
        ));
    }
    let (freeable, freeable_kind) = freeable(index, &node, &shared, &mut evidence);
    let cache = node.is_dir().then(|| {
        cache_facts(q.caches, &path, |dir, marker| {
            holds_file(index, dir, marker)
        })
    });
    let (regenerable_by, cachedir_tag, cache_medium) = cache.unwrap_or((None, false, false));
    if let Some(cache) = cache_evidence(q.caches, &path, regenerable_by, cachedir_tag) {
        evidence.push(cache);
    }
    let (repository, git_failed) = git_facts(&path, node.is_dir(), q, &mut evidence);
    // A directory the scan listed but did not enter holds nothing in the
    // index: what is under it, and so what deleting it frees, is unknown.
    let entered = !node.is_dir() || index.entered(id);
    if !entered {
        evidence.push(
            "dirs: the scan listed this directory but did not enter it (an exclude, an \
             unreadable directory or a mount it did not cross)"
                .to_owned(),
        );
    }
    let index_failed = index.failure().is_some();
    let confidence = if git_failed || index_failed || !entered {
        "low"
    } else if freeable_kind != "exact" || cache_medium {
        "medium"
    } else {
        "high"
    };
    Ok(Explain {
        path: String::from_utf8_lossy(&path_bytes).into_owned(),
        path_bytes,
        name,
        name_raw,
        kind: kind_name(node.kind),
        apparent_bytes: node.apparent,
        allocated_bytes: node.allocated,
        file_count: node.is_dir().then_some(node.file_count),
        dir_count: node.is_dir().then_some(node.dir_count),
        mtime: iso8601(node.mtime_ns),
        ctime: iso8601(node.ctime_ns),
        atime: node.atime_ns.map(iso8601),
        btime: node.btime_ns.map(iso8601),
        mode: node.mode,
        uid,
        gid,
        nlink: (!node.is_dir()).then_some(u64::from(node.nlink)),
        links_outside_subtree: Some(shared.links_outside),
        is_clone: (clones && !node.is_dir()).then_some(shared.own_clone.is_some()),
        clone_group_size: shared.own_clone,
        clone_members_outside: clones.then_some(shared.clone_outside),
        freeable_now_bytes: entered.then_some(freeable),
        freeable_kind: entered.then_some(freeable_kind),
        regenerable_by,
        cachedir_tag,
        git: repository,
        flags: flag_names(&node),
        confidence,
        evidence,
        handle: None,
    })
}

/// The bytes deleting `node` frees now, and how exact that is: its total,
/// less what a link or clone member outside it keeps (the bytes of each such
/// group charged under it); an upper bound where blocks may be shared
/// without a clone id or a directory under it could not be read, a lower
/// bound where a link or member lies outside.
fn freeable(
    index: &Index,
    node: &super::Node,
    shared: &Shared,
    evidence: &mut Vec<String>,
) -> (u64, &'static str) {
    let total = if node.is_dir() || node.charged {
        node.allocated
    } else {
        0
    };
    let kept: u64 = shared
        .kept
        .iter()
        .map(|&first| index.node(first).allocated)
        .sum();
    let unreadable = index.unreadable_dirs(node.id, node.last_id);
    // Blocks shared with no clone id to say with which, or a clone member
    // that no longer shares every block: what it frees is less than its size.
    let may_share = node.flags.contains(Flags::MAY_SHARE)
        && (shared.own_clone.is_none() || !node.flags.contains(Flags::SHARES_ALL));
    let kind = if node.upper_bound || unreadable > 0 || may_share {
        "upper_bound"
    } else if shared.links_outside > 0 || shared.clone_outside > 0 {
        "lower_bound"
    } else {
        "exact"
    };
    if unreadable > 0 {
        evidence.push(format!(
            "dirs: {unreadable} unreadable directories under the path"
        ));
    }
    (total.saturating_sub(kept), kind)
}

/// The facts of `path`, which no saved scan holds, from a fresh `stat`: its
/// own size and times, never a subtree's; low confidence.
pub fn explain_unindexed(path: &Path, q: &ExplainQuery<'_>) -> Result<Explain, CoreError> {
    let meta = std::fs::symlink_metadata(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            CoreError::ScanNotFound(format!("{} does not exist", path.display()))
        } else {
            CoreError::ScanNotFound(format!("cannot stat {}: {e}", path.display()))
        }
    })?;
    let kind = if meta.is_dir() {
        "dir"
    } else if meta.is_file() {
        "file"
    } else if meta.file_type().is_symlink() {
        "symlink"
    } else {
        "other"
    };
    let is_dir = meta.is_dir();
    let name_bytes = path.file_name().unwrap_or(path.as_os_str()).as_bytes();
    let (name, name_raw) = name_fields(name_bytes);
    let mut evidence = vec!["stat: no saved scan holds the path".to_owned()];
    let nlink = meta.nlink();
    let allocated = meta.blocks().saturating_mul(512);
    // One link, its bytes are its own unless it is a clone, which a stat
    // cannot tell: an upper bound. More links: they keep the bytes.
    let (freeable, freeable_kind) = if is_dir {
        (None, None)
    } else if nlink > 1 {
        (Some(0), Some("lower_bound"))
    } else {
        (Some(allocated), Some("upper_bound"))
    };
    let (regenerable_by, cachedir_tag, _) = if is_dir {
        cache_facts(q.caches, path, fs_holds_file)
    } else {
        (None, false, false)
    };
    if let Some(cache) = cache_evidence(q.caches, path, regenerable_by, cachedir_tag) {
        evidence.push(cache);
    }
    let (git, _) = git_facts(path, is_dir, q, &mut evidence);
    let ns = |secs: i64, nsec: i64| secs.saturating_mul(1_000_000_000).saturating_add(nsec);
    let btime = meta.created().ok().and_then(|t| {
        let since = t.duration_since(std::time::UNIX_EPOCH).ok()?;
        i64::try_from(since.as_nanos()).ok()
    });
    let path_bytes = path.as_os_str().as_bytes().to_vec();
    Ok(Explain {
        path: String::from_utf8_lossy(&path_bytes).into_owned(),
        path_bytes,
        name,
        name_raw,
        kind,
        apparent_bytes: meta.len(),
        allocated_bytes: allocated,
        file_count: None,
        dir_count: None,
        mtime: iso8601(ns(meta.mtime(), meta.mtime_nsec())),
        ctime: iso8601(ns(meta.ctime(), meta.ctime_nsec())),
        atime: Some(iso8601(ns(meta.atime(), meta.atime_nsec()))),
        btime: btime.map(iso8601),
        mode: Some(meta.mode()),
        uid: Some(meta.uid()),
        gid: Some(meta.gid()),
        nlink: (!is_dir).then_some(nlink),
        links_outside_subtree: (!is_dir).then_some(nlink.saturating_sub(1)),
        is_clone: None,
        clone_group_size: None,
        clone_members_outside: None,
        freeable_now_bytes: freeable,
        freeable_kind,
        regenerable_by,
        cachedir_tag,
        git,
        flags: Vec::new(),
        confidence: "low",
        evidence,
        handle: None,
    })
}

/// The hard-link and clone groups of the files with ids `first ..= last`.
#[derive(Debug, Default)]
struct Shared {
    /// Each link group under the path: its first id, and its links under it.
    link_groups: BTreeMap<u32, u64>,
    /// Each clone group under the path: its first id, and its members under
    /// it.
    clone_groups: BTreeMap<u32, u64>,
    /// Links of those inodes outside the path.
    links_outside: u64,
    /// Members of those clone groups outside the path.
    clone_outside: u64,
    /// The size of the clone group of the entry itself, when it is a file
    /// in one.
    own_clone: Option<u64>,
    /// The groups' first ids under the path whose bytes an outside link or
    /// member keeps.
    kept: BTreeSet<u32>,
}

impl Shared {
    fn read(index: &Index, first: u32, last: u32) -> rusqlite::Result<Shared> {
        let db = index.db();
        let mut shared = Shared::default();
        let mut stmt = db.prepare(
            "SELECT link_first, clone_first FROM shared_members WHERE id BETWEEN ?1 AND ?2",
        )?;
        let rows = stmt.query_map([first, last], |row| {
            Ok((row.get::<_, Option<u32>>(0)?, row.get::<_, Option<u32>>(1)?))
        })?;
        for row in rows {
            let (link, clone) = row?;
            if let Some(g) = link {
                *shared.link_groups.entry(g).or_default() += 1;
            }
            if let Some(g) = clone {
                *shared.clone_groups.entry(g).or_default() += 1;
            }
        }
        let inside = |id: u32| (first..=last).contains(&id);
        // Each group's whole size in one pass over each table: the links an
        // inode has, and the members a clone group has in the scan.
        let links: BTreeMap<u32, u64> = db
            .prepare(
                "SELECT first_id, nlink FROM hardlinks WHERE first_id IN \
                 (SELECT link_first FROM shared_members WHERE id BETWEEN ?1 AND ?2)",
            )?
            .query_map([first, last], |row| {
                Ok((
                    row.get(0)?,
                    u64::try_from(row.get::<_, i64>(1)?).unwrap_or(0),
                ))
            })?
            .collect::<rusqlite::Result<_>>()?;
        for (&g, &n) in &shared.link_groups {
            let outside = links.get(&g).copied().unwrap_or(0).saturating_sub(n);
            shared.links_outside += outside;
            if outside > 0 && inside(g) {
                shared.kept.insert(g);
            }
        }
        let sizes: BTreeMap<u32, u64> = db
            .prepare(
                "SELECT clone_first, COUNT(*) FROM shared_members WHERE clone_first IN \
                 (SELECT clone_first FROM shared_members WHERE id BETWEEN ?1 AND ?2) \
                 GROUP BY clone_first",
            )?
            .query_map([first, last], |row| {
                Ok((
                    row.get(0)?,
                    u64::try_from(row.get::<_, i64>(1)?).unwrap_or(0),
                ))
            })?
            .collect::<rusqlite::Result<_>>()?;
        for (&g, &n) in &shared.clone_groups {
            let members = sizes.get(&g).copied().unwrap_or(n);
            let outside = members.saturating_sub(n);
            shared.clone_outside += outside;
            if outside > 0 && inside(g) {
                shared.kept.insert(g);
            }
            if first == last {
                shared.own_clone = Some(members);
            }
        }
        Ok(shared)
    }
}

/// Whether the directory at `dir` holds an entry `name` that is not a
/// directory, as a scan would list it: a symbolic link counts and is not
/// followed.
pub fn fs_holds_file(dir: &Path, name: &str) -> bool {
    dir.join(name)
        .symlink_metadata()
        .is_ok_and(|meta| !meta.is_dir())
}

/// Whether the directory at `dir` holds a file named `marker`, read from
/// the index where the scan holds `dir`, from the filesystem above its
/// root.
pub fn holds_file(index: &Index, dir: &Path, marker: &str) -> bool {
    match crate::resolve::resolve_in_index(index, dir) {
        Some(_) => crate::resolve::resolve_in_index(index, &dir.join(marker))
            .is_some_and(|id| !index.node(id).is_dir()),
        None => fs_holds_file(dir, marker),
    }
}

/// The cache facts of the directory at `path`: the command that regenerates
/// it when a rule confirms it, whether it holds a valid `CACHEDIR.TAG`, and
/// whether a rule's name matched without its marker. `has_marker(dir,
/// name)` says whether `dir` holds a file `name`; the ancestors are asked
/// nearest first ([`Caches::taking`]).
fn cache_facts(
    caches: &Caches,
    path: &Path,
    has_marker: impl Fn(&Path, &str) -> bool,
) -> (Option<&'static str>, bool, bool) {
    let tag = duscope_walk::cachedir_tag(path);
    let components: Vec<&[u8]> = path
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(name) => Some(name.as_bytes()),
            _ => None,
        })
        .collect();
    // The ancestor with k components is the (len - k)th; the 0th is the
    // path itself.
    let ancestors: Vec<&Path> = path.ancestors().collect();
    let holds = |k: usize, marker: &str| {
        ancestors
            .get(components.len() - k)
            .is_some_and(|dir| has_marker(dir, marker))
    };
    if let Some((rule, _)) = caches.taking(&components, holds, tag) {
        return (Some(rule.regenerable_by.as_str()), tag, false);
    }
    let name = path.file_name().map_or(&[][..], OsStr::as_bytes);
    (None, tag, caches.named(name).next().is_some())
}

/// The evidence line of the cache facts, when there is one.
fn cache_evidence(
    caches: &Caches,
    path: &Path,
    regenerable_by: Option<&str>,
    cachedir_tag: bool,
) -> Option<String> {
    let name = path.file_name().map_or(&[][..], OsStr::as_bytes);
    let shown = String::from_utf8_lossy(name);
    match (regenerable_by, cachedir_tag) {
        (Some(by), true) => Some(format!(
            "caches.toml: {shown}, a valid CACHEDIR.TAG; regenerated by {by}"
        )),
        (Some(by), false) => Some(format!(
            "caches.toml: rule {shown}, its marker in an ancestor; regenerated by {by}"
        )),
        (None, _) if caches.named(name).next().is_some() => Some(format!(
            "caches.toml: {shown} names a cache rule, but no marker is above it"
        )),
        (None, true) => Some("CACHEDIR.TAG: valid".to_owned()),
        (None, false) => None,
    }
}

/// Git's facts of `path`, and whether the lookup failed; evidence for each.
fn git_facts(
    path: &Path,
    is_dir: bool,
    q: &ExplainQuery<'_>,
    evidence: &mut Vec<String>,
) -> (Option<ExplainGit>, bool) {
    match git::facts(path, is_dir, q.env) {
        Ok(Some(facts)) => {
            evidence.push(format!(
                "git: the repository at {}",
                facts.repo_root.display()
            ));
            (
                Some(ExplainGit {
                    repo_root: facts.repo_root.to_string_lossy().into_owned(),
                    ignored: facts.ignored,
                    precious: facts.precious,
                    tracked_inside: facts.tracked_inside,
                }),
                false,
            )
        }
        Ok(None) => (None, false),
        Err(e) => {
            evidence.push(format!("git: the lookup failed: {e}"));
            (None, true)
        }
    }
}

#[cfg(test)]
mod tests;
