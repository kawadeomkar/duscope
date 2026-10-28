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

use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::out::OutputArgs;

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
}

/// The verbs of phase 1.
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
    /// Print the manual page, in roff, to stdout
    Man,
    /// Print the completion script for a shell to stdout
    Completions(CompletionsArgs),
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
    #[arg(long)]
    pub skip_containers: bool,

    /// Read every directory with this provider
    // test-only
    #[arg(long, value_enum, hide = true)]
    pub provider: Option<ProviderArg>,

    /// The instant "now" is, for tests
    // test-only
    #[arg(long, hide = true, value_name = "RFC3339")]
    pub test_now: Option<String>,
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

    /// Where the next page starts
    // Pages arrive with the index (phase 2).
    #[arg(long, value_name = "CURSOR", hide = true)]
    pub cursor: Option<String>,

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
    // The index arrives in phase 2.
    #[arg(long, hide = true)]
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

    /// Also require the access time to be older (needs --attrs full)
    #[arg(long, help_heading = "Stale")]
    pub atime: bool,
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
