//! The command line: verbs, flags and their parsing.
//!
//! `duscope` alone, or with only the flags of `scan`, scans the current
//! directory. Flag names follow the conventions appendix: kebab-case,
//! GNU-style, short flags only where `du` users expect them, a `--no-` twin
//! only for a flag that is on by default, hidden test-only flags marked so.
//!
//! The doc comments of the argument structs are the help text, shown as
//! written, so they carry no Markdown.

// Help text is plain text, and a flag struct is a set of switches.
#![allow(clippy::doc_markdown, clippy::struct_excessive_bools)]

use std::ffi::OsString;
use std::path::PathBuf;

use clap::parser::ValueSource;
use clap::{ArgAction, ArgMatches, Args, Parser, Subcommand, ValueEnum};
use duscope_core::config::{Config, Setting};
use duscope_core::query::timegrammar;

use crate::log::Verbosity;
use crate::out::OutputArgs;
use crate::verbs::{Exit, Failure};

/// Fast, queryable disk-usage analyzer.
#[derive(Debug, Parser)]
#[command(
    name = "duscope",
    version,
    about = "Fast, queryable disk-usage analyzer",
    long_about = "Fast, queryable disk-usage analyzer: a parallel scanner, queries over\n\
                  its result, and a treemap in the terminal.\n\n\
                  Without a verb, duscope scans the given path, or the current directory.",
    // The flags before a verb are scan's, and main refuses them with a
    // verb; clap's own setting for that would refuse the output flags too,
    // which are global.
    override_usage = "duscope [OPTIONS] [PATH]\n       duscope <VERB>",
    subcommand_value_name = "VERB",
    subcommand_help_heading = "Verbs"
)]
pub struct Cli {
    /// The verb; `scan` when none is given.
    #[command(subcommand)]
    pub verb: Option<Verb>,
    /// `scan`'s arguments, for `duscope [PATH]` without a verb.
    #[command(flatten)]
    pub scan: ScanArgs,
    /// How the result is written, before or after the verb.
    #[command(flatten)]
    pub output: OutputArgs,
    /// The configuration file and what goes to stderr, before or after the
    /// verb.
    #[command(flatten)]
    pub global: GlobalArgs,
}

impl Cli {
    /// The list flags of the verb, for a verb that lists.
    pub fn query_args(&self) -> Option<&QueryArgs> {
        match &self.verb {
            Some(Verb::Top(a)) => Some(&a.query),
            Some(Verb::Tree(a)) => Some(&a.query),
            Some(Verb::ByType(a)) => Some(&a.query),
            Some(Verb::Stale(a)) => Some(&a.query),
            Some(Verb::Caches(a)) => Some(&a.query),
            _ => None,
        }
    }

    /// Whether the verb was asked for every row (`--all`).
    pub fn wants_all(&self) -> bool {
        match &self.verb {
            Some(Verb::Diff(a)) => a.all,
            _ => self.query_args().is_some_and(|q| q.all),
        }
    }
}

/// The flags every verb takes before or after its name, beside the output
/// flags: the configuration file and how much goes to stderr.
#[derive(Debug, Clone, Default, Args)]
#[command(next_help_heading = "Output")]
pub struct GlobalArgs {
    /// The configuration file, or none for the built-in defaults
    ///
    /// Default: DUSCOPE_CONFIG, else $XDG_CONFIG_HOME/duscope/config.toml
    #[arg(long, global = true, value_name = "FILE")]
    pub config: Option<OsString>,

    /// Print phase timings to stderr; -vv adds each volume and the workers
    #[arg(short = 'v', long, global = true, action = ArgAction::Count, conflicts_with = "quiet")]
    pub verbose: u8,

    /// Print each directory's errors to stderr as they happen
    #[arg(long, global = true, conflicts_with = "quiet")]
    pub debug: bool,

    /// Don't print warnings to stderr
    #[arg(short = 'q', long, global = true)]
    pub quiet: bool,

    /// Where saved scans are kept
    ///
    /// Default: $XDG_CACHE_HOME/duscope, else in the platform's cache directory
    #[arg(
        long,
        global = true,
        env = "DUSCOPE_INDEX_DIR",
        hide_env_values = true,
        value_name = "DIR"
    )]
    pub index_dir: Option<PathBuf>,
}

