//! `explain`'s git facts: the nearest repository above a path, whether the
//! path is ignored there (and precious, git's `$`-prefixed ignore syntax),
//! and whether any tracked file lies at or under it.
//!
//! The ignore files are those git reads: the repository's `info/exclude`,
//! the user's global file (`$XDG_CONFIG_HOME/git/ignore`), and each
//! `.gitignore` from the repository's root down to the path. A directory git
//! ignores hides everything under it, as git does not look inside. The
//! repository's index file is read only when a repository is found, so a
//! path outside one never reaches `gix-index`.

use std::cell::Cell;
use std::ffi::OsString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

use gix_ignore::glob::pattern::Case;
use gix_ignore::search::Ignore;

/// What git says of a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitFacts {
    /// The repository's working tree root.
    pub repo_root: PathBuf,
    /// Whether git ignores the path (or a directory above it).
    pub ignored: bool,
    /// Whether the ignoring pattern marks it precious (`$name`).
    pub precious: bool,
    /// Whether a tracked file lies at or under the path.
    pub tracked_inside: bool,
}

thread_local! {
    /// Index files this thread has read, for the test that a path outside
    /// a repository reads none.
    static INDEX_READS: Cell<usize> = const { Cell::new(0) };
}

/// How many index files this thread has read.
pub fn index_reads() -> usize {
    INDEX_READS.get()
}

/// The nearest directory at or above `path` holding `.git` (a directory, or
/// a file naming one).
pub fn repo_root(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .find(|dir| dir.join(".git").exists())
        .map(Path::to_owned)
}

/// The facts of `path` (absolute; a directory when `is_dir`), `None` when no
/// repository holds it. `env` reads a variable. A file git needs that cannot
/// be read is an error, which `explain` reports as low confidence.
pub fn facts(
    path: &Path,
    is_dir: bool,
    env: impl Fn(&str) -> Option<OsString>,
) -> Result<Option<GitFacts>, String> {
    let Some(root) = repo_root(path) else {
        return Ok(None);
    };
    let git_dir = git_dir(&root)?;
    let rel: Vec<&[u8]> = path
        .strip_prefix(&root)
        .map_err(|e| e.to_string())?
        .components()
        .filter_map(|c| match c {
            Component::Normal(name) => Some(name.as_bytes()),
            _ => None,
        })
        .collect();
    let (ignored, precious) = ignored(&root, &git_dir, &rel, is_dir, &env)?;
    let tracked_inside = tracked_inside(&git_dir, &rel)?;
    Ok(Some(GitFacts {
        repo_root: root,
        ignored,
        precious,
        tracked_inside,
    }))
}

/// The repository's git directory: `.git`, or the directory a `.git` file
/// names (`gitdir: <path>`, as a worktree or submodule has).
fn git_dir(root: &Path) -> Result<PathBuf, String> {
    let dot_git = root.join(".git");
    if dot_git.is_dir() {
        return Ok(dot_git);
    }
    let text =
        std::fs::read_to_string(&dot_git).map_err(|e| format!("{}: {e}", dot_git.display()))?;
    let named = text
        .lines()
        .find_map(|line| line.strip_prefix("gitdir:"))
        .map(str::trim)
        .ok_or_else(|| format!("{}: no gitdir line", dot_git.display()))?;
    Ok(root.join(named))
}

/// Whether the path at `rel` below `root` is ignored, and precious.
fn ignored(
    root: &Path,
    git_dir: &Path,
    rel: &[&[u8]],
    is_dir: bool,
    env: &impl Fn(&str) -> Option<OsString>,
) -> Result<(bool, bool), String> {
    let parse = Ignore {
        support_precious: true,
    };
    let global = env("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| env("HOME").map(|home| PathBuf::from(home).join(".config")))
        .map(|config| config.join("git").join("ignore"));
    let mut buf = Vec::new();
    let mut search = gix_ignore::Search::from_git_dir(git_dir, global, &mut buf, parse)
        .map_err(|e| format!("{}: {e}", git_dir.display()))?;
    // Each .gitignore from the root down to the path's directory.
    let mut dir = root.to_owned();
    let dirs = rel.len().saturating_sub(usize::from(!is_dir));
    for depth in 0..=dirs.min(rel.len()) {
        if depth > 0 {
            dir.push(std::ffi::OsStr::from_bytes(rel[depth - 1]));
        }
        let file = dir.join(".gitignore");
        match std::fs::read(&file) {
            Ok(text) => search
                .add_patterns_buffer(&text, file, Some(root), parse)
                .map_err(|e| e.to_string())?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("{}: {e}", file.display())),
        }
    }
    // A directory git ignores hides what is under it; else the path itself.
    let mut prefix = Vec::new();
    for (n, name) in rel.iter().enumerate() {
        if n > 0 {
            prefix.push(b'/');
        }
        prefix.extend_from_slice(name);
        let last = n + 1 == rel.len();
        let matched = search.pattern_matching_relative_path(
            prefix.as_slice().into(),
            Some(!last || is_dir),
            Case::Sensitive,
        );
        if let Some(m) = matched.filter(|m| !m.pattern.is_negative()) {
            return Ok((true, m.kind == gix_ignore::Kind::Precious));
        }
    }
    Ok((false, false))
}

/// Whether the repository's index tracks a file at or under `rel`.
fn tracked_inside(git_dir: &Path, rel: &[&[u8]]) -> Result<bool, String> {
    let file = git_dir.join("index");
    if !file.exists() {
        return Ok(false);
    }
    // SHA-256 repositories say so in their configuration.
    let config = std::fs::read_to_string(git_dir.join("config")).unwrap_or_default();
    let sha256 = config
        .lines()
        .map(|l| l.replace(' ', "").to_ascii_lowercase())
        .any(|l| l == "objectformat=sha256");
    let kind = if sha256 {
        gix_index::hash::Kind::Sha256
    } else {
        gix_index::hash::Kind::Sha1
    };
    INDEX_READS.set(INDEX_READS.get() + 1);
    let index = gix_index::File::at(&file, kind, true, gix_index::decode::Options::default())
        .map_err(|e| format!("{}: {e}", file.display()))?;
    let prefix = rel.join(&b'/');
    Ok(index.entries().iter().any(|entry| {
        let path: &[u8] = entry.path(&index);
        prefix.is_empty()
            || path == prefix.as_slice()
            || path
                .strip_prefix(prefix.as_slice())
                .is_some_and(|rest| rest.first() == Some(&b'/'))
    }))
}
