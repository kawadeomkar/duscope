//! The output flags, which every verb takes before or after its name.

// Help text is plain text, and a flag struct is a set of switches.
#![allow(clippy::doc_markdown, clippy::struct_excessive_bools)]

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
