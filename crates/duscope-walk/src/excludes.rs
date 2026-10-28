//! What a scan leaves out: paths matching the `--exclude` globs, paths a
//! gitignore-syntax file ignores (`--ignore-from`, through `gix-ignore`),
//! and cache directories (`--exclude-caches`). An excluded directory is
//! listed but not entered ([`Skip::Excluded`], [`Skip::ExcludedCache`]); an
//! excluded file is left out of the listing.
//!
//! Globs and ignore patterns are matched against an entry's path relative to
//! the root, `/`-separated (`src/main.rs`); a link followed under
//! `--follow-symlinks` keeps the path it was reached by. A cache directory
//! is one whose name a [`CacheRule`] matches while its parent's listing
//! holds one of the rule's markers, or one holding a valid `CACHEDIR.TAG`:
//! a regular file whose first 43 bytes are the tag's signature. Those 43
//! bytes are the only file contents the scanner reads, and only in a
//! directory the walk would otherwise enter (never a mount it skips).

use std::collections::HashSet;
use std::ffi::OsStr;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use gix_ignore::glob::pattern::Case;
use globset::GlobMatcher;

use crate::{CacheRule, Entry, Kind, NameRef, Skip, WalkOptions};

/// Gitignore-syntax patterns (`--ignore-from`), matched against paths
/// relative to the scan root. The last pattern that matches decides, and a
/// negated one (`!keep.log`) keeps the path.
#[derive(Debug, Clone, Default)]
pub struct IgnoreRules {
    search: gix_ignore::Search,
}

impl IgnoreRules {
    /// No patterns.
    pub fn new() -> IgnoreRules {
        IgnoreRules::default()
    }

    /// Adds the patterns of one file: `text` is its contents, `source` its
    /// path. Patterns of a later file win over an earlier one's.
    pub fn add(&mut self, text: &[u8], source: impl Into<PathBuf>) -> io::Result<()> {
        self.search.add_patterns_buffer(
            text,
            source,
            None,
            gix_ignore::search::Ignore {
                support_precious: false,
            },
        )
    }

    /// Whether there are no patterns.
    pub fn is_empty(&self) -> bool {
        self.search
            .patterns
            .iter()
            .all(|list| list.patterns.is_empty())
    }

    /// Whether `rel`, a path relative to the root, is ignored.
    pub fn ignores(&self, rel: &[u8], is_dir: bool) -> bool {
        self.search
            .pattern_matching_relative_path(rel.into(), Some(is_dir), Case::Sensitive)
            .is_some_and(|m| !m.pattern.is_negative())
    }
}

/// A walk's exclusions, with the cache rules' names compiled once.
pub(crate) struct Excludes<'a> {
    opts: &'a WalkOptions,
    /// Each cache rule's name and markers.
    caches: Vec<(GlobMatcher, Vec<NameRef>)>,
}

impl<'a> Excludes<'a> {
    /// The exclusions `opts` asks for; `None` when it asks for none.
    pub(crate) fn new(opts: &'a WalkOptions) -> Option<Excludes<'a>> {
        let asked =
            !opts.excludes.is_empty() || !opts.ignore.is_empty() || !opts.exclude_caches.is_empty();
        asked.then(|| Excludes {
            opts,
            caches: opts
                .exclude_caches
                .iter()
                .map(|rule: &CacheRule| {
                    let markers = rule.markers.iter().map(|m| NameRef::from(m.as_os_str()));
                    (rule.name.compile_matcher(), markers.collect())
                })
                .collect(),
        })
    }

    /// Whether the cache rules need the listing of each directory.
    pub(crate) fn wants_listing(&self) -> bool {
        !self.caches.is_empty()
    }

    /// Whether matching needs each entry's path relative to the root: there
    /// are globs or ignore rules.
    pub(crate) fn wants_rel(&self) -> bool {
        !self.opts.excludes.is_empty() || !self.opts.ignore.is_empty()
    }

    /// Whether a glob or an ignore rule matches `rel`, a path relative to
    /// the root; `is_dir` is `None` for an entry of unknown kind, which
    /// matches a rule for either.
    pub(crate) fn matches(&self, rel: &[u8], is_dir: Option<bool>) -> bool {
        let ignore = &self.opts.ignore;
        self.opts
            .excludes
            .is_match(Path::new(OsStr::from_bytes(rel)))
            || match is_dir {
                Some(is_dir) => ignore.ignores(rel, is_dir),
                None => ignore.ignores(rel, false) || ignore.ignores(rel, true),
            }
    }

    /// Why `entry`, at `rel` below the root, is left out by what can be
    /// told without reading anything, if it is. `listing` is every name its
    /// directory listed, for the cache rules' markers. A directory the walk
    /// would enter is then a cache if it is [`tagged`](Self::tagged).
    pub(crate) fn excluded(
        &self,
        rel: &[u8],
        entry: &Entry,
        listing: Option<&HashSet<NameRef>>,
    ) -> Option<Skip> {
        let is_dir = entry.kind == Kind::Dir;
        if self.wants_rel() && self.matches(rel, Some(is_dir)) {
            return Some(Skip::Excluded);
        }
        let marked = is_dir
            && self.caches.iter().any(|(name, markers)| {
                name.is_match(entry.name.as_os_str())
                    && listing.is_some_and(|names| markers.iter().any(|m| names.contains(m)))
            });
        marked.then_some(Skip::ExcludedCache)
    }

    /// Whether a directory's `CACHEDIR.TAG` decides that it is a cache:
    /// under `--exclude-caches`.
    pub(crate) fn wants_tags(&self) -> bool {
        !self.opts.exclude_caches.is_empty()
    }

    /// Whether one of `names` may be a cache by its name, so the cache
    /// rules need the directory's listing for its markers; when none is,
    /// the listing is never collected.
    pub(crate) fn may_mark<'n>(&self, mut names: impl Iterator<Item = &'n NameRef>) -> bool {
        !self.caches.is_empty()
            && names.any(|name| {
                self.caches
                    .iter()
                    .any(|(glob, _)| glob.is_match(name.as_os_str()))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::IgnoreRules;

    #[test]
    fn p1_walk_09_ignore_rules_honour_negations_and_anchors() {
        let mut rules = IgnoreRules::new();
        assert!(rules.is_empty());
        rules
            .add(b"*.log\n!keep.log\n/build/\ndocs/*.tmp\n", "first")
            .unwrap();
        assert!(!rules.is_empty());
        assert!(rules.ignores(b"a.log", false));
        assert!(rules.ignores(b"deep/b.log", false));
        assert!(!rules.ignores(b"keep.log", false));
        assert!(!rules.ignores(b"deep/keep.log", false));
        // A trailing slash matches directories only; a leading one anchors.
        assert!(rules.ignores(b"build", true));
        assert!(!rules.ignores(b"build", false));
        assert!(!rules.ignores(b"src/build", true));
        assert!(rules.ignores(b"docs/x.tmp", false));
        assert!(!rules.ignores(b"other/docs/x.tmp", false));
        // A later file wins.
        rules.add(b"!a.log\n", "second").unwrap();
        assert!(!rules.ignores(b"a.log", false));
    }
}