impl GlobalArgs {
    /// What `-q`, `-v`, `-vv` and `--debug` ask of the logger.
    pub fn verbosity(&self) -> Verbosity {
        if self.debug {
            Verbosity::Debug
        } else if self.quiet {
            Verbosity::Quiet
        } else {
            match self.verbose {
                0 => Verbosity::Normal,
                1 => Verbosity::Verbose,
                _ => Verbosity::VeryVerbose,
            }
        }
    }
}

/// The verbs and utilities.
#[derive(Debug, Subcommand)]
pub enum Verb {
    /// Scan a tree and show where its space goes
    Scan(ScanArgs),
    /// The largest directories or files under a path
    Top(TopArgs),
    /// A depth-limited tree with sizes and percentages
    Tree(TreeArgs),
    /// Bytes and counts by file class, with the largest examples
    ByType(ByTypeArgs),
    /// Large files not modified (or accessed) since a date
    Stale(StaleArgs),
    /// Regenerable caches (node_modules, target, .venv, …) and their projects
    Caches(CachesArgs),
    /// A scan's totals: per volume, as du counts them, and what it left unfound
    Summary(SummaryArgs),
    /// What changed between two saved scans of one tree
    Diff(DiffArgs),
    /// Facts about one path: links, clones, what deleting it frees, caches, git
    Explain(ExplainArgs),
    /// One read-only SQL statement over a saved scan's tables
    Query(SqlArgs),
    /// List, prune or check the saved scans
    Scans(ScansArgs),
    /// Print the manual page, in roff, to stdout
    Man,
    /// Print the completion script for a shell to stdout
    Completions(CompletionsArgs),
}

/// `explain`'s arguments.
#[derive(Debug, Clone, Args)]
pub struct ExplainArgs {
    /// The path (default: the current directory)
    #[arg(value_name = "PATH")]
    pub path: Option<PathBuf>,

    /// List the directories and entries a saved scan could not read
    #[arg(long, value_name = "SCAN", conflicts_with = "path")]
    pub errors: Option<String>,

    /// The saved scan to read: latest, an id prefix, or an index file
    ///
    /// Default: the newest saved scan that holds the path, else a fresh stat
    #[arg(long, value_name = "SCAN", help_heading = "Saved scans")]
    pub scan: Option<String>,

    /// How old a saved scan may be before a warning (h, d, w, y; default 7d)
    #[arg(long, value_name = "AGE", help_heading = "Saved scans")]
    pub max_age: Option<String>,

    /// The instant "now" is, for tests
    // test-only
    #[arg(long, hide = true, value_name = "RFC3339")]
    pub test_now: Option<String>,

    /// The configuration file's `[caches] extra`, merged over the vendored
    /// cache rules.
    #[arg(skip)]
    pub caches_extra: Option<PathBuf>,
}

/// `scans`' arguments.
#[derive(Debug, Clone, Args)]
pub struct ScansArgs {
    /// Remove saved scans past keeping, and stray temporary files
    ///
    /// Keeps the five newest scans of each root and any under seven days old
    #[arg(long, conflicts_with = "check")]
    pub prune: bool,

    /// Check a saved scan's file for damage
    #[arg(long, value_name = "SCAN")]
    pub check: Option<String>,

    /// The instant "now" is, for tests
    // test-only
    #[arg(long, hide = true, value_name = "RFC3339")]
    pub test_now: Option<String>,
}

/// Which saved scan a query verb reads, and whether it may scan instead.
#[derive(Debug, Clone, Default, Args)]
#[command(next_help_heading = "Saved scans")]
pub struct IndexArgs {
    /// The saved scan to read: latest, an id prefix, or an index file
    ///
    /// Default: the newest saved scan that holds the path, else a new scan
    #[arg(long, value_name = "SCAN")]
    pub scan: Option<String>,

    /// Don't save the scan run when no saved scan holds the path
    #[arg(long)]
    pub no_save: bool,

    /// Don't scan when no saved scan holds the path; fail instead
    #[arg(long, conflicts_with = "no_save")]
    pub no_scan: bool,

    /// How old a saved scan may be before a warning (h, d, w, y; default 7d)
    #[arg(long, value_name = "AGE")]
    pub max_age: Option<String>,
}

