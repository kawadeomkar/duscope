//! `explain`'s git facts: the nearest repository above a path, whether the
//! path is ignored there (and precious, git's `$`-prefixed ignore syntax),
//! and whether any tracked file lies at or under it.
//!
//! The ignore files are those git reads: the repository's `info/exclude`
//! (the common directory's, for a linked worktree), `core.excludesFile` from
//! the repository's or the user's configuration (else git's default,
//! `$XDG_CONFIG_HOME/git/ignore`), and each `.gitignore` from the
//! repository's root down to the path, compared without case when
//! `core.ignoreCase` is set. A directory git ignores hides everything under
//! it, as git does not look inside. The
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
    // A linked worktree keeps its own index; the shared exclude file and
    // configuration are in the common directory.
    let common = std::fs::read_to_string(git_dir.join("commondir"))
        .map_or_else(|_| git_dir.clone(), |named| git_dir.join(named.trim()));
    let config = Config::read(&common, &env);
    let rel: Vec<&[u8]> = path
        .strip_prefix(&root)
        .map_err(|e| e.to_string())?
        .components()
        .filter_map(|c| match c {
            Component::Normal(name) => Some(name.as_bytes()),
            _ => None,
        })
        .collect();
    let (ignored, precious) = ignored(&root, &common, &config, &rel, is_dir)?;
    let tracked_inside = tracked_inside(&git_dir, &config, &rel)?;
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

/// What git's configuration says that these facts read: the ignore file
/// beside `info/exclude`, whether names compare without case, and the
/// object format.
#[derive(Debug, Default)]
struct Config {
    excludes_file: Option<PathBuf>,
    ignore_case: bool,
    sha256: bool,
}

impl Config {
    /// The repository's configuration (in its common directory), then the
    /// user's (`$XDG_CONFIG_HOME/git/config`, `~/.gitconfig`), the
    /// repository's winning; the excludes file defaults to
    /// `$XDG_CONFIG_HOME/git/ignore`.
    fn read(common: &Path, env: &impl Fn(&str) -> Option<OsString>) -> Config {
        let home = env("HOME").filter(|v| !v.is_empty()).map(PathBuf::from);
        let xdg = env("XDG_CONFIG_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|h| h.join(".config")));
        let read = |file: Option<PathBuf>| file.and_then(|f| std::fs::read_to_string(f).ok());
        let repo = read(Some(common.join("config"))).unwrap_or_default();
        let user = [
            read(home.as_ref().map(|h| h.join(".gitconfig"))),
            read(xdg.as_ref().map(|x| x.join("git").join("config"))),
        ];
        let value = |key: &str| {
            core_value(&repo, key)
                .or_else(|| user.iter().flatten().find_map(|t| core_value(t, key)))
        };
        let expand = |path: String| match (path.strip_prefix("~/"), &home) {
            (Some(rest), Some(home)) => home.join(rest),
            _ => PathBuf::from(path),
        };
        Config {
            excludes_file: value("excludesfile")
                .map(expand)
                .or_else(|| xdg.map(|x| x.join("git").join("ignore"))),
            ignore_case: value("ignorecase").is_some_and(|v| v.eq_ignore_ascii_case("true")),
            sha256: repo.lines().any(|line| {
                let line: String = line.chars().filter(|c| !c.is_whitespace()).collect();
                line.eq_ignore_ascii_case("objectformat=sha256")
            }),
        }
    }
}

/// `key` (lower case) in the `[core]` section of a git configuration file.
fn core_value(text: &str, key: &str) -> Option<String> {
    let mut core = false;
    let mut found = None;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            core = line
                .trim_start_matches('[')
                .trim_end_matches(']')
                .trim()
                .eq_ignore_ascii_case("core");
            continue;
        }
        if let (true, Some((name, value))) = (core, line.split_once('='))
            && name.trim().eq_ignore_ascii_case(key)
        {
            found = Some(value.trim().trim_matches('"').to_owned());
        }
    }
    found
}

/// Whether the path at `rel` below `root` is ignored, and precious.
fn ignored(
    root: &Path,
    common: &Path,
    config: &Config,
    rel: &[&[u8]],
    is_dir: bool,
) -> Result<(bool, bool), String> {
    let parse = Ignore {
        support_precious: true,
    };
    let mut buf = Vec::new();
    let mut search =
        gix_ignore::Search::from_git_dir(common, config.excludes_file.clone(), &mut buf, parse)
            .map_err(|e| format!("{}: {e}", common.display()))?;
    let case = if config.ignore_case {
        Case::Fold
    } else {
        Case::Sensitive
    };
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
            case,
        );
        if let Some(m) = matched.filter(|m| !m.pattern.is_negative()) {
            return Ok((true, m.kind == gix_ignore::Kind::Precious));
        }
    }
    Ok((false, false))
}

/// Whether the repository's index tracks a file at or under `rel`.
fn tracked_inside(git_dir: &Path, config: &Config, rel: &[&[u8]]) -> Result<bool, String> {
    let file = git_dir.join("index");
    if !file.exists() {
        return Ok(false);
    }
    // SHA-256 repositories say so in their configuration.
    let kind = if config.sha256 {
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

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;

    use duscope_testkit::TestEnv;

    use super::{Config, core_value};

    #[test]
    fn p2_qry_04_git_config_is_read_as_git_writes_it() {
        let text = "[core]\n\trepositoryformatversion = 1\n\tignorecase = true\n\
                    \texcludesFile = \"~/ignores\"\n[extensions]\n\tobjectformat = sha256\n";
        assert_eq!(core_value(text, "ignorecase").as_deref(), Some("true"));
        assert_eq!(
            core_value(text, "excludesfile").as_deref(),
            Some("~/ignores")
        );
        assert_eq!(core_value(text, "objectformat"), None);
        let env = TestEnv::new();
        let common = env.tmp_dir().join("git");
        fs::create_dir_all(&common).unwrap();
        fs::write(common.join("config"), text).unwrap();
        let home = env.home();
        let vars = |name: &str| match name {
            "HOME" => Some(OsString::from(&home)),
            _ => None,
        };
        let config = Config::read(&common, &vars);
        assert!(config.sha256 && config.ignore_case);
        assert_eq!(config.excludes_file, Some(home.join("ignores")));
        // Without a setting, git's default global file.
        fs::write(common.join("config"), "[core]\n\tbare = false\n").unwrap();
        let config = Config::read(&common, &vars);
        assert!(!config.sha256 && !config.ignore_case);
        assert_eq!(
            config.excludes_file,
            Some(home.join(".config").join("git").join("ignore"))
        );
    }
}
