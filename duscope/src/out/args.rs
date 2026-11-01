//! The output flags, which every verb takes before or after its name.

// Help text is plain text, and a flag struct is a set of switches.
#![allow(clippy::doc_markdown, clippy::struct_excessive_bools)]

use std::path::PathBuf;

use clap::{Args, ValueEnum};

/// How a command writes its result.
#[derive(Debug, Clone, Default, Args)]
#[command(next_help_heading = "Output")]
pub struct OutputArgs {
    /// Print the JSON envelope (--format json)
    #[arg(long, global = true, conflicts_with_all = ["plain", "format"])]
    pub json: bool,

    /// One tab-separated record per line, sizes in bytes (--format plain)
    #[arg(long, global = true, conflicts_with = "format")]
    pub plain: bool,

    /// The output format
    #[arg(long, global = true, value_enum, value_name = "FORMAT")]
    pub format: Option<FormatArg>,

    /// When to use colour
    #[arg(long, global = true, value_enum, value_name = "WHEN")]
    pub color: Option<ColorWhen>,

    /// Never use colour (--color never)
    #[arg(long, global = true, conflicts_with = "color")]
    pub no_color: bool,

    /// Show sizes in powers of 1,000 (kB, MB, GB)
    #[arg(long, global = true)]
    pub si: bool,

    /// Show sizes as exact byte counts
    #[arg(short = 'b', long, global = true, conflicts_with = "si")]
    pub bytes: bool,

    /// End each --plain record with NUL instead of a newline
    #[arg(short = '0', long, global = true)]
    pub null: bool,

    /// Start --plain output with a line of field names
    #[arg(long, global = true)]
    pub header: bool,

    /// The fields of each row, comma-separated, in that order (help lists them)
    #[arg(long, global = true, value_name = "FIELDS", conflicts_with = "detail")]
    pub fields: Option<String>,

    /// Every field of each row
    #[arg(long, global = true)]
    pub detail: bool,

    /// Write the output to FILE, with no --limit cap and no --max-chars
    #[arg(short = 'o', long = "output", global = true, value_name = "FILE")]
    pub output: Option<PathBuf>,

    /// Put a JSON envelope over N characters in a file; print its first rows
    ///
    /// Kept a week in the index directory's out/; default 24000, 0 never
    #[arg(long, global = true, value_name = "N")]
    pub max_chars: Option<usize>,

    /// The configuration file's `format`, which decides only when no flag,
    /// `DUSCOPE_FORMAT`, agent variable or `CI` does.
    #[arg(skip)]
    pub configured: Option<FormatArg>,

    /// `-q`: warnings stay off stderr.
    #[arg(skip)]
    pub quiet: bool,
}

/// `--format`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum FormatArg {
    /// The treemap (scan), on a terminal
    Treemap,
    /// Tables for people
    Table,
    /// Tab-separated records
    Plain,
    /// The JSON envelope
    Json,
    /// One JSON object per line, every row
    Ndjson,
    /// ncdu's JSON (export)
    Ncdu,
    /// GrandPerspective's .gpscan (export)
    Gpscan,
    /// QDirStat's cache file (export)
    Qdirstat,
    /// Comma-separated values, a row an entry (export)
    Csv,
}

impl FormatArg {
    /// Whether only `export` writes it.
    pub fn exports_only(self) -> bool {
        matches!(
            self,
            FormatArg::Ncdu | FormatArg::Gpscan | FormatArg::Qdirstat | FormatArg::Csv
        )
    }
}

/// `--color`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ColorWhen {
    /// On a terminal, unless NO_COLOR or TERM=dumb
    Auto,
    /// Always
    Always,
    /// Never
    Never,
}