/// `completions`' arguments.
#[derive(Debug, Clone, Args)]
pub struct CompletionsArgs {
    /// The shell
    #[arg(value_enum, value_name = "SHELL")]
    pub shell: clap_complete::Shell,
}

/// How a scan reads the tree: the flags every verb takes for the scan it
/// runs.
#[derive(Debug, Clone, Args)]
#[command(next_help_heading = "Scan")]
pub struct WalkArgs {
    /// The tree to read (default: the current directory)
    #[arg(value_name = "PATH")]
    pub path: Option<PathBuf>,

    /// Worker threads (default: the platform's, from its thread sweep)
    #[arg(long, env = "DUSCOPE_THREADS", value_parser = parse_threads, value_name = "N")]
    pub threads: Option<usize>,

    /// Stay on the root's filesystem
    #[arg(short = 'x', long)]
    pub one_file_system: bool,

    /// Enter network filesystems (NFS, SMB, FUSE, …)
    #[arg(long)]
    pub include_network: bool,

    /// Follow symbolic links
    #[arg(long)]
    pub follow_symlinks: bool,

    /// The size shown: bytes on disk or apparent length
    #[arg(long, value_enum, value_name = "KIND")]
    pub size: Option<SizeKind>,

    /// Show apparent sizes (--size apparent)
    #[arg(long, conflicts_with = "size")]
    pub apparent_size: bool,

    /// The attributes to collect: full adds atime, btime, mode, uid and gid
    #[arg(long, value_enum, default_value_t = AttrsArg::Basic, value_name = "SET")]
    pub attrs: AttrsArg,

    /// Count APFS clones once (on by default)
    #[arg(long, overrides_with = "no_clones")]
    pub clones: bool,

    /// Count every clone in full
    #[arg(long, overrides_with = "clones")]
    pub no_clones: bool,

    /// Don't enter directories matching these patterns (read from FILE)
    #[arg(long, value_name = "FILE")]
    pub exclude_from: Vec<PathBuf>,

    /// Don't enter what these gitignore files ignore
    #[arg(long, value_name = "FILE")]
    pub ignore_from: Vec<PathBuf>,

    /// Don't enter cache directories (node_modules, target, .venv, …)
    #[arg(long)]
    pub exclude_caches: bool,

    /// Don't enter ~/Library/Containers (macOS)
    // The containers rule arrives in phase 5 (P5-WALK-03).
    #[arg(long, hide = true)]
    pub skip_containers: bool,

    /// Read every directory with this provider
    // test-only
    #[arg(long, value_enum, hide = true)]
    pub provider: Option<ProviderArg>,

    /// The instant "now" is, for tests
    // test-only
    #[arg(long, hide = true, value_name = "RFC3339")]
    pub test_now: Option<String>,

    /// The configuration file's `[excludes] patterns`, which every scan
    /// leaves out beside `--exclude`.
    #[arg(skip)]
    pub config_excludes: Vec<String>,

    /// The configuration file's `[classes] extra`, merged over the vendored
    /// classes unless `--classes` names a table.
    #[arg(skip)]
    pub classes_extra: Option<PathBuf>,

    /// The configuration file's `[caches] extra`, merged over the vendored
    /// cache rules.
    #[arg(skip)]
    pub caches_extra: Option<PathBuf>,
}

/// The flags of the list verbs: what to show and how to filter it.
#[derive(Debug, Clone, Args)]
#[command(next_help_heading = "Filters")]
pub struct QueryArgs {
    /// Rows to show
    #[arg(long, value_name = "N")]
    pub limit: Option<usize>,

    /// How deep to list (alias: --max-depth)
    #[arg(long, visible_alias = "max-depth", value_name = "N")]
    pub depth: Option<u32>,

    /// Leave out rows shallower than this
    #[arg(long, value_name = "N")]
    pub min_depth: Option<u32>,

    /// Leave out rows smaller than this (100M, 1.5G, 10KB, …)
    #[arg(long, value_parser = parse_size, value_name = "SIZE")]
    pub min_size: Option<u64>,

    /// Leave out rows larger than this
    #[arg(long, value_parser = parse_size, value_name = "SIZE")]
    pub max_size: Option<u64>,

    /// Rows untouched for at least this long (h, d, w, y)
    #[arg(long, value_name = "AGE")]
    pub older_than: Option<String>,

