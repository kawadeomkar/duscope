//! `--roots`: the directories an MCP client may reach, and the checks every
//! tool's paths and saved scans go through.
//!
//! The roots are canonicalised once, at startup. A path a client sends is
//! refused (`refused_path`) when it holds a `..`, which could climb out of a
//! root, and otherwise resolved as the file system resolves it, links
//! included, before it is compared with the roots component by component: a
//! link inside a root that leads out of it is refused like the path it leads
//! to. A part of the path that does not exist (a file deleted since the scan)
//! is taken as written below the deepest part that does.
//!
//! A saved scan is named by its id or `latest`, never by an index file, so a
//! client cannot point a tool at a file outside the index directory. A saved
//! scan whose root lies under a root is visible as it is; one whose root lies
//! above a root is read at that root, as `--under` reads a subtree; any other
//! is invisible, as if it did not exist.

use std::fmt;
use std::io;
use std::path::{Component, Path, PathBuf};

use duscope_core::index::Index;
use duscope_core::query::resolve::MIN_PREFIX;
use duscope_core::query::{TreeView, index_files};

/// A path or scan a client may not reach, and why: the error a tool answers
/// with in place of running its verb.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    /// `refused_path`, or `scan_not_found` for a saved scan the roots hide.
    pub code: &'static str,
    /// One sentence.
    pub message: String,
    /// One imperative sentence naming what to do instead.
    pub hint: String,
}

impl Refusal {
    fn path(message: String) -> Refusal {
        Refusal {
            code: "refused_path",
            message,
            hint: "Name a path under one of the roots, or start duscope mcp with its directory in \
                   --roots"
                .to_owned(),
        }
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.message, self.hint)
    }
}

/// A `--roots` directory the server cannot serve.
#[derive(Debug)]
pub struct RootError {
    /// The directory as given.
    pub path: PathBuf,
    /// What is wrong with it.
    pub source: io::Error,
}

impl fmt::Display for RootError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "--roots {}: {}", self.path.display(), self.source)
    }
}

impl std::error::Error for RootError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// How a client sees a saved scan, by the scan's root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Visibility {
    /// The scan's root lies under a root (or is one): visible as it is.
    Inside,
    /// The scan's root lies above these roots: read at one of them.
    Above(Vec<PathBuf>),
    /// The scan's root shares nothing with the roots: invisible.
    Hidden,
}

/// A saved scan a client named, as the roots let it see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedScan {
    /// The scan's full id.
    pub id: String,
    /// The root it was taken at.
    pub root: PathBuf,
    /// How the roots let a client read it ([`Visibility::Hidden`] never
    /// comes back from [`Roots::saved_scan`]).
    pub visibility: Visibility,
}

/// The canonical `--roots` directories, without duplicates, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Roots {
    roots: Vec<PathBuf>,
}

impl Roots {
    /// The roots `paths` name, each canonicalised: every one must be a
    /// directory there is. A root given twice, or through a link to another,
    /// is kept once.
    pub fn new<P: AsRef<Path>>(paths: impl IntoIterator<Item = P>) -> Result<Roots, RootError> {
        let mut roots: Vec<PathBuf> = Vec::new();
        for path in paths {
            let path = path.as_ref();
            let failed = |source| RootError {
                path: path.to_owned(),
                source,
            };
            let canonical = path.canonicalize().map_err(failed)?;
            if !canonical.is_dir() {
                return Err(failed(io::Error::new(
                    io::ErrorKind::NotADirectory,
                    "not a directory",
                )));
            }
            if !roots.contains(&canonical) {
                roots.push(canonical);
            }
        }
        roots.sort();
        Ok(Roots { roots })
    }

    /// The roots, canonical and sorted.
    pub fn paths(&self) -> &[PathBuf] {
        &self.roots
    }

    /// The root a tool reads when the client names no path: the one root,
    /// or `None` when there are several.
    pub fn default_path(&self) -> Option<&Path> {
        match self.roots.as_slice() {
            [root] => Some(root),
            _ => None,
        }
    }

    /// The root that holds `path`, which must be canonical.
    pub fn root_of(&self, path: &Path) -> Option<&Path> {
        self.roots
            .iter()
            .filter(|root| path.starts_with(root))
            .max_by_key(|root| root.components().count())
            .map(PathBuf::as_path)
    }

