//! The expanded manifest: every entry of the tree, in path order, with its
//! kind, size, modes and timestamps resolved. Its text form is what the
//! manifest hash covers, so the hash changes exactly when the tree the
//! generator intends to build changes.

use std::collections::{BTreeSet, HashMap};
use std::fmt::Write as _;

use sha2::{Digest, Sha256};

use crate::manifest::{self, EntrySpec, GenerateSpec, Manifest, SizeSpec, decode, encode};
use crate::rng::Rng;
use crate::time::{self, Stamp};
use crate::{Error, Result};

/// The manifest format this generator expands.
pub const FORMAT: u32 = 1;

/// Version of the generated file content ([`crate::build::content`]).
/// Bump it when the content changes: compressed sizes depend on it. (The
/// hash also covers the source of `build.rs`, where the content is made, so
/// a forgotten bump still changes the hash.)
pub const CONTENT_VERSION: u32 = 1;

/// The source of the builder. How entries are made on disk is not in the
/// manifest, so the hash covers it: a change to the builder rebuilds every
/// tree and invalidates every cache keyed on the hash.
const BUILDER_SOURCE: &[u8] = include_bytes!("build.rs");

/// Default atimes fall within this many seconds (one year) before the
/// instant. Together with an instant later than any real build date, this
/// keeps every default atime in the real future, where Linux's `relatime`
/// leaves it alone when a scanner lists a directory or reads a
/// `CACHEDIR.TAG`. macOS moves the atime of a file that is read even then,
/// so the tree lives below a `.noindex` directory that Spotlight skips, and
/// verification reports such reads apart (`Report::read_after_build`).
pub const ATIME_WINDOW: i64 = 365 * 86_400;

/// What an entry is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A directory.
    Dir,
    /// A regular file: generated content of `size` bytes, or exactly `text`.
    File {
        /// Apparent size in bytes.
        size: u64,
        /// Explicit content, when the manifest gives it.
        text: Option<Vec<u8>>,
    },
    /// A sparse file of `size` apparent bytes with data only in `data`
    /// (offset, length) extents.
    Sparse {
        /// Apparent size in bytes.
        size: u64,
        /// Written extents.
        data: Vec<(u64, u64)>,
    },
    /// A file stored with filesystem compression where the platform has it.
    Compressed {
        /// Apparent size in bytes.
        size: u64,
    },
    /// A hard link to the file at `target` (a tree-relative path).
    Hardlink {
        /// The linked file.
        target: Vec<u8>,
    },
    /// A clone of the file at `source` where the filesystem can clone.
    Clone {
        /// The cloned file.
        source: Vec<u8>,
    },
    /// A symbolic link whose content is `target`.
    Symlink {
        /// The link's target, verbatim.
        target: Vec<u8>,
    },
    /// A FIFO.
    Fifo,
    /// A Unix-domain socket.
    Socket,
}

impl Kind {
    /// The one-word name used in the expanded manifest.
    pub fn name(&self) -> &'static str {
        match self {
            Kind::Dir => "dir",
            Kind::File { text: None, .. } => "file",
            Kind::File { text: Some(_), .. } => "text",
            Kind::Sparse { .. } => "sparse",
            Kind::Compressed { .. } => "compressed",
            Kind::Hardlink { .. } => "hardlink",
            Kind::Clone { .. } => "clone",
            Kind::Symlink { .. } => "symlink",
            Kind::Fifo => "fifo",
            Kind::Socket => "socket",
        }
    }
}

/// One entry of the tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Path relative to the tree root, `/`-separated bytes; empty for the
    /// root itself.
    pub path: Vec<u8>,
    /// What it is.
    pub kind: Kind,
    /// Permission bits at rest. Hard links carry their target's; symlinks
    /// carry 0 (the operating system decides their mode).
    pub mode: u32,
    /// Permission bits while a test environment holds the tree.
    pub test_mode: Option<u32>,
    /// Modification time.
    pub mtime: Stamp,
    /// Access time.
    pub atime: Stamp,
    /// Index of the case the entry belongs to, if any.
    pub case: Option<usize>,
}