    /// Rows touched within this long
    #[arg(long, value_name = "AGE")]
    pub newer_than: Option<String>,

    /// Rows touched since this date (RFC 3339 or YYYY-MM-DD)
    #[arg(long, value_name = "DATE")]
    pub since: Option<String>,

    /// Rows touched before this date
    #[arg(long, value_name = "DATE")]
    pub until: Option<String>,

    /// The time the age flags measure
    #[arg(long, value_enum, default_value_t = TimeField::Mtime, value_name = "TIME")]
    pub time: TimeField,

    /// Rows of these kinds: file, dir, symlink, other
    #[arg(long = "type", value_enum, value_delimiter = ',', value_name = "KINDS")]
    pub kinds: Vec<KindArg>,

    /// The sort key
    #[arg(long, value_enum, value_name = "KEY")]
    pub sort: Option<SortKey>,

    /// Reverse the sort
    #[arg(short = 'r', long)]
    pub reverse: bool,

    /// Rows whose path matches this glob
    #[arg(long, value_name = "GLOB")]
    pub glob: Vec<String>,

    /// Rows whose path matches this regular expression
    #[arg(long, value_name = "REGEX")]
    pub regex: Option<String>,

    /// Show the rows --glob and --regex leave out instead
    #[arg(long)]
    pub invert: bool,

    /// Leave out rows whose path matches this glob
    #[arg(long, value_name = "GLOB")]
    pub exclude: Vec<String>,

    /// Where the next page starts: an envelope's next_cursor
    #[arg(long, value_name = "CURSOR", conflicts_with = "all")]
    pub cursor: Option<String>,

    /// Every row, with no --limit: as ndjson, unless --plain or --format
    #[arg(long, conflicts_with = "limit")]
    pub all: bool,

    /// Treat unreadable directories as a failure (exit 4)
    #[arg(long)]
    pub strict: bool,
}

/// `scan`'s flags.
#[derive(Debug, Clone, Args)]
pub struct ScanArgs {
    #[command(flatten)]
    pub walk: WalkArgs,

    /// Don't enter directories matching this glob
    #[arg(long, value_name = "GLOB", help_heading = "Scan")]
    pub exclude: Vec<String>,

    /// How deep the treemap is drawn (the scan reads the whole tree)
    #[arg(
        long,
        visible_alias = "max-depth",
        value_name = "N",
        help_heading = "Output"
    )]
    pub depth: Option<u32>,

    /// When to draw the treemap
    #[arg(long, value_enum, value_name = "WHEN", help_heading = "Output")]
    pub treemap: Option<TreemapWhen>,

    /// What colours the treemap's tiles
    ///
    /// lscolors takes LS_COLORS' *suffix= and ex= (with --attrs full) entries
    #[arg(long, value_enum, value_name = "BY", help_heading = "Output")]
    pub color_by: Option<ColorBy>,

    /// The colours: a theme name or the path of a theme file
    ///
    /// mocha|tokyo-night|dracula|gruvbox|light|terminal-native|no-color
    #[arg(
        long,
        env = "DUSCOPE_THEME",
        value_name = "THEME",
        value_parser = ThemeName,
        hide_possible_values = true,
        help_heading = "Output"
    )]
    pub theme: Option<OsString>,

    /// Pool files into their directory's tile
    #[arg(long, help_heading = "Output")]
    pub pool_files: bool,

    /// The smallest tile the treemap draws, in half-cell units
    #[arg(long, value_enum, value_name = "UNITS", help_heading = "Output")]
    pub min_tile: Option<MinTile>,

    /// Save the scan as an index and print its id
    #[arg(long, help_heading = "Output")]
    pub save: bool,

    /// Open the interactive viewer on the result
    // The viewer arrives in phase 4.
    #[arg(short = 'i', long, hide = true)]
    pub interactive: bool,
}

/// `top`'s flags.
#[derive(Debug, Clone, Args)]
pub struct TopArgs {
    #[command(flatten)]
    pub walk: WalkArgs,
    #[command(flatten)]
    pub query: QueryArgs,
    #[command(flatten)]
    pub index: IndexArgs,

    /// List files instead of directories
    #[arg(long, help_heading = "Top")]
    pub files: bool,

