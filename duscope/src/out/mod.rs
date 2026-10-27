//! The output layer, and the only module that writes to stdout: which
//! format and colours a command uses, how sizes and times read, and each
//! format's writer.
//!
//! In the machine formats (`--json`, `--plain`, `--format ndjson`) nothing
//! but the payload reaches stdout; footers and warnings go to stderr.

mod args;
mod color;
mod format;
mod table;
mod units;

use std::borrow::Cow;
use std::io::IsTerminal;

use duscope_core::envelope::{ByTypeEntry, Envelope, StaleEntry, TopEntry, TreeEntry, Warning};
use serde::Serialize;

pub use args::{ColorWhen, FormatArg, OutputArgs};
pub use units::{Units, iso8601, thousands};

use crate::cli::TreemapWhen;
use crate::verbs::{Exit, Failure};
use format::{EnvVars, emit};
use table::printable;

/// A format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// The treemap (`scan` on a terminal).
    Treemap,
    /// Tables for people.
    Table,
    /// Tab-separated records.
    Plain,
    /// The JSON envelope, compact.
    Json,
    /// One JSON object per line.
    Ndjson,
}

impl Format {
    /// The format `DUSCOPE_FORMAT` names.
    fn from_name(name: &str) -> Option<Format> {
        Some(match name {
            "treemap" => Format::Treemap,
            "table" => Format::Table,
            "plain" => Format::Plain,
            "json" => Format::Json,
            "ndjson" => Format::Ndjson,
            _ => return None,
        })
    }
}

impl From<FormatArg> for Format {
    fn from(arg: FormatArg) -> Format {
        match arg {
            FormatArg::Treemap => Format::Treemap,
            FormatArg::Table => Format::Table,
            FormatArg::Plain => Format::Plain,
            FormatArg::Json => Format::Json,
            FormatArg::Ndjson => Format::Ndjson,
        }
    }
}

/// How this command writes its result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    /// The format.
    pub format: Format,
    /// Whether what people read is coloured.
    pub color: bool,
    /// How sizes read for people.
    pub units: Units,
    /// `-0`: `--plain` records end with NUL.
    pub null: bool,
    /// `--header`: `--plain` starts with the field names.
    pub header: bool,
}

impl Output {
    /// Decides from the flags, the environment and whether stdout is a
    /// terminal. `treemap` is `scan`'s `--treemap`, `None` for the other
    /// verbs, whose terminal format is a table.
    pub fn decide(args: &OutputArgs, treemap: Option<TreemapWhen>) -> Result<Output, Failure> {
        let env = EnvVars::read(|name| std::env::var_os(name));
        let tty = std::io::stdout().is_terminal();
        Output::decide_with(args, treemap, &env, tty)
    }

    fn decide_with(
        args: &OutputArgs,
        treemap: Option<TreemapWhen>,
        env: &EnvVars,
        tty: bool,
    ) -> Result<Output, Failure> {
        let explicit = if args.json {
            Some(Format::Json)
        } else if args.plain {
            Some(Format::Plain)
        } else if let Some(format) = args.format {
            Some(format.into())
        } else {
            match treemap {
                Some(TreemapWhen::Always | TreemapWhen::Pixels) => Some(Format::Treemap),
                _ => None,
            }
        };
        let human = match treemap {
            Some(TreemapWhen::Never) | None => Format::Table,
            Some(_) => Format::Treemap,
        };
        let format = format::decide(explicit, env, tty, human)
            .map_err(|message| Failure::new(Exit::Usage, "usage", message))?;
        let color = if args.no_color {
            Some(ColorWhen::Never)
        } else {
            args.color
        };
        Ok(Output {
            format,
            color: color::decide(color, env, tty),
            units: Units {
                si: args.si,
                bytes: args.bytes,
            },
            null: args.null,
            header: args.header,
        })
    }

    /// Whether a command line clap refused asked for JSON, through its own
    /// flags as far as they can be read or through the environment: its
    /// usage error is then the error envelope.
    pub fn json_requested(args: &[String]) -> bool {
        let mut explicit = None;
        let mut args = args.iter().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--" => break,
                "--json" => explicit = Some(Format::Json),
                "--plain" => explicit = Some(Format::Plain),
                "--format" => explicit = args.next().and_then(|v| Format::from_name(v)),
                _ => {
                    if let Some(name) = arg.strip_prefix("--format=") {
                        explicit = Format::from_name(name);
                    }
                }
            }
        }
        let env = EnvVars::read(|name| std::env::var_os(name));
        let tty = std::io::stdout().is_terminal();
        matches!(
            format::decide(explicit, &env, tty, Format::Table),
            Ok(Format::Json | Format::Ndjson)
        )
    }

    /// Whether the format is for machines: JSON, `ndjson` or plain.
    pub fn is_machine(&self) -> bool {
        matches!(self.format, Format::Plain | Format::Json | Format::Ndjson)
    }

    /// Whether failures are written as the JSON error envelope.
    pub fn is_json(&self) -> bool {
        matches!(self.format, Format::Json | Format::Ndjson)
    }
}

