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
//! it, as git does not look inside. The repository's index file is read
//! only when a repository is found, so a path outside one reads none.
//!
//! Each file is read through [`duscope_walk::read_regular`]: only a regular
//! file, and only up to a cap of its kind (64 KiB for a `.git` file and
//! `commondir`, 1 MiB per configuration file, 1 GiB for the index and a
//! shared index), and the ignore files of one lookup only up to 1 MiB
//! together, so a FIFO or a device in a repository cannot block `explain`
//! or fill its memory. Links are followed (a `~/.gitconfig` is often one),
//! except to an in-tree `.gitignore`, which git has not followed since
//! 2.32: a `.gitignore` that is a link reads as absent.

use std::cell::Cell;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

use gix_ignore::glob::pattern::Case;
use gix_ignore::search::Ignore;

/// The most read of a `.git` file and of `commondir`, each a path.
const DOT_GIT_CAP: u64 = 64 << 10;

/// The most read of each configuration file.
const CONFIG_CAP: u64 = 1 << 20;

/// The most read of the ignore files of one lookup, together: the excludes
/// file, `info/exclude` and each `.gitignore` from the repository's root
/// down to the path. One budget for the lookup, not a cap for each file:
/// parsed, short patterns take over 40 times their text, and a path can
/// have a `.gitignore` at every level, so at 16 MiB a file a lookup three
/// levels deep took 2 GB of memory. At this budget it takes about 45 MB at
/// most. Real ignore files are far smaller: most are under a kilobyte, and
/// a long one, Homebrew's `.gitignore`, is 7 KB, so a megabyte is over a
/// hundred times what a path's ignore files hold together. Git's own bound,
/// which skips a pattern file of 100 MiB or more, guards against abuse; it
/// is not a size a repository needs.
const IGNORE_BUDGET: u64 = 1 << 20;

/// The most read of the repository's index and of a split index's shared
/// index.
const INDEX_CAP: u64 = 1 << 30;

/// `path` read whole as text, following links, at most `cap` bytes.
fn read_text(path: &Path, cap: u64) -> std::io::Result<String> {
    String::from_utf8(duscope_walk::read_regular(path, cap, true)?)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// The ignore file at `file`, read within what is `left` of its lookup's
/// [`IGNORE_BUDGET`], which loses what was read. A file longer than what is
/// left is refused by its length, unopened, as `FileTooLarge`.
fn read_ignore(file: &Path, left: &mut u64, follow: bool) -> std::io::Result<Vec<u8>> {
    let text = duscope_walk::read_regular(file, *left, follow).map_err(|e| {
        if e.kind() == std::io::ErrorKind::FileTooLarge {
            std::io::Error::new(
                e.kind(),
                format!(
                    "with this file, the ignore files hold more than {IGNORE_BUDGET} bytes, \
                     the most read of them in one lookup"
                ),
            )
        } else {
            e
        }
    })?;
    *left = left.saturating_sub(u64::try_from(text.len()).unwrap_or(u64::MAX));
    Ok(text)
}

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
    let common = read_text(&git_dir.join("commondir"), DOT_GIT_CAP)
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
        read_text(&dot_git, DOT_GIT_CAP).map_err(|e| format!("{}: {e}", dot_git.display()))?;
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
        let read = |file: Option<PathBuf>| file.and_then(|f| read_text(&f, CONFIG_CAP).ok());
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
    // What is left of the lookup's budget for its ignore files.
    let mut left = IGNORE_BUDGET;
    // The excludes file, then `info/exclude`, as gix-ignore's
    // `Search::from_git_dir` adds them (it reads them unbounded); as there,
    // a file that is absent, a directory, or below a file that is not a
    // directory (a `~/.config/git` file) adds nothing.
    let mut search = gix_ignore::Search::default();
    let global = [
        config.excludes_file.clone(),
        Some(common.join("info").join("exclude")),
    ];
    for file in global.into_iter().flatten() {
        match read_ignore(&file, &mut left, true) {
            Ok(text) => search
                .add_patterns_buffer(&text, file, None, parse)
                .map_err(|e| e.to_string())?,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::NotFound
                        | std::io::ErrorKind::IsADirectory
                        | std::io::ErrorKind::NotADirectory
                ) => {}
            Err(e) => return Err(format!("{}: {e}", file.display())),
        }
    }
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
        // Not followed through a link, which reads as absent.
        let file = dir.join(".gitignore");
        match read_ignore(&file, &mut left, false) {
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
    let hash_len = if config.sha256 { 32 } else { 20 };
    let prefix = rel.join(&b'/');
    let mut inside = |path: &[u8]| {
        // A sparse index keeps a directory outside the sparse checkout as
        // one entry, `dir/`: what it holds is tracked.
        let dir = path.strip_suffix(b"/").unwrap_or(path);
        prefix.is_empty()
            || dir == prefix.as_slice()
            || path
                .strip_prefix(prefix.as_slice())
                .is_some_and(|rest| rest.first() == Some(&b'/'))
            || (path.ends_with(b"/") && prefix.starts_with(path))
    };
    let read = |file: &Path| {
        INDEX_READS.set(INDEX_READS.get() + 1);
        duscope_walk::read_regular(file, INDEX_CAP, true)
            .map_err(|e| format!("{}: {e}", file.display()))
    };
    let bytes = read(&file)?;
    let index = read_index(&bytes, hash_len, &mut inside)
        .map_err(|e| format!("{}: {e}", file.display()))?;
    match index {
        Index::Hit => Ok(true),
        Index::Miss { shared: None } => Ok(false),
        // A split index keeps most entries in the shared index it names.
        // Its deletions are not applied: a file removed since the split
        // still counts as tracked.
        Index::Miss { shared: Some(name) } => {
            let file = git_dir.join(format!("sharedindex.{name}"));
            let bytes = read(&file)?;
            read_index(&bytes, hash_len, &mut inside)
                .map(|index| matches!(index, Index::Hit))
                .map_err(|e| format!("{}: {e}", file.display()))
        }
    }
}