    /// Don't look through a directory that holds 80% of its parent
    #[arg(long, help_heading = "Top")]
    pub no_cut_through: bool,
}

/// `tree`'s flags.
#[derive(Debug, Clone, Args)]
pub struct TreeArgs {
    #[command(flatten)]
    pub walk: WalkArgs,
    #[command(flatten)]
    pub query: QueryArgs,
    #[command(flatten)]
    pub index: IndexArgs,

    /// Leave out rows below this share of their parent, in percent
    #[arg(long, value_name = "PCT", help_heading = "Tree")]
    pub min_pct: Option<f64>,
}

/// `by-type`'s flags.
#[derive(Debug, Clone, Args)]
pub struct ByTypeArgs {
    #[command(flatten)]
    pub walk: WalkArgs,
    #[command(flatten)]
    pub query: QueryArgs,
    #[command(flatten)]
    pub index: IndexArgs,

    /// A classes table merged over the built-in one
    #[arg(long, value_name = "FILE", help_heading = "By type")]
    pub classes: Option<PathBuf>,

    /// Largest examples per class
    #[arg(long, value_name = "N", help_heading = "By type")]
    pub examples: Option<usize>,
}

/// `stale`'s flags.
#[derive(Debug, Clone, Args)]
pub struct StaleArgs {
    #[command(flatten)]
    pub walk: WalkArgs,
    #[command(flatten)]
    pub query: QueryArgs,
    #[command(flatten)]
    pub index: IndexArgs,

    /// Also require the access time to be older (needs --attrs full)
    #[arg(long, help_heading = "Stale")]
    pub atime: bool,
}

/// `caches`' flags.
#[derive(Debug, Clone, Args)]
pub struct CachesArgs {
    #[command(flatten)]
    pub walk: WalkArgs,
    #[command(flatten)]
    pub query: QueryArgs,
    #[command(flatten)]
    pub index: IndexArgs,

    /// One row per project: the directory holding the caches' marker
    #[arg(long, help_heading = "Caches")]
    pub by_project: bool,

    /// Also list a cache's name without the marker that confirms it
    #[arg(long, help_heading = "Caches")]
    pub loose: bool,
}

/// `diff`'s arguments.
#[derive(Debug, Clone, Args)]
pub struct DiffArgs {
    /// The older saved scan: an id prefix, latest, or an index file
    #[arg(value_name = "OLD")]
    pub old: String,

    /// The newer saved scan
    #[arg(value_name = "NEW")]
    pub new: String,

    /// Compare this path, which both scans hold, rather than their roots
    #[arg(long, value_name = "PATH")]
    pub under: Option<PathBuf>,

    /// Leave out growth or shrinkage smaller than this (default 1M)
    #[arg(long, value_parser = parse_size, value_name = "SIZE")]
    pub min_delta: Option<u64>,

    /// Compare directories at this depth whole (alias: --max-depth)
    #[arg(long, visible_alias = "max-depth", value_name = "N")]
    pub depth: Option<u32>,

    /// Rows to show
    #[arg(long, value_name = "N")]
    pub limit: Option<usize>,

    /// Every row, with no --limit: as ndjson, unless --plain or --format
    #[arg(long, conflicts_with = "limit")]
    pub all: bool,

    /// The size compared: bytes on disk or apparent length
    #[arg(long, value_enum, value_name = "KIND")]
    pub size: Option<SizeKind>,
}

/// `query`'s flags.
#[derive(Debug, Clone, Args)]
pub struct SqlArgs {
    /// One SELECT over the scan's tables; without a LIMIT, 25 rows at most
    #[arg(long, value_name = "SQL")]
    pub sql: String,

    /// The most rows a LIMIT may return (default and ceiling 10000)
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..=10_000), value_name = "N")]
    pub max_rows: Option<u64>,

    /// Stop the statement after this many seconds (default 5, at most 60)
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..=60), value_name = "SECONDS")]
    pub timeout: Option<u64>,

    #[command(flatten)]
    pub walk: WalkArgs,
    #[command(flatten)]
    pub index: IndexArgs,
}

/// `summary`'s flags.
#[derive(Debug, Clone, Args)]
pub struct SummaryArgs {
    #[command(flatten)]
    pub walk: WalkArgs,
    #[command(flatten)]
    pub index: IndexArgs,
}

