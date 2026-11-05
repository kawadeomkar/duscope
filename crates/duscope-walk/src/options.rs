//! What the caller asks of a walk.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use globset::{Glob, GlobSet};

use crate::IgnoreRules;

/// Which attribute set a scan collects.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Attrs {
    /// The times, sizes and identity that accounting and the common verbs need.
    #[default]
    Basic,
    /// The basic set plus access and creation times, mode and owner.
    Full,
}

/// What a walk counts beside its entries, for `-v` and `-vv`. Off, it reads
/// no clock and touches no counter for them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Counters {
    /// Nothing: a plain walk.
    #[default]
    Off,
    /// The workers' time listing directories apart from their time reading
    /// entries' attributes, with a provider that times them apart
    /// ([`Provider::times_listing`](crate::Provider::times_listing));
    /// nothing with one that cannot (`-v`).
    Listing,
    /// That, and each worker's entries and time in the provider's reads
    /// (`-vv`).
    Workers,
}

/// A cache-directory rule, compiled by the binary from duscope-core's
/// `caches.toml`: a directory whose name matches `name` and whose parent
/// listing holds one of `markers` is a cache.
#[derive(Debug, Clone)]
pub struct CacheRule {
    /// The directory name pattern.
    pub name: Glob,
    /// Marker files, any one of which confirms the match.
    pub markers: Vec<OsString>,
}

/// The directories of a home that `--skip-containers` lists without
/// entering: the app sandboxes of macOS and the containers app groups share,
/// which hold much of a home and whose access checks slow a scan.
pub const CONTAINERS: [&str; 2] = ["Library/Containers", "Library/Group Containers"];

/// The options of one walk.
#[derive(Debug, Clone)]
// One field per scan flag, as the crate contract names them.
#[allow(clippy::struct_excessive_bools)]
pub struct WalkOptions {
    /// Worker threads; 0 asks for the provider's default
    /// ([`Provider::default_threads`](crate::Provider::default_threads)).
    pub threads: usize,
    /// Follow symbolic links to directories and files.
    pub follow_symlinks: bool,
    /// Stay on the root's mount (`-x`).
    pub one_file_system: bool,
    /// Enter network filesystems.
    pub include_network: bool,
    /// The attribute set.
    pub attrs: Attrs,
    /// Collect clone ids where the filesystem has them.
    pub clones: bool,
    /// Paths to leave out (`--exclude`, `--exclude-from`), matched against
    /// each entry's path relative to the root, `/`-separated: a directory
    /// that matches is listed but not entered, a file is left out.
    pub excludes: GlobSet,
    /// Gitignore-syntax patterns to leave out (`--ignore-from`), matched the
    /// same way.
    pub ignore: IgnoreRules,
    /// Cache directories not to enter (`--exclude-caches`); when there are
    /// any, a directory holding a valid `CACHEDIR.TAG` is not entered either.
    pub exclude_caches: Vec<CacheRule>,
    /// The home directory whose [`CONTAINERS`] the walk lists without
    /// entering (`--skip-containers`); `None` enters them. They are known by
    /// their identity, so a walk that reaches them by another path (a
    /// firmlink, a link it follows) skips them too.
    pub skip_containers: Option<PathBuf>,
    /// Set to stop the walk. Workers poll it once per directory.
    pub cancel: Arc<AtomicBool>,
    /// Read every directory with the fallback provider, `readdir` and
    /// `fstatat`, instead of this platform's own (`--provider fallback`, a
    /// hidden flag for tests).
    pub fallback: bool,
    /// What the walk counts beside its entries, reported in the
    /// [`WalkSummary`](crate::WalkSummary).
    pub counters: Counters,
}

impl Default for WalkOptions {
    /// The options of a plain `duscope scan`.
    fn default() -> WalkOptions {
        WalkOptions {
            threads: 0,
            follow_symlinks: false,
            one_file_system: false,
            include_network: false,
            attrs: Attrs::Basic,
            clones: true,
            excludes: GlobSet::empty(),
            ignore: IgnoreRules::default(),
            exclude_caches: Vec::new(),
            skip_containers: None,
            cancel: Arc::default(),
            fallback: false,
            counters: Counters::Off,
        }
    }
}