/// A row a list verb prints.
pub trait Row: Serialize {
    /// `--plain`'s fields, in order, as `--header` names them.
    const FIELDS: &'static [&'static str];
    /// The table's column headings. Every column but the last is
    /// right-aligned; the last is the path, unpadded.
    const COLUMNS: &'static [&'static str];
    /// The fields' values as `--plain` writes them: bytes as integers,
    /// paths raw.
    fn plain(&self) -> Vec<Cow<'_, [u8]>>;
    /// The table's cells, sizes in `size_kind` (`allocated` or `apparent`).
    fn cells(&self, units: Units, size_kind: &str) -> Vec<String>;
}

impl Row for TopEntry {
    const FIELDS: &'static [&'static str] = &[
        "allocated_bytes",
        "apparent_bytes",
        "file_count",
        "dir_count",
        "max_mtime",
        "kind",
        "path",
    ];
    const COLUMNS: &'static [&'static str] = &["Size", "%", "Files", "Path"];

    fn plain(&self) -> Vec<Cow<'_, [u8]>> {
        let number = |n: u64| Cow::Owned(n.to_string().into_bytes());
        vec![
            number(self.allocated_bytes),
            number(self.apparent_bytes),
            number(self.file_count),
            number(self.dir_count),
            Cow::Borrowed(self.max_mtime.as_bytes()),
            Cow::Borrowed(self.kind.as_bytes()),
            Cow::Borrowed(&self.path_bytes),
        ]
    }

    fn cells(&self, units: Units, size_kind: &str) -> Vec<String> {
        let size = if size_kind == "apparent" {
            self.apparent_bytes
        } else {
            self.allocated_bytes
        };
        vec![
            units.size(size),
            format!("{:.1}%", self.pct_of_root),
            thousands(self.file_count),
            printable(&self.path).into_owned(),
        ]
    }
}

impl Row for TreeEntry {
    const FIELDS: &'static [&'static str] = &[
        "allocated_bytes",
        "apparent_bytes",
        "pct_of_parent",
        "depth",
        "kind",
        "path",
    ];
    const COLUMNS: &'static [&'static str] = &["Size", "%", "Path"];

    fn plain(&self) -> Vec<Cow<'_, [u8]>> {
        let text = |s: String| Cow::Owned(s.into_bytes());
        vec![
            text(self.allocated_bytes.to_string()),
            text(self.apparent_bytes.to_string()),
            text(format!("{:.1}", self.pct_of_parent)),
            text(self.depth.to_string()),
            Cow::Borrowed(self.kind.as_bytes()),
            Cow::Borrowed(&self.path_bytes),
        ]
    }

    fn cells(&self, units: Units, size_kind: &str) -> Vec<String> {
        let size = if size_kind == "apparent" {
            self.apparent_bytes
        } else {
            self.allocated_bytes
        };
        // The root by its path, everything under it by name, indented.
        let label = if self.depth == 0 {
            self.path.clone()
        } else {
            let slash = if self.kind == "dir" { "/" } else { "" };
            format!("{}{}{slash}", "  ".repeat(self.depth as usize), self.name)
        };
        vec![
            units.size(size),
            format!("{:.1}%", self.pct_of_parent),
            printable(&label).into_owned(),
        ]
    }
}

impl Row for ByTypeEntry {
    const FIELDS: &'static [&'static str] =
        &["allocated_bytes", "apparent_bytes", "file_count", "class"];
    const COLUMNS: &'static [&'static str] = &["Size", "%", "Files", "Class"];

    fn plain(&self) -> Vec<Cow<'_, [u8]>> {
        let number = |n: u64| Cow::Owned(n.to_string().into_bytes());
        vec![
            number(self.allocated_bytes),
            number(self.apparent_bytes),
            number(self.file_count),
            Cow::Borrowed(self.class.as_bytes()),
        ]
    }

    fn cells(&self, units: Units, size_kind: &str) -> Vec<String> {
        let size = if size_kind == "apparent" {
            self.apparent_bytes
        } else {
            self.allocated_bytes
        };
        vec![
            units.size(size),
            format!("{:.1}%", self.pct_of_root),
            thousands(self.file_count),
            self.class.to_owned(),
        ]
    }
}