/// `--size`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SizeKind {
    /// Bytes allocated on disk
    Allocated,
    /// Apparent length
    Apparent,
}

/// `--attrs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum AttrsArg {
    /// Sizes, times, identity
    Basic,
    /// Basic plus atime, btime, mode, uid and gid
    Full,
}

/// `--time`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum TimeField {
    /// Modification time
    Mtime,
    /// Access time (needs --attrs full)
    Atime,
    /// Inode change time
    Ctime,
    /// Creation time (needs --attrs full)
    Btime,
}

/// `--type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum KindArg {
    /// Regular files
    File,
    /// Directories
    Dir,
    /// Symbolic links
    Symlink,
    /// Sockets, pipes and devices
    Other,
}

/// `--sort`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SortKey {
    /// Size, largest first
    Size,
    /// Entry count
    Count,
    /// Modification time
    Mtime,
    /// Path, bytewise
    Path,
}

/// `--treemap`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum TreemapWhen {
    /// On a terminal
    Auto,
    /// Never
    Never,
    /// Always, 80x24 when not on a terminal
    Always,
    /// In pixels, where the terminal shows images
    Pixels,
}

/// `--min-tile`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum MinTile {
    /// One unit: every tile that rounds to a unit (the default)
    #[value(name = "1")]
    One,
    /// Two units
    #[value(name = "2")]
    Two,
    /// Four units: a calmer picture
    #[value(name = "4")]
    Four,
}

/// `--color-by`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ColorBy {
    /// File class
    Type,
    /// Top-level folder
    Folder,
    /// Depth
    Level,
    /// Age
    Age,
    /// LS_COLORS, else file class
    Lscolors,
}

/// `--provider`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ProviderArg {
    /// The platform's own
    Native,
    /// readdir and fstatat
    Fallback,
}