impl Entry {
    /// The path relative to the tree root as printed (`.` for the root).
    pub fn display(&self) -> String {
        if self.path.is_empty() {
            ".".to_owned()
        } else {
            encode(&self.path)
        }
    }

    /// The parent's path (the root's parent is the root).
    pub fn parent(&self) -> &[u8] {
        match self.path.iter().rposition(|&b| b == b'/') {
            Some(i) => &self.path[..i],
            None => &[],
        }
    }

    /// Whether the path is valid UTF-8. Filesystems such as APFS refuse
    /// names that are not, so such entries may be absent.
    pub fn utf8(&self) -> bool {
        std::str::from_utf8(&self.path).is_ok()
    }
}

/// A case as the plan knows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaseInfo {
    /// The case's name.
    pub name: String,
    /// What it exercises.
    pub about: String,
    /// Its root, relative to the tree root.
    pub root: Vec<u8>,
}

/// The expanded manifest.
#[derive(Clone, Debug)]
pub struct Plan {
    /// The fixed instant, seconds since the epoch.
    pub instant: i64,
    /// The hash the manifest file records (`hash = ...`).
    pub recorded_hash: String,
    /// The cases, in manifest order.
    pub cases: Vec<CaseInfo>,
    /// Every entry, sorted by path bytes, so parents precede children.
    pub entries: Vec<Entry>,
    index: HashMap<Vec<u8>, usize>,
}

impl Plan {
    /// Expands a manifest.
    pub fn new(manifest: &Manifest) -> Result<Plan> {
        Expander::new(manifest)?.run()
    }

    /// The embedded manifest, expanded.
    pub fn embedded() -> Result<Plan> {
        Plan::new(&Manifest::embedded()?)
    }

    /// The instant as RFC 3339, the value tests export as `DUSCOPE_TEST_NOW`.
    pub fn instant_text(&self) -> String {
        time::format_instant(self.instant)
    }

    /// The entry at a tree-relative path.
    pub fn get(&self, path: &[u8]) -> Option<&Entry> {
        self.index.get(path).map(|&i| &self.entries[i])
    }

    /// The index in [`Plan::entries`] of the entry at a tree-relative path.
    pub fn position(&self, path: &[u8]) -> Option<usize> {
        self.index.get(path).copied()
    }

    /// A case by name.
    pub fn case(&self, name: &str) -> Option<(usize, &CaseInfo)> {
        self.cases.iter().enumerate().find(|(_, c)| c.name == name)
    }

    /// The expanded manifest as text: a header (with the SHA-256 of the
    /// builder's source), then one tab-separated line per entry (path, kind,
    /// mode, test mode, mtime, atime, case). This is what
    /// `mktree --print-plan` prints and what the hash covers.
    pub fn text(&self) -> String {
        let mut out = String::with_capacity(self.entries.len() * 80);
        let _ = writeln!(out, "mktree expanded manifest, format {FORMAT}");
        let _ = writeln!(out, "content {CONTENT_VERSION}");
        let _ = writeln!(out, "builder {}", hex(&Sha256::digest(BUILDER_SOURCE)));
        let _ = writeln!(out, "instant {}", self.instant_text());
        for case in &self.cases {
            let _ = writeln!(out, "case\t{}\t{}", case.name, encode(&case.root));
        }
        for e in &self.entries {
            let detail = match &e.kind {
                Kind::Dir | Kind::Fifo | Kind::Socket => String::new(),
                Kind::File { size, text: None } | Kind::Compressed { size } => size.to_string(),
                Kind::File { text: Some(t), .. } => encode(t),
                Kind::Sparse { size, data } => {
                    let extents: Vec<String> =
                        data.iter().map(|(o, l)| format!("{o}+{l}")).collect();
                    format!("{size} {}", extents.join(","))
                }
                Kind::Hardlink { target: path }
                | Kind::Clone { source: path }
                | Kind::Symlink { target: path } => encode(path),
            };
            let mode = match e.kind {
                Kind::Symlink { .. } => "-".to_owned(),
                _ => format!("{:04o}", e.mode),
            };
            let test_mode = e.test_mode.map_or("-".to_owned(), |m| format!("{m:04o}"));
            let case = e.case.map_or("-", |c| self.cases[c].name.as_str());
            let sep = if detail.is_empty() { "" } else { " " };
            let _ = writeln!(
                out,
                "{}\t{}{sep}{detail}\t{mode}\t{test_mode}\t{}\t{}\t{case}",
                e.display(),
                e.kind.name(),
                e.mtime.text(),
                e.atime.text(),
            );
        }
        out
    }