/// What an index file answered.
enum Index {
    /// An entry's path was the one asked for.
    Hit,
    /// None was; a split index names its shared index (the hash, in hex).
    Miss { shared: Option<String> },
}

/// Reads the entries of a git index file (git's `gitformat-index`,
/// versions 2 to 4) until `hit` answers true for a path: each entry's
/// path is in the fixed fields' wake, NUL-terminated and padded to a
/// multiple of eight bytes, except in version 4, which drops the padding
/// and stores each path as the bytes it keeps of the one before and its
/// own suffix. `hash_len` is the object format's: 20 bytes for SHA-1, 32
/// for SHA-256. The checksum is not verified.
fn read_index(
    bytes: &[u8],
    hash_len: usize,
    hit: &mut dyn FnMut(&[u8]) -> bool,
) -> Result<Index, String> {
    let truncated = || "the index file is truncated".to_owned();
    let u32_at = |at: usize| -> Result<u32, String> {
        bytes
            .get(at..at + 4)
            .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
            .ok_or_else(truncated)
    };
    if bytes.get(..4) != Some(b"DIRC".as_slice()) {
        return Err("not a git index file".to_owned());
    }
    let version = u32_at(4)?;
    if !(2..=4).contains(&version) {
        return Err(format!("index version {version}, of which 2 to 4 are read"));
    }
    let count = u32_at(8)?;
    let mut at = 12usize;
    let mut path: Vec<u8> = Vec::new();
    for _ in 0..count {
        // ctime, mtime, dev, ino, mode, uid, gid and size: ten 32-bit
        // fields; then the object name and the flags.
        let flags_at = at + 40 + hash_len;
        let flags = bytes
            .get(flags_at..flags_at + 2)
            .map(|b| u16::from_be_bytes([b[0], b[1]]))
            .ok_or_else(truncated)?;
        let mut name_at = flags_at + 2;
        // The extended flags, from version 3.
        if version >= 3 && flags & 0x4000 != 0 {
            name_at += 2;
        }
        if version == 4 {
            let (strip, used) = varint(bytes.get(name_at..).ok_or_else(truncated)?)
                .ok_or_else(|| "a bad path prefix in the index".to_owned())?;
            name_at += used;
            let keep = usize::try_from(strip)
                .ok()
                .and_then(|strip| path.len().checked_sub(strip))
                .ok_or_else(|| "a path prefix longer than the path before it".to_owned())?;
            path.truncate(keep);
            let end = nul(bytes, name_at).ok_or_else(truncated)?;
            path.extend_from_slice(&bytes[name_at..end]);
            at = end + 1;
        } else {
            let end = nul(bytes, name_at).ok_or_else(truncated)?;
            path.clear();
            path.extend_from_slice(&bytes[name_at..end]);
            // The entry, its name and one to eight NULs: a multiple of 8.
            at += (end - at + 8) & !7;
        }
        if hit(&path) {
            return Ok(Index::Hit);
        }
    }
    // The extensions, each a signature, a 32-bit length and its data, up
    // to the checksum; a split index's `link` starts with its shared
    // index's hash.
    let end = bytes.len().saturating_sub(hash_len);
    let mut shared = None;
    while at + 8 <= end {
        let len = usize::try_from(u32_at(at + 4)?).map_err(|e| e.to_string())?;
        if &bytes[at..at + 4] == b"link" {
            let hash = bytes.get(at + 8..at + 8 + hash_len).ok_or_else(truncated)?;
            shared = Some(hash.iter().fold(String::new(), |mut hex, b| {
                let _ = write!(hex, "{b:02x}");
                hex
            }));
        }
        at = at.saturating_add(8).saturating_add(len);
    }
    Ok(Index::Miss { shared })
}