/// Fills from the configuration file what no flag and no environment
/// variable set: flag > environment > file > built-in. clap reads
/// `DUSCOPE_THREADS` and `DUSCOPE_THEME` into their flags, so a flag still
/// unset here was given neither way; `matches` (the verb's, or the top
/// level's without one) tell a defaulted `--attrs` from a given one. The
/// format goes to [`OutputArgs::configured`], below `DUSCOPE_FORMAT` and the
/// agent variables. A value the flag would refuse is a usage error naming
/// the file and line.
pub fn apply_config(cli: &mut Cli, matches: &ArgMatches, config: &Config) -> Result<(), Failure> {
    let d = &config.defaults;
    let refused = |key: &str, line: usize, why: &str| refused(config, key, line, why);
    if let Some(format) = &d.format {
        cli.output.configured = Some(choice(config, "format", format)?);
    }
    cli.output.quiet = cli.global.quiet;
    if cli.global.index_dir.is_none() {
        cli.global.index_dir = d.index_dir.as_ref().map(|dir| dir.value.clone());
    }
    if let Some(age) = &d.max_age {
        timegrammar::parse_age(&age.value).map_err(|e| refused("max_age", age.line, &e))?;
    }
    // Asked only of a verb that has --attrs: clap panics on an unknown id.
    let attrs_defaulted = || matches.value_source("attrs") == Some(ValueSource::DefaultValue);
    let walk_defaults = |walk: &mut WalkArgs| -> Result<(), Failure> {
        if walk.threads.is_none()
            && let Some(threads) = &d.threads
        {
            let n = parse_threads(&threads.value.to_string())
                .map_err(|_| refused("threads", threads.line, &format!("at most {MAX_THREADS}")))?;
            walk.threads = Some(n);
        }
        if !walk.apparent_size {
            size_default(config, &mut walk.size)?;
        }
        if let Some(attrs) = &d.attrs
            && attrs_defaulted()
        {
            walk.attrs = choice(config, "attrs", attrs)?;
        }
        if !walk.clones && !walk.no_clones && d.clones.as_ref().is_some_and(|c| !c.value) {
            walk.no_clones = true;
        }
        walk.config_excludes.clone_from(&config.excludes);
        walk.classes_extra.clone_from(&config.classes_extra);
        walk.caches_extra.clone_from(&config.caches_extra);
        Ok(())
    };
    let max_age_default = |max_age: &mut Option<String>| {
        if max_age.is_none() {
            *max_age = d.max_age.as_ref().map(|age| age.value.clone());
        }
    };
    let index_defaults = |index: &mut IndexArgs| max_age_default(&mut index.max_age);
    let query_defaults = |query: &mut QueryArgs, index: &mut IndexArgs| {
        limit_default(config, &mut query.limit);
        index_defaults(index);
    };
    let list_defaults = |walk: &mut WalkArgs,
                         query: &mut QueryArgs,
                         index: &mut IndexArgs|
     -> Result<(), Failure> {
        walk_defaults(walk)?;
        query_defaults(query, index);
        Ok(())
    };
    let scan_defaults = |scan: &mut ScanArgs| -> Result<(), Failure> {
        walk_defaults(&mut scan.walk)?;
        if scan.theme.is_none()
            && let Some(theme) = &d.theme
        {
            scan.theme = Some(OsString::from(&theme.value));
        }
        if scan.color_by.is_none()
            && let Some(by) = &d.color_by
        {
            scan.color_by = Some(choice(config, "color_by", by)?);
        }
        Ok(())
    };
    match &mut cli.verb {
        None => scan_defaults(&mut cli.scan)?,
        Some(Verb::Scan(args)) => scan_defaults(args)?,
        Some(Verb::Top(a)) => list_defaults(&mut a.walk, &mut a.query, &mut a.index)?,
        Some(Verb::Tree(a)) => list_defaults(&mut a.walk, &mut a.query, &mut a.index)?,
        Some(Verb::ByType(a)) => {
            list_defaults(&mut a.walk, &mut a.query, &mut a.index)?;
            if a.classes.is_none() {
                a.classes.clone_from(&config.classes_extra);
            }
        }
        Some(Verb::Stale(a)) => list_defaults(&mut a.walk, &mut a.query, &mut a.index)?,
        Some(Verb::Caches(a)) => list_defaults(&mut a.walk, &mut a.query, &mut a.index)?,
        Some(
            Verb::Summary(SummaryArgs { walk, index }) | Verb::Query(SqlArgs { walk, index, .. }),
        ) => {
            walk_defaults(walk)?;
            index_defaults(index);
        }
        Some(Verb::Diff(args)) => {
            size_default(config, &mut args.size)?;
            limit_default(config, &mut args.limit);
        }
        Some(Verb::Explain(args)) => {
            max_age_default(&mut args.max_age);
            args.caches_extra.clone_from(&config.caches_extra);
        }
        Some(Verb::Scans(_) | Verb::Man | Verb::Completions(_)) => {}
    }
    Ok(())
}

/// `[defaults] size` where no flag set `--size`.
fn size_default(config: &Config, size: &mut Option<SizeKind>) -> Result<(), Failure> {
    if size.is_none()
        && let Some(set) = &config.defaults.size
    {
        *size = Some(choice(config, "size", set)?);
    }
    Ok(())
}

/// `[defaults] limit` where no flag set `--limit`.
fn limit_default(config: &Config, limit: &mut Option<usize>) {
    if limit.is_none()
        && let Some(set) = &config.defaults.limit
    {
        *limit = Some(usize::try_from(set.value).unwrap_or(usize::MAX));
    }
}

/// A `[defaults]` value the flag would refuse: a usage error naming the file
/// and line.
fn refused(config: &Config, key: &str, line: usize, why: &str) -> Failure {
    Failure::new(
        Exit::Usage,
        "usage",
        format!("{}: [defaults] {key}: {why}", config.at(line)),
    )
    .hint("Fix the configuration file, or run duscope with --config none")
}

/// The value of a flag of enumerated values that the `[defaults]` key
/// `setting` gives.
fn choice<T: ValueEnum>(
    config: &Config,
    key: &str,
    setting: &Setting<String>,
) -> Result<T, Failure> {
    T::from_str(&setting.value, false).map_err(|_| {
        let names: Vec<String> = T::value_variants()
            .iter()
            .filter_map(ValueEnum::to_possible_value)
            .map(|v| v.get_name().to_owned())
            .collect();
        let why = format!("{:?} is not one of {}", setting.value, names.join("|"));
        refused(config, key, setting.line, &why)
    })
}

/// The most worker threads a scan starts. More than this is a typing
/// mistake, not a faster scan.
pub const MAX_THREADS: usize = 1024;

