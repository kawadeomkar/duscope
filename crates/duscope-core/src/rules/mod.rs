//! The vendored rule tables: `classes.toml`, the file classes `by-type` and
//! the treemap use, and `caches.toml`, the cache rules `caches` and
//! `scan --exclude-caches` use. Both are data reviewed like code (a rule
//! change is a pull request of its own); their schemas are in
//! `docs/rules.md`.
//!
//! A user table (`--classes FILE`, `[classes] extra` and `[caches] extra` in
//! the config file) is read with the same loader and merged over the
//! vendored one.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::ops::Range;

use globset::{Glob, GlobMatcher};
use serde::Deserialize;

use crate::CoreError;

/// The vendored `classes.toml`.
pub const CLASSES_TOML: &str = include_str!("classes.toml");
/// The vendored `caches.toml`.
pub const CACHES_TOML: &str = include_str!("caches.toml");

/// The seven file classes, in the order the legend lists them; `Other` is
/// always last.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Class {
    /// Source code, WebAssembly modules, and what package managers install
    /// beside them.
    Code,
    /// Images, video, audio, fonts.
    Media,
    /// Compressed archives, disk images and installers.
    Archives,
    /// Databases, caches, blobs, structured data, VM disks.
    Data,
    /// Compiled objects, libraries, frameworks, symbols.
    Binaries,
    /// Text, documents, PDFs, mail and logs.
    Docs,
    /// No extension, an unknown one, and pooled remainders.
    Other,
}

impl Class {
    /// Every class, in legend order.
    pub const ALL: [Class; 7] = [
        Class::Code,
        Class::Media,
        Class::Archives,
        Class::Data,
        Class::Binaries,
        Class::Docs,
        Class::Other,
    ];

    /// The class as tables and the envelope spell it.
    pub fn as_str(self) -> &'static str {
        match self {
            Class::Code => "code",
            Class::Media => "media",
            Class::Archives => "archives",
            Class::Data => "data",
            Class::Binaries => "binaries",
            Class::Docs => "docs",
            Class::Other => "other",
        }
    }
}

impl fmt::Display for Class {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The suffixes kept whole by [`ext`]: `photo-dump.tar.gz` is `tar.gz`.
const WHOLE_SUFFIXES: [&str; 5] = ["tar.gz", "tar.bz2", "tar.xz", "tar.zst", "tar.lz4"];

/// The longest extension: a longer suffix is no extension at all.
const EXT_MAX: usize = 16;

/// A name's extension: its last suffix, lower-cased, at most 16 bytes of
/// `[A-Za-z0-9+~-]`, with the five `tar.*` suffixes kept whole. Empty for a
/// dotfile (`.bashrc`), an undotted name and a trailing dot.
pub fn ext(name: &[u8]) -> String {
    let lower = name.to_ascii_lowercase();
    for whole in WHOLE_SUFFIXES {
        if let Some(stem) = lower.strip_suffix(whole.as_bytes())
            && stem.len() > 1
            && stem.ends_with(b".")
        {
            return whole.to_owned();
        }
    }
    let Some(dot) = lower.iter().rposition(|&b| b == b'.') else {
        return String::new();
    };
    let suffix = &lower[dot + 1..];
    let valid = |b: &u8| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'~' | b'-');
    if dot == 0 || suffix.is_empty() || suffix.len() > EXT_MAX || !suffix.iter().all(valid) {
        return String::new();
    }
    String::from_utf8_lossy(suffix).into_owned()
}

/// A directory rule's pattern: one glob per path component, matched
/// against the last components of a directory's path.
#[derive(Debug, Clone)]
struct DirPattern {
    source: String,
    components: Vec<GlobMatcher>,
}