    /// The manifest hash: SHA-256 of [`Plan::text`], lower-case hex. It
    /// depends only on the manifest and the generator, never on the machine.
    pub fn hash(&self) -> String {
        hex(&Sha256::digest(self.text().as_bytes()))
    }

    /// The number of hard-link entries that point at each linked file.
    pub fn link_counts(&self) -> HashMap<&[u8], u64> {
        let mut counts: HashMap<&[u8], u64> = HashMap::new();
        for e in &self.entries {
            if let Kind::Hardlink { target } = &e.kind {
                *counts.entry(target.as_slice()).or_default() += 1;
            }
        }
        counts
    }
}

/// Lower-case hexadecimal.
pub fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// ASCII case folding: two paths that fold equal would collide on a
/// case-insensitive volume (APFS by default), so the plan refuses them.
fn fold(path: &[u8]) -> Vec<u8> {
    path.to_ascii_lowercase()
}

/// Checks a relative path: no empty, `.` or `..` components, no NUL.
fn check_relative(path: &[u8], what: &str) -> Result<()> {
    let ok = !path.is_empty()
        && !path.contains(&0)
        && path
            .split(|&b| b == b'/')
            .all(|c| !c.is_empty() && c != b"." && c != b"..");
    if ok {
        Ok(())
    } else {
        Err(Error::new(format!(
            "{what} {:?} is not a clean relative path",
            encode(path)
        )))
    }
}

fn join(base: &[u8], rel: &[u8]) -> Vec<u8> {
    if rel == b"." {
        return base.to_vec();
    }
    let mut out = Vec::with_capacity(base.len() + 1 + rel.len());
    out.extend_from_slice(base);
    if !base.is_empty() {
        out.push(b'/');
    }
    out.extend_from_slice(rel);
    out
}

struct Expander<'m> {
    manifest: &'m Manifest,
    instant: i64,
    default_age: i64,
    entries: Vec<Entry>,
    taken: HashMap<Vec<u8>, usize>,
    cases: Vec<CaseInfo>,
}