/// A thread count (`--threads`, `DUSCOPE_THREADS`): a whole number up to
/// [`MAX_THREADS`]. Empty, like 0, leaves the choice to the platform, so an
/// exported but empty `DUSCOPE_THREADS` is the same as an unset one.
pub fn parse_threads(text: &str) -> Result<usize, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(0);
    }
    match text.parse::<usize>() {
        Ok(n) if n <= MAX_THREADS => Ok(n),
        Ok(_) => Err(format!(
            "at most {MAX_THREADS} threads (from --threads or DUSCOPE_THREADS)"
        )),
        Err(_) => {
            Err("not a whole number of threads (from --threads or DUSCOPE_THREADS)".to_owned())
        }
    }
}

/// The multipliers of size suffixes: `K/M/G/T` are powers of 1,024, `KB/MB/GB/TB`
/// of 1,000; a bare number is bytes.
const SIZE_SUFFIXES: [(&str, u64); 9] = [
    ("KB", 1_000),
    ("MB", 1_000_000),
    ("GB", 1_000_000_000),
    ("TB", 1_000_000_000_000),
    ("K", 1 << 10),
    ("M", 1 << 20),
    ("G", 1 << 30),
    ("T", 1 << 40),
    ("B", 1),
];

/// A size argument: a number with an optional suffix (`100M`, `1.5G`,
/// `10KB`).
pub fn parse_size(text: &str) -> Result<u64, String> {
    let upper = text.trim().to_ascii_uppercase();
    let (number, multiplier) = SIZE_SUFFIXES
        .iter()
        .find_map(|&(suffix, m)| upper.strip_suffix(suffix).map(|n| (n.to_owned(), m)))
        .unwrap_or((upper.clone(), 1));
    let bad = || {
        format!(
            "{text:?} is not a size: use a number with K, M, G or T (KB, MB, … for powers of 1,000)"
        )
    };
    if number.is_empty() || !number.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return Err(bad());
    }
    match number.split_once('.') {
        None => number
            .parse::<u64>()
            .ok()
            .and_then(|n| n.checked_mul(multiplier))
            .ok_or_else(bad),
        Some((whole, fraction)) => {
            // Exact decimal arithmetic: whole·m + fraction·m / 10^digits.
            let whole: u64 = if whole.is_empty() {
                0
            } else {
                whole.parse().map_err(|_| bad())?
            };
            let digits = u32::try_from(fraction.len()).map_err(|_| bad())?;
            let scale = 10u64.checked_pow(digits).ok_or_else(bad)?;
            let fraction: u64 = if fraction.is_empty() {
                0
            } else {
                fraction.parse().map_err(|_| bad())?
            };
            let part = u128::from(fraction) * u128::from(multiplier) / u128::from(scale);
            whole
                .checked_mul(multiplier)
                .and_then(|w| w.checked_add(u64::try_from(part).ok()?))
                .ok_or_else(bad)
        }
    }
}

/// `--theme`: a pack's name or a theme file's path, taken as given; the
/// pack names are offered to the shell completions.
#[derive(Debug, Clone, Copy)]
struct ThemeName;

impl clap::builder::TypedValueParser for ThemeName {
    type Value = OsString;

    fn parse_ref(
        &self,
        _command: &clap::Command,
        _arg: Option<&clap::Arg>,
        value: &std::ffi::OsStr,
    ) -> Result<OsString, clap::Error> {
        Ok(value.to_owned())
    }

    fn possible_values(
        &self,
    ) -> Option<Box<dyn Iterator<Item = clap::builder::PossibleValue> + '_>> {
        Some(Box::new(
            duscope_treemap::packs::NAMES
                .iter()
                .map(clap::builder::PossibleValue::new),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_THREADS, parse_threads};

    #[test]
    fn p1_cli_01_thread_counts_are_bounded() {
        assert_eq!(parse_threads(""), Ok(0));
        assert_eq!(parse_threads("  "), Ok(0));
        assert_eq!(parse_threads("0"), Ok(0));
        assert_eq!(parse_threads("8"), Ok(8));
        assert_eq!(parse_threads("1024"), Ok(MAX_THREADS));
        assert!(parse_threads("1025").is_err());
        assert!(parse_threads("-1").is_err());
        assert!(parse_threads("eight").is_err());
    }
}