/// The index of the first NUL at or after `from`.
fn nul(bytes: &[u8], from: usize) -> Option<usize> {
    let rest = bytes.get(from..)?;
    rest.iter().position(|&b| b == 0).map(|i| from + i)
}

/// Git's offset varint (`varint.c`): seven bits a byte, most significant
/// first, each continued byte adding one; the value and the bytes it took.
fn varint(bytes: &[u8]) -> Option<(u64, usize)> {
    let mut used = 0;
    let mut byte = *bytes.first()?;
    used += 1;
    let mut value = u64::from(byte & 0x7f);
    while byte & 0x80 != 0 {
        value = value.checked_add(1)?;
        if value >> 57 != 0 {
            return None;
        }
        byte = *bytes.get(used)?;
        used += 1;
        value = (value << 7) | u64::from(byte & 0x7f);
    }
    Some((value, used))
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::{Path, PathBuf};
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    use duscope_testkit::TestEnv;

    use super::{Config, GitFacts, IGNORE_BUDGET, INDEX_CAP, Index, core_value, facts, read_index};

    /// An entry's fixed fields: zeros, then its flags.
    fn fixed(hash_len: usize, flags: u16) -> Vec<u8> {
        let mut entry = vec![0u8; 40 + hash_len];
        entry.extend(flags.to_be_bytes());
        entry
    }

    fn header(version: u32, count: usize) -> Vec<u8> {
        let mut bytes = b"DIRC".to_vec();
        bytes.extend(version.to_be_bytes());
        bytes.extend(u32::try_from(count).unwrap().to_be_bytes());
        bytes
    }

    /// An index of `paths` in version 2 (or 3, the first entry with the
    /// extended flags), then `extensions` and a checksum of zeros.
    fn padded(version: u32, paths: &[&str], hash_len: usize, extensions: &[u8]) -> Vec<u8> {
        let mut bytes = header(version, paths.len());
        for (i, path) in paths.iter().enumerate() {
            let start = bytes.len();
            let extended = version == 3 && i == 0;
            let flags =
                u16::try_from(path.len().min(0xfff)).unwrap() | if extended { 0x4000 } else { 0 };
            bytes.extend(fixed(hash_len, flags));
            if extended {
                bytes.extend(0x2000u16.to_be_bytes());
            }
            bytes.extend(path.as_bytes());
            let len = bytes.len() - start;
            bytes.resize(start + ((len + 8) & !7), 0);
        }
        bytes.extend(extensions);
        bytes.extend(vec![0u8; hash_len]);
        bytes
    }

    /// Git's offset varint, as `varint.c` encodes it.
    fn varint(mut value: u64) -> Vec<u8> {
        let mut out = vec![u8::try_from(value & 0x7f).unwrap()];
        value >>= 7;
        while value != 0 {
            value -= 1;
            out.push(0x80 | u8::try_from(value & 0x7f).unwrap());
            value >>= 7;
        }
        out.reverse();
        out
    }

    /// An index of `paths` in version 4: each path as what it strips of the
    /// one before and its suffix.
    fn compressed(paths: &[&str]) -> Vec<u8> {
        let mut bytes = header(4, paths.len());
        let mut before: &[u8] = b"";
        for path in paths {
            let path = path.as_bytes();
            let common = before.iter().zip(path).take_while(|(a, b)| a == b).count();
            bytes.extend(fixed(20, 0));
            bytes.extend(varint((before.len() - common) as u64));
            bytes.extend(&path[common..]);
            bytes.push(0);
            before = path;
        }
        bytes.extend([0u8; 20]);
        bytes
    }

    /// Whether the index tracks a file at or under `rel`, as
    /// `tracked_inside` asks.
    fn tracks(bytes: &[u8], hash_len: usize, rel: &str) -> bool {
        let prefix = rel.as_bytes();
        let mut inside = |path: &[u8]| {
            path == prefix
                || path
                    .strip_prefix(prefix)
                    .is_some_and(|rest| rest.first() == Some(&b'/'))
                || (path.ends_with(b"/") && prefix.starts_with(path))
        };
        matches!(
            read_index(bytes, hash_len, &mut inside).unwrap(),
            Index::Hit
        )
    }

    #[test]
    fn p2_qry_04_index_files_of_each_version_are_read() {
        let paths = [
            "a/b.txt",
            "src/lib/a.rs",
            "src/lib/b.rs",
            "src/main.rs",
            "vendor/",
        ];
        for (name, bytes, hash_len) in [
            ("v2", padded(2, &paths, 20, &[]), 20),
            ("v3", padded(3, &paths, 20, &[]), 20),
            ("v2, SHA-256", padded(2, &paths, 32, &[]), 32),
            ("v4", compressed(&paths), 20),
        ] {
            for rel in [
                "a",
                "a/b.txt",
                "src",
                "src/lib",
                "src/main.rs",
                "vendor/x/y",
            ] {
                assert!(tracks(&bytes, hash_len, rel), "{name}: {rel}");
            }
            // By whole components only.
            for rel in ["a/b", "sr", "src/li", "tests"] {
                assert!(!tracks(&bytes, hash_len, rel), "{name}: {rel}");
            }
        }
        // A name of 0xfff bytes or more is read to its NUL.
        let long = "d/".to_owned() + &"x".repeat(5000);
        assert!(tracks(&padded(2, &[long.as_str(), "z"], 20, &[]), 20, "z"));
    }

    #[test]
    fn p2_qry_04_a_split_index_names_its_shared_index() {
        let mut link = b"link".to_vec();
        link.extend(20u32.to_be_bytes());
        link.extend((0u8..20).collect::<Vec<u8>>());
        let bytes = padded(2, &["new.txt"], 20, &link);
        let mut never = |_: &[u8]| false;
        match read_index(&bytes, 20, &mut never).unwrap() {
            Index::Miss { shared } => assert_eq!(
                shared.as_deref(),
                Some("000102030405060708090a0b0c0d0e0f10111213")
            ),
            Index::Hit => panic!("no entry was asked for"),
        }
    }

    #[test]
    fn p2_qry_04_a_damaged_index_is_an_error() {
        let mut any = |_: &[u8]| false;
        let good = padded(2, &["a.txt", "b.txt"], 20, &[]);
        assert!(read_index(&good[..good.len() - 30], 20, &mut any).is_err());
        assert!(read_index(b"DIRX\0\0\0\x02\0\0\0\0", 20, &mut any).is_err());
        let mut five = good.clone();
        five[7] = 5;
        assert!(read_index(&five, 20, &mut any).is_err());
        // A version-4 prefix longer than the path before it.
        let mut bad = header(4, 1);
        bad.extend(fixed(20, 0));
        bad.extend(varint(3));
        bad.extend(b"x\0");
        assert!(read_index(&bad, 20, &mut any).is_err());
    }

    /// Runs git in `dir`; false where git is missing or refuses (an object
    /// format it predates).
    fn git(env: &TestEnv, dir: &Path, args: &[&str]) -> bool {
        match env.command("git").args(args).current_dir(dir).output() {
            Ok(out) if out.status.success() => true,
            Ok(out) => {
                duscope_testkit::skip(&format!(
                    "git {args:?}: {}",
                    String::from_utf8_lossy(&out.stderr)
                ));
                false
            }
            Err(e) => {
                duscope_testkit::skip(&format!("no git here ({e})"));
                false
            }
        }
    }

    /// A repository case: its name, git init's arguments and the commands
    /// run after it.
    type Case<'a> = (&'a str, &'a [&'a str], &'a [&'a [&'a str]]);

    #[test]
    fn p2_qry_04_git_writes_indexes_this_reads() {
        let env = TestEnv::new();
        let home = env.home();
        let vars = |name: &str| match name {
            "HOME" => Some(OsString::from(&home)),
            _ => None,
        };
        // Version 4, a split index, and the SHA-256 object format.
        let cases: [Case<'_>; 3] = [
            (
                "v4",
                &["init", "-q"],
                &[&["update-index", "--index-version", "4"]],
            ),
            (
                "split",
                &["init", "-q"],
                &[&["update-index", "--split-index"]],
            ),
            ("sha256", &["init", "-q", "--object-format=sha256"], &[]),
        ];
        for (name, init, after) in cases {
            let repo = env.tmp_dir().join(name);
            fs::create_dir_all(repo.join("src/deep")).unwrap();
            fs::create_dir_all(repo.join("scratch")).unwrap();
            fs::write(repo.join("src/deep/a.rs"), b"a").unwrap();
            fs::write(repo.join("src/b.rs"), b"b").unwrap();
            fs::write(repo.join("scratch/notes"), b"n").unwrap();
            if !git(&env, &repo, init) || !git(&env, &repo, &["add", "src/deep/a.rs"]) {
                return;
            }
            for args in after {
                assert!(git(&env, &repo, args), "{name}: {args:?}");
            }
            // After the split, a new entry lands in the index itself and
            // the older one stays in the shared index.
            assert!(git(&env, &repo, &["add", "src/b.rs"]), "{name}");
            if name == "split" {
                let shared = fs::read_dir(repo.join(".git")).unwrap().any(|e| {
                    e.unwrap()
                        .file_name()
                        .to_string_lossy()
                        .starts_with("sharedindex.")
                });
                assert!(shared, "git wrote no shared index");
            }
            let repo = fs::canonicalize(&repo).unwrap();
            let tracked = |rel: &str, is_dir: bool| {
                facts(&repo.join(rel), is_dir, vars)
                    .unwrap()
                    .unwrap()
                    .tracked_inside
            };
            assert!(tracked("src", true), "{name}");
            assert!(tracked("src/deep", true), "{name}");
            assert!(tracked("src/b.rs", false), "{name}");
            assert!(!tracked("scratch", true), "{name}");
        }
    }

    /// How long `facts` may take before a test fails: a read blocked on a
    /// FIFO would otherwise hang the suite.
    const PATIENCE: Duration = Duration::from_secs(10);

    /// `facts` of the file `f` in `repo`, with `home` as `HOME`, on a
    /// thread of its own; the test fails if it has not returned within
    /// [`PATIENCE`]. A thread left blocked ends with the test process.
    fn facts_within(repo: &Path, home: &Path) -> Result<Option<GitFacts>, String> {
        let (path, home) = (repo.join("f"), home.to_owned());
        let (tx, rx) = mpsc::channel();
        let asked = path.clone();
        thread::spawn(move || {
            let vars = |name: &str| (name == "HOME").then(|| OsString::from(&home));
            let _ = tx.send(facts(&path, false, vars));
        });
        rx.recv_timeout(PATIENCE)
            .unwrap_or_else(|_| panic!("facts of {} blocked", asked.display()))
    }

    /// A repository whose git directory is made by hand (git is not
    /// needed), holding the file `f`.
    fn repo(env: &TestEnv, name: &str) -> PathBuf {
        let repo = env.tmp_dir().join(name);
        fs::create_dir_all(repo.join(".git").join("info")).unwrap();
        fs::write(repo.join("f"), b"f").unwrap();
        repo
    }

    fn mkfifo(env: &TestEnv, path: &Path) {
        let made = env.command("mkfifo").arg(path).status().unwrap();
        assert!(made.success(), "mkfifo {}", path.display());
    }

    /// The three ignore files a repository can hand `explain`: a
    /// `.gitignore` in its tree, its `info/exclude`, and the excludes file
    /// its own configuration names.
    const IGNORE_FILES: [&str; 3] = [".gitignore", "info/exclude", "core.excludesFile"];

    /// Where `repo` reads the ignore file `which` from, its configuration
    /// written to name it.
    fn ignore_file(repo: &Path, which: &str) -> PathBuf {
        match which {
            ".gitignore" => repo.join(".gitignore"),
            "info/exclude" => repo.join(".git").join("info").join("exclude"),
            _ => {
                let file = repo.join("ignores");
                let config = format!("[core]\n\texcludesFile = {}\n", file.display());
                fs::write(repo.join(".git").join("config"), config).unwrap();
                file
            }
        }
    }

    #[test]
    fn p5_sec_03_a_fifo_as_an_ignore_file_fails_the_lookup_without_blocking() {
        let env = TestEnv::new();
        for which in IGNORE_FILES {
            let repo = repo(&env, &which.replace(['/', '.'], "_"));
            let fifo = ignore_file(&repo, which);
            mkfifo(&env, &fifo);
            let error = facts_within(&repo, &env.home()).unwrap_err();
            assert!(
                error.starts_with(&format!("{}: ", fifo.display()))
                    && error.ends_with("not a regular file"),
                "{which}: {error}"
            );
        }
    }

    #[test]
    fn p5_sec_03_a_link_to_dev_zero_as_an_ignore_file_is_never_read() {
        let env = TestEnv::new();
        for which in IGNORE_FILES {
            let repo = repo(&env, &which.replace(['/', '.'], "_"));
            let link = ignore_file(&repo, which);
            symlink("/dev/zero", &link).unwrap();
            let found = facts_within(&repo, &env.home());
            if which == ".gitignore" {
                // Not followed: the repository has no .gitignore.
                assert!(!found.unwrap().unwrap().ignored, "{which}");
            } else {
                let error = found.unwrap_err();
                assert!(
                    error.starts_with(&format!("{}: ", link.display()))
                        && error.ends_with("not a regular file"),
                    "{which}: {error}"
                );
            }
        }
    }

    #[test]
    fn p5_sec_03_an_index_over_its_cap_fails_the_lookup_unread() {
        // Sparse files: they take no room on disk, and a read of either
        // would take a gibibyte of memory.
        let env = TestEnv::new();
        let repo = repo(&env, "index");
        let index = repo.join(".git").join("index");
        fs::File::create(&index)
            .unwrap()
            .set_len(INDEX_CAP + 1)
            .unwrap();
        let error = facts_within(&repo, &env.home()).unwrap_err();
        assert_eq!(
            error,
            format!(
                "{}: more than {INDEX_CAP} bytes, the most read of this file",
                index.display()
            )
        );
        // A split index's shared index has the same cap.
        let mut link = b"link".to_vec();
        link.extend(20u32.to_be_bytes());
        link.extend([0u8; 20]);
        fs::write(&index, padded(2, &["other"], 20, &link)).unwrap();
        let shared = repo
            .join(".git")
            .join(format!("sharedindex.{}", "00".repeat(20)));
        fs::File::create(&shared)
            .unwrap()
            .set_len(INDEX_CAP + 1)
            .unwrap();
        let error = facts_within(&repo, &env.home()).unwrap_err();
        assert!(
            error.starts_with(&format!("{}: more than", shared.display())),
            "{error}"
        );
    }

    #[test]
    fn p5_sec_03_ignore_files_past_one_lookups_budget_fail_it_unread() {
        // Five ignore files of a quarter of the budget each, far under what
        // one file could hold: the excludes file, `info/exclude` and a
        // `.gitignore` at each of the three levels above `a/b/f`.
        let env = TestEnv::new();
        let repo = repo(&env, "budget");
        let below = repo.join("a").join("b");
        fs::create_dir_all(&below).unwrap();
        fs::write(below.join("f"), b"f").unwrap();
        let quarter = usize::try_from(IGNORE_BUDGET / 4).unwrap();
        // `f`, then a comment to the quarter's length.
        let patterns = format!("f\n#{}\n", "-".repeat(quarter - 4));
        for file in [
            ignore_file(&repo, "core.excludesFile"),
            ignore_file(&repo, "info/exclude"),
            repo.join(".gitignore"),
            repo.join("a").join(".gitignore"),
        ] {
            fs::write(file, &patterns).unwrap();
        }
        // Four quarters meet the budget: all are read.
        assert!(facts_within(&below, &env.home()).unwrap().unwrap().ignored);
        // The fifth passes it and is refused by its length. Unreadable to
        // all but root, it shows it was never opened.
        let last = below.join(".gitignore");
        fs::write(&last, &patterns).unwrap();
        if duscope_walk::effective_uid() != 0 {
            fs::set_permissions(&last, fs::Permissions::from_mode(0o000)).unwrap();
        }
        let error = facts_within(&below, &env.home()).unwrap_err();
        assert_eq!(
            error,
            format!(
                "{}: with this file, the ignore files hold more than {IGNORE_BUDGET} bytes, \
                 the most read of them in one lookup",
                last.display()
            )
        );
    }

    #[test]
    fn p5_sec_03_an_ignore_file_below_a_regular_file_is_absent() {
        // `.git/info`, then `~/.config/git` in the test's own home, as a
        // regular file: `info/exclude` and the default excludes file lie
        // below a file and add nothing, as gix-ignore read them, and the
        // `.gitignore` still answers.
        for file in [".git/info", "home/.config/git"] {
            let env = TestEnv::new();
            let repo = repo(&env, "below_a_file");
            fs::write(repo.join(".gitignore"), "f\n").unwrap();
            let file = match file.strip_prefix("home/") {
                Some(rest) => env.home().join(rest),
                None => repo.join(file),
            };
            if file.is_dir() {
                fs::remove_dir(&file).unwrap();
            }
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(&file, b"").unwrap();
            let found = facts_within(&repo, &env.home());
            assert!(found.unwrap().unwrap().ignored, "{}", file.display());
        }
    }

    #[test]
    fn p5_sec_03_a_gitignore_that_is_a_link_is_absent() {
        let env = TestEnv::new();
        let repo = repo(&env, "linked");
        fs::write(repo.join("patterns"), "f\n").unwrap();
        symlink("patterns", repo.join(".gitignore")).unwrap();
        let found = facts_within(&repo, &env.home()).unwrap().unwrap();
        assert!(!found.ignored, "the link was followed");
        // The same patterns in the file itself ignore `f`.
        fs::remove_file(repo.join(".gitignore")).unwrap();
        fs::rename(repo.join("patterns"), repo.join(".gitignore")).unwrap();
        assert!(facts_within(&repo, &env.home()).unwrap().unwrap().ignored);
    }

    #[test]
    fn p5_sec_03_a_gitconfig_that_is_a_link_is_still_read() {
        // A configuration kept with the user's dotfiles and linked into the
        // home, naming an excludes file that ignores `f`.
        let env = TestEnv::new();
        let repo = repo(&env, "repo");
        let dotfiles = env.tmp_dir().join("dotfiles");
        fs::create_dir_all(&dotfiles).unwrap();
        fs::write(dotfiles.join("ignore"), "f\n").unwrap();
        let config = format!(
            "[core]\n\texcludesFile = {}\n",
            dotfiles.join("ignore").display()
        );
        fs::write(dotfiles.join("gitconfig"), config).unwrap();
        symlink(dotfiles.join("gitconfig"), env.home().join(".gitconfig")).unwrap();
        let found = facts_within(&repo, &env.home()).unwrap().unwrap();
        assert!(found.ignored, "the linked ~/.gitconfig was not read");
    }

    #[test]
    fn p5_sec_03_a_fifo_as_dot_git_commondir_or_config_does_not_block() {
        let env = TestEnv::new();
        // A `.git` file names the git directory: unreadable, the lookup
        // fails.
        let dot_git = env.tmp_dir().join("dot_git");
        fs::create_dir_all(&dot_git).unwrap();
        fs::write(dot_git.join("f"), b"f").unwrap();
        mkfifo(&env, &dot_git.join(".git"));
        let error = facts_within(&dot_git, &env.home()).unwrap_err();
        assert!(error.ends_with(".git: not a regular file"), "{error}");
        // `commondir`, and the configuration of the repository and of the
        // user: each is skipped as an unreadable one is, and the git
        // directory's own `info/exclude` still ignores `f`.
        let repo = repo(&env, "others");
        mkfifo(&env, &repo.join(".git").join("commondir"));
        mkfifo(&env, &repo.join(".git").join("config"));
        mkfifo(&env, &env.home().join(".gitconfig"));
        fs::write(repo.join(".git").join("info").join("exclude"), "f\n").unwrap();
        assert!(facts_within(&repo, &env.home()).unwrap().unwrap().ignored);
    }

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