impl Row for StaleEntry {
    const FIELDS: &'static [&'static str] = &[
        "allocated_bytes",
        "apparent_bytes",
        "mtime",
        "ctime",
        "atime",
        "kind",
        "path",
    ];
    const COLUMNS: &'static [&'static str] = &["Size", "Modified", "Path"];

    fn plain(&self) -> Vec<Cow<'_, [u8]>> {
        let number = |n: u64| Cow::Owned(n.to_string().into_bytes());
        vec![
            number(self.allocated_bytes),
            number(self.apparent_bytes),
            Cow::Borrowed(self.mtime.as_bytes()),
            Cow::Borrowed(self.ctime.as_bytes()),
            Cow::Borrowed(self.atime.as_deref().unwrap_or("").as_bytes()),
            Cow::Borrowed(self.kind.as_bytes()),
            Cow::Borrowed(&self.path_bytes),
        ]
    }

    fn cells(&self, units: Units, size_kind: &str) -> Vec<String> {
        let size = if size_kind == "apparent" {
            self.apparent_bytes
        } else {
            self.allocated_bytes
        };
        vec![
            units.size(size),
            self.mtime.get(..10).unwrap_or(&self.mtime).to_owned(),
            printable(&self.path).into_owned(),
        ]
    }
}

impl Output {
    /// Writes a list verb's result in the decided format. `footer` is the
    /// one line for people: after the table, or on stderr in the machine
    /// formats. Warnings go in the envelope under JSON, on stderr otherwise.
    pub fn list<T: Row>(&self, envelope: &Envelope<T>, footer: &str) -> Result<(), Failure> {
        let written = match self.format {
            Format::Json => emit(|out| format::json(out, envelope)),
            Format::Ndjson => emit(|out| format::ndjson(out, envelope)),
            Format::Plain => emit(|out| {
                let rows = envelope.entries.iter().map(Row::plain);
                format::plain(out, T::FIELDS, rows, self.null, self.header)
            }),
            Format::Table | Format::Treemap => {
                let size_kind = envelope.scan.size_kind;
                let table = table::table(&envelope.entries, self.units, size_kind, self.color);
                emit(|out| writeln!(out, "{table}{footer}"))
            }
        };
        if self.is_machine() {
            eprintln!("{footer}");
        }
        if !self.is_json() {
            warn(&envelope.warnings);
        }
        written
    }

    /// Writes one line for people: on stdout in the human formats, on
    /// stderr in the machine formats.
    pub fn line(&self, line: &str) -> Result<(), Failure> {
        if self.is_machine() {
            eprintln!("{line}");
            Ok(())
        } else {
            emit(|out| writeln!(out, "{line}"))
        }
    }

    /// Reports a failure: the error envelope on stdout under JSON, two
    /// lines on stderr otherwise.
    pub fn failure(&self, failure: &Failure) {
        if self.is_json() {
            let envelope = format::error_envelope(failure);
            // The exit code still reports the failure if stdout is gone.
            let _ = emit(|out| format::json(out, &envelope));
        } else {
            to_stderr(failure);
        }
    }
}

/// Warnings for people, on stderr.
fn warn(warnings: &[Warning]) {
    for warning in warnings {
        eprintln!("warning: {}", warning.message);
        if let Some(hint) = &warning.hint {
            eprintln!("hint: {hint}");
        }
    }
}

/// A failure for people, on stderr.
pub fn to_stderr(failure: &Failure) {
    eprintln!("duscope: {}", failure.message);
    if let Some(hint) = &failure.hint {
        eprintln!("hint: {hint}");
    }
}

#[cfg(test)]
mod tests {
    use super::format::EnvVars;
    use super::{Format, Output, OutputArgs};
    use crate::cli::TreemapWhen;

    #[test]
    fn p1_cli_02_scan_draws_a_treemap_on_a_terminal_and_the_flags_win() {
        let env = EnvVars::default();
        let decide = |args: &OutputArgs, treemap| {
            Output::decide_with(args, treemap, &env, true)
                .unwrap()
                .format
        };
        let none = OutputArgs::default();
        assert_eq!(decide(&none, Some(TreemapWhen::Auto)), Format::Treemap);
        assert_eq!(decide(&none, Some(TreemapWhen::Never)), Format::Table);
        assert_eq!(decide(&none, None), Format::Table);
        let json = OutputArgs {
            json: true,
            ..OutputArgs::default()
        };
        assert_eq!(decide(&json, Some(TreemapWhen::Always)), Format::Json);
        // --treemap always draws one when stdout is not a terminal.
        let piped = Output::decide_with(&none, Some(TreemapWhen::Always), &env, false).unwrap();
        assert_eq!(piped.format, Format::Treemap);
        let piped = Output::decide_with(&none, Some(TreemapWhen::Auto), &env, false).unwrap();
        assert_eq!(piped.format, Format::Plain);
    }
}