impl<'m> Expander<'m> {
    fn new(manifest: &'m Manifest) -> Result<Expander<'m>> {
        if manifest.format != FORMAT {
            return Err(Error::new(format!(
                "manifest format {} is not {FORMAT}",
                manifest.format
            )));
        }
        let instant = time::parse_instant(&manifest.instant)?;
        let default_age = time::parse_age(&manifest.defaults.age)?;
        let mut cases = Vec::with_capacity(manifest.cases.len());
        for spec in &manifest.cases {
            let root = decode(&spec.root)?;
            check_relative(&root, "case root")?;
            if cases.iter().any(|c: &CaseInfo| c.name == spec.name) {
                return Err(Error::new(format!("case {:?} appears twice", spec.name)));
            }
            cases.push(CaseInfo {
                name: spec.name.clone(),
                about: spec.about.clone(),
                root,
            });
        }
        Ok(Expander {
            manifest,
            instant,
            default_age,
            entries: Vec::new(),
            taken: HashMap::new(),
            cases,
        })
    }

    fn run(mut self) -> Result<Plan> {
        for (ci, spec) in self.manifest.cases.iter().enumerate() {
            for entry in &spec.entries {
                self.explicit(ci, entry).map_err(|e| {
                    Error::new(format!("case {:?}, entry {:?}: {e}", spec.name, entry.path))
                })?;
            }
        }
        // Parents of explicit entries first, so generated names avoid them.
        self.implicit_parents()?;
        for spec in &self.manifest.generators {
            self.generate(spec)
                .map_err(|e| Error::new(format!("generator at {:?}: {e}", spec.root)))?;
        }
        self.implicit_parents()?;
        let mut entries = self.entries;
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        let index = entries
            .iter()
            .enumerate()
            .map(|(i, e)| (e.path.clone(), i))
            .collect();
        let mut plan = Plan {
            instant: self.instant,
            recorded_hash: self.manifest.hash.clone(),
            cases: self.cases,
            entries,
            index,
        };
        resolve_links(&mut plan)?;
        Ok(plan)
    }

    /// mtime and atime for an age, optional nanoseconds and an optional
    /// atime age. The default atime age is half the age, at most
    /// [`ATIME_WINDOW`]: later than the mtime and within the window before
    /// the instant, so a `readdir` or read under `relatime` never moves it.
    fn times(&self, age: i64, nanos: u32, atime_age: Option<i64>) -> (Stamp, Stamp) {
        let mtime = Stamp {
            secs: self.instant - age,
            nanos,
        };
        let atime_age = atime_age.unwrap_or_else(|| (age / 2).min(ATIME_WINDOW));
        (mtime, Stamp::secs(self.instant - atime_age))
    }

    fn add(&mut self, entry: Entry) -> Result<()> {
        let key = fold(&entry.path);
        if let Some(&other) = self.taken.get(&key) {
            return Err(Error::new(format!(
                "{} collides with {} (paths may not differ only in ASCII case)",
                entry.display(),
                self.entries[other].display()
            )));
        }
        self.taken.insert(key, self.entries.len());
        self.entries.push(entry);
        Ok(())
    }

    #[allow(clippy::too_many_lines)] // one branch per entry kind, in manifest order
    fn explicit(&mut self, ci: usize, spec: &EntrySpec) -> Result<()> {
        let rel = decode(&spec.path)?;
        if rel != b"." {
            check_relative(&rel, "path")?;
        }
        let path = join(&self.cases[ci].root, &rel);
        let defaults = &self.manifest.defaults;
        let age = spec
            .age
            .as_deref()
            .map_or(Ok(self.default_age), time::parse_age)?;
        let atime_age = spec.atime.as_deref().map(time::parse_age).transpose()?;
        let nanos = spec.nanos.unwrap_or(0);
        if nanos >= 1_000_000_000 {
            return Err(Error::new("nanos must be below 1,000,000,000"));
        }
        let (mtime, atime) = self.times(age, nanos, atime_age);
        let kinds = [
            spec.dir.is_some(),
            spec.file.is_some(),
            spec.text.is_some(),
            spec.sparse.is_some(),
            spec.compressed.is_some(),
            spec.hardlink.is_some(),
            spec.clone.is_some(),
            spec.symlink.is_some(),
            spec.fifo.is_some(),
            spec.socket.is_some(),
            spec.many.is_some(),
            spec.chain.is_some(),
        ];
        if kinds.iter().filter(|&&k| k).count() != 1 {
            return Err(Error::new("exactly one kind field must be set"));
        }
        let resolve = |target: &str| -> Result<Vec<u8>> {
            let raw = decode(target)?;
            let resolved = match raw.strip_prefix(b"/") {
                Some(abs) => abs.to_vec(),
                None => join(&case_root(&self.cases, ci), &raw),
            };
            check_relative(&resolved, "link target")?;
            Ok(resolved)
        };
        let is_dir = spec.dir.is_some() || spec.many.is_some() || spec.chain.is_some();
        let mode = spec.mode.unwrap_or(if is_dir {
            defaults.dir_mode
        } else {
            defaults.file_mode
        });
        if spec.test_mode.is_some() && spec.dir.is_none() {
            return Err(Error::new("test_mode applies to directories only"));
        }
        // At rest the tree must stay walkable, archivable and removable (by
        // `cargo clean` too): denying modes belong in `test_mode`.
        let floor = if is_dir { 0o700 } else { 0o400 };
        if spec.symlink.is_none() && spec.hardlink.is_none() && mode & floor != floor {
            return Err(Error::new(format!(
                "mode {mode:04o} denies the owner; put it in test_mode"
            )));
        }
        let whole_seconds_only =
            spec.symlink.is_some() || spec.fifo.is_some() || spec.socket.is_some();
        if whole_seconds_only && nanos != 0 {
            return Err(Error::new(
                "symlinks, FIFOs and sockets take whole-second times",
            ));
        }
        let kind = if spec.dir.is_some() || spec.many.is_some() || spec.chain.is_some() {
            Kind::Dir
        } else if let Some(size) = &spec.file {
            Kind::File {
                size: size.bytes()?,
                text: None,
            }
        } else if let Some(text) = &spec.text {
            let bytes = text.as_bytes().to_vec();
            Kind::File {
                size: bytes.len() as u64,
                text: Some(bytes),
            }
        } else if let Some(size) = &spec.sparse {
            let size = size.bytes()?;
            let mut data = Vec::new();
            for extent in spec.data.iter().flatten() {
                let (o, l) = extent
                    .split_once('+')
                    .ok_or_else(|| Error::new(format!("extent {extent:?} is not offset+length")))?;
                let (o, l) = (manifest::parse_size(o)?, manifest::parse_size(l)?);
                if o + l > size || data.last().is_some_and(|&(po, pl)| po + pl > o) {
                    return Err(Error::new(format!(
                        "extent {extent:?} overlaps or overruns"
                    )));
                }
                data.push((o, l));
            }
            Kind::Sparse { size, data }
        } else if let Some(size) = &spec.compressed {
            Kind::Compressed {
                size: size.bytes()?,
            }
        } else if let Some(target) = &spec.hardlink {
            Kind::Hardlink {
                target: resolve(target)?,
            }
        } else if let Some(source) = &spec.clone {
            Kind::Clone {
                source: resolve(source)?,
            }
        } else if let Some(target) = &spec.symlink {
            Kind::Symlink {
                target: decode(target)?,
            }
        } else if spec.fifo.is_some() {
            Kind::Fifo
        } else {
            Kind::Socket
        };
        let mode = if matches!(kind, Kind::Symlink { .. }) {
            0
        } else {
            mode
        };
        self.add(Entry {
            path: path.clone(),
            kind,
            mode,
            test_mode: spec.test_mode,
            mtime,
            atime,
            case: Some(ci),
        })?;
        if let Some(count) = spec.many {
            let prefix = decode(spec.prefix.as_deref().unwrap_or("f"))?;
            let width = usize::try_from(spec.width.unwrap_or(6)).unwrap_or(6);
            let file_times = self.times(age, 0, atime_age);
            for i in 0..count {
                let mut name = prefix.clone();
                name.extend_from_slice(format!("{i:0width$}").as_bytes());
                self.add(Entry {
                    path: join(&path, &name),
                    kind: Kind::File {
                        size: 0,
                        text: None,
                    },
                    mode: defaults.file_mode,
                    test_mode: None,
                    mtime: file_times.0,
                    atime: file_times.1,
                    case: Some(ci),
                })?;
            }
        }
        if let Some(levels) = spec.chain {
            let name = decode(spec.name.as_deref().unwrap_or("d"))?;
            let mut at = path;
            for _ in 0..levels {
                at = join(&at, &name);
                self.add(Entry {
                    path: at.clone(),
                    kind: Kind::Dir,
                    mode,
                    test_mode: None,
                    mtime,
                    atime,
                    case: Some(ci),
                })?;
            }
            let leaf = decode(spec.leaf.as_deref().unwrap_or("leaf"))?;
            let size = spec.leaf_size.as_ref().map_or(Ok(0), SizeSpec::bytes)?;
            self.add(Entry {
                path: join(&at, &leaf),
                kind: Kind::File { size, text: None },
                mode: defaults.file_mode,
                test_mode: None,
                mtime,
                atime,
                case: Some(ci),
            })?;
        }
        Ok(())
    }

    /// A name under `parent` that no entry takes yet: `word.ext`, then
    /// `word-2.ext`, `word-3.ext`, …
    fn unique(&self, parent: &[u8], word: &str, ext: &str) -> Vec<u8> {
        let dot = if ext.is_empty() { "" } else { "." };
        let mut n = 1;
        loop {
            let name = if n == 1 {
                format!("{word}{dot}{ext}")
            } else {
                format!("{word}-{n}{dot}{ext}")
            };
            let path = join(parent, name.as_bytes());
            if !self.taken.contains_key(&fold(&path)) {
                return path;
            }
            n += 1;
        }
    }

    fn generate(&mut self, g: &GenerateSpec) -> Result<()> {
        let (ci, _) = self
            .cases
            .iter()
            .enumerate()
            .find(|(_, c)| c.name == g.case)
            .ok_or_else(|| Error::new(format!("unknown case {:?}", g.case)))?;
        let root = decode(&g.root)?;
        check_relative(&root, "generator root")?;
        let words = &self.manifest.words;
        if words.is_empty() || g.max_depth == 0 || g.empty_pct > 100 {
            return Err(Error::new(
                "needs words, max_depth >= 1 and empty_pct <= 100",
            ));
        }
        let mut classes = Vec::new();
        for (class, &weight) in &g.classes {
            let pool = self
                .manifest
                .extensions
                .get(class)
                .filter(|p| !p.is_empty())
                .ok_or_else(|| Error::new(format!("no extensions for class {class:?}")))?;
            classes.push((weight, pool));
        }
        let total: u32 = classes.iter().map(|(w, _)| w).sum();
        if total == 0 {
            return Err(Error::new("class weights sum to zero"));
        }
        let size_max = g.size_max.bytes()?;
        let age_min = time::parse_age(&g.age_min)?;
        let age_max = time::parse_age(&g.age_max)?;
        if age_max < age_min || age_min < 0 {
            return Err(Error::new("ages must satisfy 0 <= age_min <= age_max"));
        }
        let span = u64::try_from(age_max - age_min).unwrap_or(0);
        let defaults = &self.manifest.defaults;
        let mut rng = Rng::new(g.seed);
        let mut dirs: Vec<(Vec<u8>, u32)> = vec![(root, 0)];
        let stamp = |rng: &mut Rng, instant: i64| -> (Stamp, Stamp) {
            let age = age_min + i64::try_from(rng.skewed(span, g.age_skew)).unwrap_or(0);
            // atime: uniform in the last min(age, ATIME_WINDOW) before the
            // instant, so always later than the mtime (see `times`).
            let window = u64::try_from(age.min(ATIME_WINDOW)).unwrap_or(0);
            let atime_age = i64::try_from(rng.skewed(window, 1)).unwrap_or(0);
            (Stamp::secs(instant - age), Stamp::secs(instant - atime_age))
        };
        for _ in 0..g.dirs {
            let parent = loop {
                let p = rng.below(dirs.len());
                if dirs[p].1 < g.max_depth {
                    break p;
                }
            };
            let word = &words[rng.below(words.len())];
            let path = self.unique(&dirs[parent].0, word, "");
            let depth = dirs[parent].1 + 1;
            let (mtime, atime) = stamp(&mut rng, self.instant);
            self.add(Entry {
                path: path.clone(),
                kind: Kind::Dir,
                mode: defaults.dir_mode,
                test_mode: None,
                mtime,
                atime,
                case: Some(ci),
            })?;
            dirs.push((path, depth));
        }
        for _ in 0..g.files {
            let parent = rng.below(dirs.len());
            let mut pick = u32::try_from(rng.below(total as usize)).unwrap_or(0);
            let mut pool = classes[0].1;
            for (weight, p) in &classes {
                if pick < *weight {
                    pool = p;
                    break;
                }
                pick -= weight;
            }
            let ext = &pool[rng.below(pool.len())];
            let word = &words[rng.below(words.len())];
            let path = self.unique(&dirs[parent].0, word, ext);
            let size = if rng.below(100) < g.empty_pct as usize {
                0
            } else {
                rng.skewed(size_max, g.size_skew).max(1)
            };
            let (mtime, atime) = stamp(&mut rng, self.instant);
            self.add(Entry {
                path,
                kind: Kind::File { size, text: None },
                mode: defaults.file_mode,
                test_mode: None,
                mtime,
                atime,
                case: Some(ci),
            })?;
        }
        Ok(())
    }

    /// Adds every missing ancestor directory (and the root) with the default
    /// mode and age; refuses a non-directory used as a parent.
    fn implicit_parents(&mut self) -> Result<()> {
        let mut missing: BTreeSet<Vec<u8>> = BTreeSet::new();
        for e in &self.entries {
            let mut path = e.path.as_slice();
            loop {
                let parent = match path.iter().rposition(|&b| b == b'/') {
                    Some(i) => &path[..i],
                    None if path.is_empty() => break,
                    None => &[][..],
                };
                match self.taken.get(&fold(parent)) {
                    Some(&i) if self.entries[i].kind != Kind::Dir => {
                        return Err(Error::new(format!(
                            "{} is used as a directory but is not one",
                            self.entries[i].display()
                        )));
                    }
                    Some(_) => break,
                    None => {
                        missing.insert(parent.to_vec());
                    }
                }
                path = parent;
            }
        }
        let (mtime, atime) = self.times(self.default_age, 0, None);
        for path in missing {
            let case = self
                .cases
                .iter()
                .enumerate()
                .filter(|(_, c)| {
                    path == c.root || path.starts_with(&[c.root.as_slice(), b"/"].concat())
                })
                .max_by_key(|(_, c)| c.root.len())
                .map(|(i, _)| i);
            self.add(Entry {
                path,
                kind: Kind::Dir,
                mode: self.manifest.defaults.dir_mode,
                test_mode: None,
                mtime,
                atime,
                case,
            })?;
        }
        Ok(())
    }
}

fn case_root(cases: &[CaseInfo], ci: usize) -> Vec<u8> {
    cases[ci].root.clone()
}

/// Checks hard-link and clone targets and gives hard links their target's
/// mode and times (they share its inode).
fn resolve_links(plan: &mut Plan) -> Result<()> {
    let mut fixes = Vec::new();
    for (i, e) in plan.entries.iter().enumerate() {
        let (target, allowed): (&[u8], &[&str]) = match &e.kind {
            Kind::Hardlink { target } => {
                (target, &["file", "text", "sparse", "compressed", "clone"])
            }
            Kind::Clone { source } => (source, &["file", "text"]),
            _ => continue,
        };
        let t = plan.get(target).ok_or_else(|| {
            Error::new(format!(
                "{}: target {} is not in the manifest",
                e.display(),
                encode(target)
            ))
        })?;
        if !allowed.contains(&t.kind.name()) || !t.utf8() {
            return Err(Error::new(format!(
                "{}: cannot link to {} ({}, valid UTF-8 required)",
                e.display(),
                t.display(),
                t.kind.name()
            )));
        }
        if matches!(e.kind, Kind::Hardlink { .. }) {
            fixes.push((i, t.mode, t.mtime, t.atime));
        }
    }
    for (i, mode, mtime, atime) in fixes {
        let e = &mut plan.entries[i];
        e.mode = mode;
        e.mtime = mtime;
        e.atime = atime;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan_of(cases: &str) -> Result<Plan> {
        let text = format!(
            r#"
format = 1
hash = ""
instant = "2030-01-01T00:00:00Z"
words = ["alpha", "beta"]
[defaults]
dir_mode = 0o755
file_mode = 0o644
age = "30d"
[extensions]
code = ["rs", "py"]
{cases}
"#
        );
        Plan::new(&Manifest::parse(&text)?)
    }

    #[test]
    fn p0_test_01_plan_adds_parents_and_sorts() {
        let plan = plan_of(
            r#"
[[case]]
name = "c"
about = "x"
root = "cases/c"
entries = [
  { path = "z/file", file = "4K", age = "2d" },
  { path = "a", dir = true, mode = 0o700 },
]
"#,
        )
        .unwrap();
        let paths: Vec<String> = plan.entries.iter().map(Entry::display).collect();
        assert_eq!(
            paths,
            [
                ".",
                "cases",
                "cases/c",
                "cases/c/a",
                "cases/c/z",
                "cases/c/z/file"
            ]
        );
        let file = plan.get(b"cases/c/z/file").unwrap();
        assert_eq!(file.mtime.secs, 1_893_456_000 - 2 * 86_400);
        assert_eq!(file.atime.secs, 1_893_456_000 - 86_400);
        assert_eq!(plan.get(b"cases/c/a").unwrap().mode, 0o700);
        assert_eq!(plan.get(b"cases").unwrap().case, None);
        assert_eq!(plan.get(b"cases/c/z").unwrap().case, Some(0));
    }

    #[test]
    fn p0_test_01_plan_refuses_case_collisions() {
        let err = plan_of(
            r#"
[[case]]
name = "c"
about = "x"
root = "c"
entries = [ { path = "README", file = 1 }, { path = "readme", file = 1 } ]
"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("collides"), "{err}");
    }

    #[test]
    fn p0_test_01_plan_links_share_their_target() {
        let plan = plan_of(
            r#"
[[case]]
name = "c"
about = "x"
root = "c"
entries = [
  { path = "orig", file = "1K", mode = 0o600, age = "9d" },
  { path = "sub/link", hardlink = "orig" },
  { path = "abs", hardlink = "/c/orig" },
]
"#,
        )
        .unwrap();
        let link = plan.get(b"c/sub/link").unwrap();
        assert_eq!(
            link.kind,
            Kind::Hardlink {
                target: b"c/orig".to_vec()
            }
        );
        assert_eq!(link.mode, 0o600);
        assert_eq!(link.mtime, plan.get(b"c/orig").unwrap().mtime);
        assert_eq!(plan.link_counts()[b"c/orig".as_slice()], 2);
        assert!(
            plan_of(
                r#"
[[case]]
name = "c"
about = "x"
root = "c"
entries = [ { path = "link", hardlink = "missing" } ]
"#
            )
            .is_err()
        );
    }

    #[test]
    fn p0_test_01_plan_expands_many_and_chain() {
        let plan = plan_of(
            r#"
[[case]]
name = "c"
about = "x"
root = "c"
entries = [
  { path = "wide", many = 3, prefix = "f", width = 2 },
  { path = "deep", chain = 3, name = "d", leaf = "x", leaf_size = 5 },
]
"#,
        )
        .unwrap();
        for p in ["c/wide/f00", "c/wide/f02", "c/deep/d/d/d/x"] {
            assert!(plan.get(p.as_bytes()).is_some(), "{p}");
        }
        assert!(plan.get(b"c/wide/f03").is_none());
    }

    #[test]
    fn p0_test_01_generator_is_deterministic() {
        let gen_case = r#"
[[case]]
name = "g"
about = "x"
root = "g"
[[generate]]
case = "g"
root = "g/tree"
seed = 5
dirs = 20
files = 50
max_depth = 3
size_max = "8K"
size_skew = 2
empty_pct = 10
age_min = "1h"
age_max = "2y"
age_skew = 2
classes = { code = 1 }
"#;
        let a = plan_of(gen_case).unwrap();
        let b = plan_of(gen_case).unwrap();
        assert_eq!(a.text(), b.text());
        assert_eq!(a.entries.len(), 20 + 50 + 3);
        for e in &a.entries {
            assert!(e.atime >= e.mtime || e.kind == Kind::Dir && e.case.is_none());
        }
    }

    #[test]
    fn p0_test_01_text_names_every_field() {
        let plan = plan_of(
            r#"
[[case]]
name = "c"
about = "x"
root = "c"
entries = [ { path = "a b", file = 3, age = "1s", nanos = 5 } ]
"#,
        )
        .unwrap();
        let text = plan.text();
        assert!(
            text.contains(
                "c/a%20b\tfile 3\t0644\t-\t1893455999.000000005\t1893456000.000000000\tc\n"
            ),
            "{text}"
        );
        assert_eq!(plan.hash().len(), 64);
    }
}
