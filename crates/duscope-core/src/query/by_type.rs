//! `by-type`: bytes and counts by file class, with each class's largest
//! files.
//!
//! A file's class is that of the outermost directory above it that a
//! directory rule matches (a rule wins over the extension of every file
//! beneath it), else that of its extension in the classes table. A file
//! without an extension is `binaries` when the scan collected its mode and
//! an execute bit is set, `other` otherwise, as is an extension the table
//! does not list; a directory rule matching a directory above the root
//! applies too. A hard-linked file counts once, at the link charged with
//! it; a clone's allocated bytes count at the member charged with them,
//! its apparent bytes and the file itself at each member, as in the
//! totals. `other` is always the last row.

use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use crate::envelope::{ByTypeEntry, Example, name_fields};
use crate::rules::{Class, Classes, ext};

use super::{Filters, Page, SizeKind, TreeView, abs_path, pct};

/// What `by-type` was asked.
#[derive(Debug, Clone)]
pub struct ByTypeQuery<'c> {
    /// The classes table, a user's merged over the vendored one.
    pub classes: &'c Classes,
    /// `--examples`: the largest files listed per class.
    pub examples: usize,
    /// The shared filters, applied to files.
    pub filters: Filters,
    /// `--limit`: the classes listed; `None` lists all.
    pub limit: Option<usize>,
    /// The size classes are ranked and shown by.
    pub size: SizeKind,
}

/// What one class adds up to.
#[derive(Debug, Default, Clone)]
struct Tally {
    allocated: u64,
    apparent: u64,
    files: u64,
    /// The largest files: (size, id), largest first.
    largest: Vec<(u64, u32)>,
}

/// The file classes under the root of `view`.
pub fn by_type(view: &(impl TreeView + ?Sized), q: &ByTypeQuery<'_>) -> Page<ByTypeEntry> {
    if view.is_empty() {
        return Page {
            entries: Vec::new(),
            total_count: 0,
        };
    }
    let root = view.node(0);
    let tallies = tally(view, q);
    let root_total = root.size(q.size);
    let mut classes: Vec<Class> = Class::ALL
        .into_iter()
        .filter(|&class| tallies[index(class)].files > 0)
        .collect();
    let size_of = |class: Class| match q.size {
        SizeKind::Allocated => tallies[index(class)].allocated,
        SizeKind::Apparent => tallies[index(class)].apparent,
    };
    // Largest first, `other` last; legend order among equals.
    classes.sort_by_key(|&class| (class == Class::Other, std::cmp::Reverse(size_of(class))));
    let total_count = classes.len() as u64;
    if let Some(limit) = q.limit {
        classes.truncate(limit);
    }
    let entries = classes
        .into_iter()
        .map(|class| {
            let tally = &tallies[index(class)];
            ByTypeEntry {
                class: class.as_str(),
                allocated_bytes: tally.allocated,
                apparent_bytes: tally.apparent,
                pct_of_root: pct(size_of(class), root_total),
                file_count: tally.files,
                largest: tally
                    .largest
                    .iter()
                    .map(|&(_, id)| {
                        let node = view.node(id);
                        let (name, name_raw) = name_fields(&view.name(id));
                        Example {
                            path: String::from_utf8_lossy(&abs_path(view, id)).into_owned(),
                            name,
                            name_raw,
                            allocated_bytes: node.allocated,
                            apparent_bytes: node.apparent,
                        }
                    })
                    .collect(),
            }
        })
        .collect();
    Page {
        entries,
        total_count,
    }
}

/// The names of the directories above `root`, outermost first, which the
/// directory rules match with the names below them; and the class a rule
/// gives a directory among them, the outermost's, which everything under
/// the root then takes.
pub fn root_dirs<'p>(classes: &Classes, root: &'p Path) -> (Vec<&'p [u8]>, Option<Class>) {
    let mut components: Vec<&[u8]> = root
        .as_os_str()
        .as_bytes()
        .split(|&b| b == b'/')
        .filter(|c| !c.is_empty())
        .collect();
    components.pop();
    let class = (1..=components.len()).find_map(|end| classes.of_dir(&components[..end]));
    (components, class)
}

/// Each class's files under the root, by [`index`].
fn tally(view: &(impl TreeView + ?Sized), q: &ByTypeQuery<'_>) -> Vec<Tally> {
    let mut tallies: Vec<Tally> = vec![Tally::default(); Class::ALL.len()];
    let root = view.node(0);
    // The directory rules match a directory's last path components, the
    // root's own included.
    let (mut components, above) = root_dirs(q.classes, view.root_path());
    // The directories open above the current id: (last id, its class).
    let mut open: Vec<(u32, Option<Class>)> = vec![(u32::MAX, above)];
    for id in 0..=root.last_id {
        let node = view.node(id);
        while open.last().is_some_and(|&(last, _)| id > last) {
            open.pop();
            components.pop();
        }
        let inherited = open.last().and_then(|&(_, class)| class);
        if node.is_dir() {
            components.push(node.name);
            let class = inherited.or_else(|| q.classes.of_dir(&components));
            open.push((node.last_id, class));
            continue;
        }
        if node.linked_away || !q.filters.keeps(view, &node, q.size) {
            continue;
        }
        let class = inherited.unwrap_or_else(|| {
            let suffix = ext(node.name);
            if suffix.is_empty() {
                let executable = node.mode.is_some_and(|mode| mode & 0o111 != 0);
                if executable {
                    Class::Binaries
                } else {
                    Class::Other
                }
            } else {
                q.classes.of_ext(&suffix).unwrap_or(Class::Other)
            }
        });
        let tally = &mut tallies[index(class)];
        tally.allocated += node.counted(SizeKind::Allocated);
        tally.apparent += node.counted(SizeKind::Apparent);
        tally.files += 1;
        if q.examples > 0 {
            let size = node.counted(q.size);
            let at = tally
                .largest
                .partition_point(|&(s, other)| s > size || (s == size && other < id));
            if at < q.examples {
                tally.largest.insert(at, (size, id));
                tally.largest.truncate(q.examples);
            }
        }
    }
    tallies
}

/// A class's place in [`Class::ALL`].
fn index(class: Class) -> usize {
    Class::ALL.iter().position(|&c| c == class).unwrap_or(0)
}