impl DirPattern {
    fn new(source: &str) -> Result<DirPattern, String> {
        let components = source
            .split('/')
            .map(|part| {
                if part.is_empty() {
                    return Err(format!("{source:?} has an empty path component"));
                }
                Glob::new(part)
                    .map(|glob| glob.compile_matcher())
                    .map_err(|e| format!("{source:?}: {e}"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(DirPattern {
            source: source.to_owned(),
            components,
        })
    }

    /// The sort key that puts the most specific pattern first.
    fn specificity(&self) -> (std::cmp::Reverse<usize>, usize) {
        let globbed = self
            .source
            .split('/')
            .filter(|part| part.contains(['*', '?', '[', '{', '\\']))
            .count();
        (std::cmp::Reverse(self.components.len()), globbed)
    }

    /// Whether the last components of `path` match, the last against the
    /// last.
    fn matches<C: AsRef<[u8]>>(&self, path: &[C]) -> bool {
        path.len() >= self.components.len()
            && self
                .components
                .iter()
                .rev()
                .zip(path.iter().rev())
                .all(|(glob, part)| glob.is_match(String::from_utf8_lossy(part.as_ref()).as_ref()))
    }
}

/// The class rules: the extension map and the directory rules.
#[derive(Debug, Clone)]
pub struct Classes {
    ext: BTreeMap<String, Class>,
    /// In precedence order: a user table's rules come before the vendored
    /// ones.
    dir: Vec<(DirPattern, Class)>,
}

/// A classes table as read, every key with its place in the text so an
/// error can name its line whatever the TOML shape (a dotted key, an
/// inline table, a byte-order mark).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClassesFile {
    #[serde(default)]
    ext: BTreeMap<toml::Spanned<String>, Class>,
    #[serde(default)]
    dir: BTreeMap<toml::Spanned<String>, Class>,
}

impl Classes {
    /// The vendored table.
    ///
    /// # Panics
    ///
    /// Never: this crate's tests load the vendored file.
    pub fn vendored() -> Classes {
        Classes::parse(CLASSES_TOML, "classes.toml").unwrap_or_else(|e| panic!("{e}"))
    }

    /// A table read from TOML text; `file` names it in errors.
    pub fn parse(text: &str, file: &str) -> Result<Classes, CoreError> {
        let parsed: ClassesFile = toml::from_str(text).map_err(|e| toml_error(text, file, &e))?;
        let mut ext_map = BTreeMap::new();
        for (key, class) in parsed.ext {
            let span = key.span();
            let key = key.into_inner();
            if key.is_empty() || ext(format!("x.{key}").as_bytes()) != key {
                return Err(span_error(
                    text,
                    file,
                    &span,
                    format!("{key:?} is not an extension duscope's ext rule can produce"),
                ));
            }
            ext_map.insert(key, class);
        }
        let mut dir = Vec::with_capacity(parsed.dir.len());
        for (key, class) in parsed.dir {
            let pattern = DirPattern::new(key.as_ref())
                .map_err(|message| span_error(text, file, &key.span(), message))?;
            dir.push((pattern, class));
        }
        // The most specific rule is tried first: more path components, then
        // fewer globbed ones; the sort is stable, so keys break ties.
        dir.sort_by_key(|(pattern, _)| pattern.specificity());
        Ok(Classes { ext: ext_map, dir })
    }

    /// This table with `user` merged over it: the user's extensions replace
    /// the same keys, and the user's directory rules are tried first.
    #[must_use]
    pub fn merged(mut self, user: Classes) -> Classes {
        self.ext.extend(user.ext);
        let mut dir = user.dir;
        dir.append(&mut self.dir);
        self.dir = dir;
        self
    }

    /// The class of an extension, as [`ext`] returns it; `None` when the
    /// table does not list it.
    pub fn of_ext(&self, ext: &str) -> Option<Class> {
        self.ext.get(ext).copied()
    }

    /// The class of a directory whose path, as components, is `path`; `None`
    /// when no directory rule matches.
    pub fn of_dir<C: AsRef<[u8]>>(&self, path: &[C]) -> Option<Class> {
        self.dir
            .iter()
            .find(|(pattern, _)| pattern.matches(path))
            .map(|&(_, class)| class)
    }

    /// The directory rules' keys, in precedence order.
    pub fn dir_rules(&self) -> impl Iterator<Item = &str> {
        self.dir.iter().map(|(pattern, _)| pattern.source.as_str())
    }

    /// The extension map.
    pub fn ext_rules(&self) -> impl Iterator<Item = (&str, Class)> {
        self.ext.iter().map(|(key, &class)| (key.as_str(), class))
    }
}

/// The fixed set of commands that regenerate a cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
pub enum Regenerable {
    /// `npm install`.
    #[serde(rename = "npm install")]
    Npm,
    /// `cargo build`.
    #[serde(rename = "cargo build")]
    Cargo,
    /// `pip install`.
    #[serde(rename = "pip install")]
    Pip,
    /// `gradle build`.
    #[serde(rename = "gradle build")]
    Gradle,
    /// `cmake --build`.
    #[serde(rename = "cmake --build")]
    Cmake,
    /// `pod install`.
    #[serde(rename = "pod install")]
    Pod,
    /// `composer install`.
    #[serde(rename = "composer install")]
    Composer,
    /// `xcodebuild`.
    #[serde(rename = "xcodebuild")]
    Xcodebuild,
    /// A cache whose tool is not known (a `CACHEDIR.TAG`).
    #[serde(rename = "unknown")]
    Unknown,
}

impl Regenerable {
    /// The command as the envelope spells it.
    pub fn as_str(self) -> &'static str {
        match self {
            Regenerable::Npm => "npm install",
            Regenerable::Cargo => "cargo build",
            Regenerable::Pip => "pip install",
            Regenerable::Gradle => "gradle build",
            Regenerable::Cmake => "cmake --build",
            Regenerable::Pod => "pod install",
            Regenerable::Composer => "composer install",
            Regenerable::Xcodebuild => "xcodebuild",
            Regenerable::Unknown => "unknown",
        }
    }
}

/// What confirms that a directory is the cache a rule names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confirm {
    /// One of these files in an ancestor of the directory.
    Markers(Vec<String>),
    /// The directory's parent path ends with these components.
    Under(Vec<String>),
    /// A valid `CACHEDIR.TAG` inside the directory, whatever its name.
    CachedirTag,
}

/// One cache rule.
#[derive(Debug, Clone)]
pub struct CacheRule {
    /// The directory name or glob; `None` for the `CACHEDIR.TAG` rule.
    pub name: Option<String>,
    /// What confirms the match.
    pub confirm: Confirm,
    /// What regenerates the cache.
    pub regenerable_by: Regenerable,
    matcher: Option<GlobMatcher>,
}

impl CacheRule {
    /// Whether a directory's name matches the rule's name; never for the
    /// `CACHEDIR.TAG` rule.
    pub fn name_matches(&self, name: &[u8]) -> bool {
        self.matcher
            .as_ref()
            .is_some_and(|m| m.is_match(String::from_utf8_lossy(name).as_ref()))
    }