    /// The canonical path `path` names, absolute or relative to `base` (a
    /// scan's root), refused when it holds a `..`, is relative with no base,
    /// or resolves outside every root.
    pub fn bound_path(&self, path: &Path, base: Option<&Path>) -> Result<PathBuf, Refusal> {
        let shown = path.display();
        if path.components().any(|c| c == Component::ParentDir) {
            return Err(Refusal::path(format!(
                "{shown} holds `..`, which a path a client sends may not"
            )));
        }
        let joined = if path.is_absolute() {
            path.to_owned()
        } else if let Some(base) = base {
            base.join(path)
        } else {
            return Err(Refusal::path(format!(
                "{shown} is relative, and there is no scan root to read it from"
            )));
        };
        let resolved = resolve(&joined);
        if self.root_of(&resolved).is_none() {
            let through = if resolved == joined {
                String::new()
            } else {
                format!(" (it resolves to {})", resolved.display())
            };
            return Err(Refusal::path(format!(
                "{shown} is outside the roots{through}"
            )));
        }
        Ok(resolved)
    }

    /// A scan selector a client sent: `latest`, or an id prefix of at least
    /// [`MIN_PREFIX`] hex digits. Anything else, an index file's path above
    /// all, is refused: a client reads the saved scans in the index
    /// directory, by id.
    pub fn scan_selector<'a>(&self, selector: &'a str) -> Result<&'a str, Refusal> {
        let is_id = selector.len() >= MIN_PREFIX
            && selector
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        if selector == "latest" || is_id {
            return Ok(selector);
        }
        Err(Refusal {
            code: "refused_path",
            message: format!(
                "scan_id {} is neither latest nor a scan id: a client names saved scans by id, \
                 never by an index file",
                selector.escape_debug()
            ),
            hint: "Call duscope_scans for the saved scans' ids".to_owned(),
        })
    }

    /// How a client sees a saved scan whose root is `scan_root`.
    pub fn visibility(&self, scan_root: &Path) -> Visibility {
        if self.root_of(scan_root).is_some() {
            return Visibility::Inside;
        }
        let below: Vec<PathBuf> = self
            .roots
            .iter()
            .filter(|root| root.starts_with(scan_root))
            .cloned()
            .collect();
        if below.is_empty() {
            Visibility::Hidden
        } else {
            Visibility::Above(below)
        }
    }

    /// The saved scan under `index_dir` whose id starts with `prefix`, as the
    /// roots show it: one they hide is `scan_not_found`, as one that does
    /// not exist; a prefix two visible scans share is refused too.
    pub fn saved_scan(&self, index_dir: &Path, prefix: &str) -> Result<SavedScan, Refusal> {
        let not_found = |message: String| Refusal {
            code: "scan_not_found",
            message,
            hint: "Call duscope_scans for the saved scans' ids, or duscope_scan to scan".to_owned(),
        };
        let mut found: Vec<SavedScan> = index_files(index_dir)
            .into_iter()
            .filter(|(id, _)| id.to_string().starts_with(prefix))
            .filter_map(|(id, file)| {
                let root = Index::open(&file).ok()?.root_path().to_owned();
                let visibility = self.visibility(&root);
                (visibility != Visibility::Hidden).then(|| SavedScan {
                    id: id.to_string(),
                    root,
                    visibility,
                })
            })
            .collect();
        match found.len() {
            0 => Err(not_found(format!("no saved scan {prefix} under the roots"))),
            1 => Ok(found.remove(0)),
            n => Err(Refusal {
                code: "usage",
                message: format!("{n} saved scans start with {prefix}"),
                hint: "Give more of the scan's id".to_owned(),
            }),
        }
    }
}

/// `path`, absolute and free of `..`, with its links resolved: the deepest
/// part that exists canonicalised, and the rest below it as written.
fn resolve(path: &Path) -> PathBuf {
    let mut existing = path;
    let mut rest = Vec::new();
    loop {
        if let Ok(canonical) = existing.canonicalize() {
            return rest
                .iter()
                .rev()
                .fold(canonical, |at, part: &&std::ffi::OsStr| at.join(part));
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name);
                existing = parent;
            }
            // `/` always canonicalises; a path that gets here is not absolute.
            _ => return path.to_owned(),
        }
    }
}

#[cfg(test)]
mod tests;