    /// Whether the rule takes a directory whose path, as components, is
    /// `path`: `has_marker` says whether an ancestor holds a file of that
    /// name, `cachedir_tag` whether the directory holds a valid
    /// `CACHEDIR.TAG`.
    pub fn matches(
        &self,
        path: &[&[u8]],
        mut has_marker: impl FnMut(&str) -> bool,
        cachedir_tag: bool,
    ) -> bool {
        let Some((name, parent)) = path.split_last() else {
            return false;
        };
        match &self.confirm {
            Confirm::CachedirTag => cachedir_tag,
            Confirm::Markers(markers) => {
                self.name_matches(name) && markers.iter().any(|m| has_marker(m))
            }
            Confirm::Under(under) => {
                self.name_matches(name)
                    && parent.len() >= under.len()
                    && under
                        .iter()
                        .rev()
                        .zip(parent.iter().rev())
                        .all(|(u, p)| u.as_bytes() == *p)
            }
        }
    }
}

/// The cache rules.
#[derive(Debug, Clone)]
pub struct Caches {
    /// In precedence order: a user table's rules come first.
    rules: Vec<CacheRule>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CachesFile {
    #[serde(default)]
    rule: Vec<toml::Spanned<RuleFile>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuleFile {
    name: Option<String>,
    #[serde(default)]
    markers: Vec<String>,
    under: Option<String>,
    #[serde(default)]
    cachedir_tag: bool,
    regenerable_by: Regenerable,
}

impl Caches {
    /// The rules whose name matches `name`, whatever confirms them.
    pub fn named(&self, name: &[u8]) -> impl Iterator<Item = &CacheRule> {
        self.rules
            .iter()
            .filter(move |rule| rule.name_matches(name))
    }

    /// The vendored table.
    ///
    /// # Panics
    ///
    /// Never: this crate's tests load the vendored file.
    pub fn vendored() -> Caches {
        Caches::parse(CACHES_TOML, "caches.toml").unwrap_or_else(|e| panic!("{e}"))
    }

    /// A table read from TOML text; `file` names it in errors.
    pub fn parse(text: &str, file: &str) -> Result<Caches, CoreError> {
        let parsed: CachesFile = toml::from_str(text).map_err(|e| toml_error(text, file, &e))?;
        let mut rules = Vec::with_capacity(parsed.rule.len());
        for spanned in parsed.rule {
            let span = spanned.span();
            let rule = spanned.into_inner();
            let fail = |message: &str| CoreError::Rules {
                file: file.to_owned(),
                line: line_of(text, &span),
                message: message.to_owned(),
            };
            let confirm = match (
                rule.cachedir_tag,
                &rule.name,
                rule.markers.is_empty(),
                &rule.under,
            ) {
                (true, None, true, None) => Confirm::CachedirTag,
                (true, ..) => {
                    return Err(fail("a cachedir_tag rule takes no name, markers or under"));
                }
                (false, None, ..) => {
                    return Err(fail("a rule needs a name, or cachedir_tag = true"));
                }
                (false, Some(_), false, None) => Confirm::Markers(rule.markers),
                (false, Some(_), true, Some(under)) => {
                    let parts: Vec<String> = under.split('/').map(str::to_owned).collect();
                    if parts.iter().any(String::is_empty) {
                        return Err(fail("under has an empty path component"));
                    }
                    Confirm::Under(parts)
                }
                (false, Some(_), false, Some(_)) => {
                    return Err(fail("a rule takes markers or under, not both"));
                }
                (false, Some(_), true, None) => {
                    return Err(fail("a named rule needs markers or under"));
                }
            };
            if let Confirm::Markers(markers) = &confirm
                && markers.iter().any(|m| m.is_empty() || m.contains('/'))
            {
                return Err(fail("a marker is a file name"));
            }
            let matcher = match &rule.name {
                Some(name) if name.contains('/') || name.is_empty() => {
                    return Err(fail("a rule's name is one path component"));
                }
                Some(name) => Some(
                    Glob::new(name)
                        .map_err(|e| fail(&e.to_string()))?
                        .compile_matcher(),
                ),
                None => None,
            };
            rules.push(CacheRule {
                name: rule.name,
                confirm,
                regenerable_by: rule.regenerable_by,
                matcher,
            });
        }
        Ok(Caches { rules })
    }

    /// This table with `user`'s rules tried first.
    #[must_use]
    pub fn merged(mut self, user: Caches) -> Caches {
        let mut rules = user.rules;
        rules.append(&mut self.rules);
        self.rules = rules;
        self
    }

    /// The rules, in precedence order.
    pub fn rules(&self) -> &[CacheRule] {
        &self.rules
    }

    /// The first rule a directory matches: `path` is its path as
    /// components, `has_marker` says whether an ancestor of it holds a file
    /// of that name, and `cachedir_tag` whether it holds a valid
    /// `CACHEDIR.TAG`.
    pub fn matching(
        &self,
        path: &[&[u8]],
        mut has_marker: impl FnMut(&str) -> bool,
        cachedir_tag: bool,
    ) -> Option<&CacheRule> {
        self.rules
            .iter()
            .find(|rule| rule.matches(path, |m| has_marker(m), cachedir_tag))
    }

    /// The rules as the scanner takes them for `--exclude-caches`: a name
    /// and the markers it looks for in the directory's parent listing. Only
    /// rules confirmed by markers compile. A rule confirmed by an ancestor
    /// path (the one such rule, `DerivedData`, is Xcode's) has no marker the
    /// walker could check, and its name alone would exclude every directory
    /// so named; the `CACHEDIR.TAG` rule is the scanner's own check.
    pub fn compile(&self) -> Vec<duscope_walk::CacheRule> {
        self.rules
            .iter()
            .filter_map(|rule| match (&rule.confirm, &rule.matcher) {
                // The glob the loader compiled, not a second compile whose
                // failure would drop the rule.
                (Confirm::Markers(markers), Some(matcher)) => Some(duscope_walk::CacheRule {
                    name: matcher.glob().clone(),
                    markers: markers.iter().map(OsString::from).collect(),
                }),
                _ => None,
            })
            .collect()
    }
}

/// The 1-based line of the start of `span` in `text`.
pub(crate) fn line_of(text: &str, span: &Range<usize>) -> usize {
    // Bytes, not a str slice: a span need not fall on a char boundary.
    let before = &text.as_bytes()[..span.start.min(text.len())];
    before.split(|&b| b == b'\n').count()
}

/// A TOML parse error, with the line it points at.
fn toml_error(text: &str, file: &str, error: &toml::de::Error) -> CoreError {
    CoreError::Rules {
        file: file.to_owned(),
        line: error.span().map_or(1, |span| line_of(text, &span)),
        message: error.message().to_owned(),
    }
}

/// An invalid rule whose key is at `span` in `text`.
fn span_error(text: &str, file: &str, span: &Range<usize>, message: String) -> CoreError {
    CoreError::Rules {
        file: file.to_owned(),
        line: line_of(text, span),
        message,
    }
}

#[cfg(test)]
mod tests;
