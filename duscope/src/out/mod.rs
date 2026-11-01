//! The output layer, and the only module that writes to stdout: which
//! format and colours a command uses, how sizes and times read, and each
//! format's writer.
//!
//! In the machine formats (`--json`, `--plain`, `--format ndjson`) nothing
//! but the payload reaches stdout; footers and warnings go to stderr.

mod args;
mod color;
mod fields;
mod format;
mod spill;
mod table;
mod units;

use std::borrow::Cow;
use std::io::{IsTerminal, Write};
use std::path::PathBuf;

use duscope_core::envelope::{
    ByTypeEntry, CacheEntry, CheckedEntry, DiffEntry, DiffEnvelope, DiffTotals, Envelope,
    ErrorEnvelope, Listing, ObjectEnvelope, ProjectEntry, PrunedEntry, SavedScanEntry,
    ScanErrorEntry, SqlQueryBlock, SqlResult, SqlValue, StaleEntry, TopEntry, TreeEntry, Warning,
};
use serde::Serialize;

pub use args::{ColorWhen, FormatArg, OutputArgs};
pub use fields::Fields;

/// A floating-point value as the JSON writes it (`3.0`, `1e300`); empty
/// for one the JSON writes as null.
fn real(x: f64) -> String {
    serde_json::Number::from_f64(x).map_or_else(String::new, |n| n.to_string())
}

/// Writes `bytes` to stdout as they are (the treemap's escapes and
/// cells); a closed pipe ends the output quietly.
pub fn emit_bytes(bytes: &[u8]) -> Result<(), Failure> {
    emit(|out| out.write_all(bytes))
}
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
// The flags' switches.
#[allow(clippy::struct_excessive_bools)]
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
    /// `-q`: no warnings on stderr.
    pub quiet: bool,
    /// `--fields`, `--detail`.
    pub fields: Option<Fields>,
    /// `-o FILE`: where the output goes instead of stdout.
    pub file: Option<PathBuf>,
    /// `--max-chars`: a longer JSON envelope is spilled; 0 never is.
    pub max_chars: usize,
    /// Where a spilled envelope goes: the index directory's `out/`.
    pub spill_dir: Option<PathBuf>,
}

impl Output {
    /// Decides from the flags, the environment and whether stdout is a
    /// terminal. `treemap` is `scan`'s `--treemap`, `None` for the other
    /// verbs, whose terminal format is a table.
    pub fn decide(args: &OutputArgs, treemap: Option<TreemapWhen>) -> Result<Output, Failure> {
        let env = EnvVars::read(|name| std::env::var_os(name));
        // A file is no terminal: -o decides as a pipe does, without colour.
        let tty = std::io::stdout().is_terminal() && args.output.is_none();
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
        // The configuration file's format stands in for the terminal test,
        // below the environment's choice.
        let explicit = explicit.or_else(|| {
            let env_decides = env.duscope_format.is_some() || env.agent || env.ci;
            args.configured.filter(|_| !env_decides).map(Format::from)
        });
        let human = match treemap {
            Some(TreemapWhen::Never) | None => Format::Table,
            Some(_) => Format::Treemap,
        };
        let format = match format::decide(explicit, env, tty, human)
            .map_err(|message| Failure::new(Exit::Usage, "usage", message))?
        {
            // Only scan draws a treemap; the list verbs show their table.
            Format::Treemap if treemap.is_none() => Format::Table,
            format => format,
        };
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
            quiet: args.quiet,
            fields: Fields::from_flags(args.fields.as_deref(), args.detail),
            file: args.output.clone(),
            max_chars: args.max_chars.unwrap_or(spill::DEFAULT_MAX_CHARS),
            spill_dir: None,
        })
    }

    /// Whether a command line clap refused asked for JSON, through its own
    /// flags as far as they can be read or through the environment: its
    /// usage error is then the error envelope.
    pub fn json_requested(args: &[String]) -> bool {
        // The flags rank as the decision ranks them: --json, then --plain,
        // then --format.
        let (mut json, mut plain, mut format) = (false, false, None);
        let mut args = args.iter().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--" => break,
                "--json" => json = true,
                "--plain" => plain = true,
                "--format" => format = args.next().and_then(|v| Format::from_name(v)),
                _ => {
                    if let Some(name) = arg.strip_prefix("--format=") {
                        format = Format::from_name(name);
                    }
                }
            }
        }
        let explicit = if json {
            Some(Format::Json)
        } else if plain {
            Some(Format::Plain)
        } else {
            format
        };
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
pub trait Row: Serialize + duscope_core::envelope::Fields {
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
    const FIELDS: &'static [&'static str] = &[
        "allocated_bytes",
        "apparent_bytes",
        "pct_of_root",
        "file_count",
        "class",
    ];
    const COLUMNS: &'static [&'static str] = &["Size", "%", "Files", "Class"];

    fn plain(&self) -> Vec<Cow<'_, [u8]>> {
        let number = |n: u64| Cow::Owned(n.to_string().into_bytes());
        vec![
            number(self.allocated_bytes),
            number(self.apparent_bytes),
            Cow::Owned(format!("{:.1}", self.pct_of_root).into_bytes()),
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

impl Row for CacheEntry {
    const FIELDS: &'static [&'static str] = &[
        "allocated_bytes",
        "apparent_bytes",
        "file_count",
        "max_mtime",
        "regenerable_by",
        "confirmed",
        "cachedir_tag",
        "project_root",
        "path",
    ];
    const COLUMNS: &'static [&'static str] = &["Size", "Modified", "Regenerated by", "Path"];

    fn plain(&self) -> Vec<Cow<'_, [u8]>> {
        let number = |n: u64| Cow::Owned(n.to_string().into_bytes());
        let flag = |b: bool| Cow::Borrowed(if b { &b"true"[..] } else { &b"false"[..] });
        vec![
            number(self.allocated_bytes),
            number(self.apparent_bytes),
            number(self.file_count),
            Cow::Borrowed(self.max_mtime.as_bytes()),
            Cow::Borrowed(self.regenerable_by.as_bytes()),
            flag(self.confirmed),
            flag(self.cachedir_tag),
            Cow::Borrowed(&self.project_bytes),
            Cow::Borrowed(&self.path_bytes),
        ]
    }

    fn cells(&self, units: Units, size_kind: &str) -> Vec<String> {
        let size = if size_kind == "apparent" {
            self.apparent_bytes
        } else {
            self.allocated_bytes
        };
        // A name without what confirms it (--loose) says so.
        let path = if self.confirmed {
            printable(&self.path).into_owned()
        } else {
            format!("{} (no marker)", printable(&self.path))
        };
        vec![
            units.size(size),
            self.max_mtime
                .get(..10)
                .unwrap_or(&self.max_mtime)
                .to_owned(),
            self.regenerable_by.to_owned(),
            path,
        ]
    }
}

impl Row for DiffEntry {
    const FIELDS: &'static [&'static str] = &[
        "change",
        "bytes_delta",
        "old_bytes",
        "new_bytes",
        "collapsed_count",
        "kind",
        "path",
    ];
    const COLUMNS: &'static [&'static str] = &["Change", "Delta", "Path"];

    fn plain(&self) -> Vec<Cow<'_, [u8]>> {
        let number =
            |n: Option<u64>| Cow::Owned(n.map(|n| n.to_string()).unwrap_or_default().into_bytes());
        vec![
            Cow::Borrowed(self.change.as_bytes()),
            Cow::Owned(self.bytes_delta.to_string().into_bytes()),
            number(self.old_bytes),
            number(self.new_bytes),
            number(self.collapsed_count),
            Cow::Borrowed(self.kind.as_bytes()),
            Cow::Borrowed(&self.path_bytes),
        ]
    }

    fn cells(&self, units: Units, _size_kind: &str) -> Vec<String> {
        let sign = if self.bytes_delta < 0 { "-" } else { "+" };
        let path = match self.collapsed_count {
            Some(n) => format!("{} ({} entries)", printable(&self.path), thousands(n)),
            None => printable(&self.path).into_owned(),
        };
        vec![
            self.change.to_owned(),
            format!("{sign}{}", units.size(self.bytes_delta.unsigned_abs())),
            path,
        ]
    }
}

impl Row for ProjectEntry {
    const FIELDS: &'static [&'static str] = &[
        "allocated_bytes",
        "apparent_bytes",
        "cache_count",
        "max_mtime",
        "project_root",
    ];
    const COLUMNS: &'static [&'static str] = &["Size", "Caches", "Modified", "Project"];

    fn plain(&self) -> Vec<Cow<'_, [u8]>> {
        let number = |n: u64| Cow::Owned(n.to_string().into_bytes());
        vec![
            number(self.allocated_bytes),
            number(self.apparent_bytes),
            number(self.cache_count),
            Cow::Borrowed(self.max_mtime.as_bytes()),
            Cow::Borrowed(&self.path_bytes),
        ]
    }

    fn cells(&self, units: Units, size_kind: &str) -> Vec<String> {
        let size = if size_kind == "apparent" {
            self.apparent_bytes
        } else {
            self.allocated_bytes
        };
        // The caches no marker confirms: a CACHEDIR.TAG, an ancestor path
        // (DerivedData) or --loose.
        let project = self
            .project_root
            .as_deref()
            .map_or_else(|| "(no project)".to_owned(), |p| printable(p).into_owned());
        vec![
            units.size(size),
            thousands(self.cache_count),
            self.max_mtime
                .get(..10)
                .unwrap_or(&self.max_mtime)
                .to_owned(),
            project,
        ]
    }
}

impl Row for SavedScanEntry {
    const FIELDS: &'static [&'static str] = &[
        "id",
        "finished_at",
        "entry_count",
        "allocated_bytes",
        "apparent_bytes",
        "root",
    ];
    const COLUMNS: &'static [&'static str] = &["Id", "Finished", "Entries", "Size", "Root"];

    fn plain(&self) -> Vec<Cow<'_, [u8]>> {
        let number = |n: u64| Cow::Owned(n.to_string().into_bytes());
        vec![
            Cow::Borrowed(self.id.as_bytes()),
            Cow::Borrowed(self.finished_at.as_bytes()),
            number(self.entry_count),
            number(self.allocated_bytes),
            number(self.apparent_bytes),
            Cow::Borrowed(self.root.as_bytes()),
        ]
    }

    fn cells(&self, units: Units, size_kind: &str) -> Vec<String> {
        let size = if size_kind == "apparent" {
            self.apparent_bytes
        } else {
            self.allocated_bytes
        };
        vec![
            self.short_id.clone(),
            self.finished_at
                .get(..16)
                .unwrap_or(&self.finished_at)
                .replace('T', " "),
            thousands(self.entry_count),
            units.size(size),
            printable(&self.root).into_owned(),
        ]
    }
}

impl Row for PrunedEntry {
    const FIELDS: &'static [&'static str] = &["kind", "bytes", "path"];
    const COLUMNS: &'static [&'static str] = &["Kind", "Size", "Path"];

    fn plain(&self) -> Vec<Cow<'_, [u8]>> {
        vec![
            Cow::Borrowed(self.kind.as_bytes()),
            Cow::Owned(self.bytes.to_string().into_bytes()),
            Cow::Borrowed(self.path.as_bytes()),
        ]
    }

    fn cells(&self, units: Units, _size_kind: &str) -> Vec<String> {
        vec![
            self.kind.to_owned(),
            units.size(self.bytes),
            printable(&self.path).into_owned(),
        ]
    }
}

impl Row for CheckedEntry {
    const FIELDS: &'static [&'static str] = &["id", "quick_check", "index_path"];
    const COLUMNS: &'static [&'static str] = &["Id", "Check", "Path"];

    fn plain(&self) -> Vec<Cow<'_, [u8]>> {
        vec![
            Cow::Borrowed(self.id.as_bytes()),
            Cow::Borrowed(self.quick_check.as_bytes()),
            Cow::Borrowed(self.index_path.as_bytes()),
        ]
    }

    fn cells(&self, _units: Units, _size_kind: &str) -> Vec<String> {
        vec![
            self.id.clone(),
            self.quick_check.to_owned(),
            printable(&self.index_path).into_owned(),
        ]
    }
}

impl Row for ScanErrorEntry {
    const FIELDS: &'static [&'static str] = &["kind", "errno", "message", "path"];
    const COLUMNS: &'static [&'static str] = &["Kind", "Error", "Path"];

    fn plain(&self) -> Vec<Cow<'_, [u8]>> {
        vec![
            Cow::Borrowed(self.kind.as_bytes()),
            Cow::Owned(self.errno.to_string().into_bytes()),
            Cow::Borrowed(self.message.as_bytes()),
            Cow::Borrowed(&self.path_bytes),
        ]
    }

    fn cells(&self, _units: Units, _size_kind: &str) -> Vec<String> {
        vec![
            self.kind.to_owned(),
            self.message.clone(),
            printable(&self.path).into_owned(),
        ]
    }
}

impl Output {
    /// Writes a one-object answer (`explain`): the envelope under `--json`
    /// and `--format ndjson`, `plain`'s name and value per line under
    /// `--plain`, `people`'s aligned under the table format. Warnings go in
    /// the envelope under JSON, on stderr otherwise.
    pub fn object<T: Serialize>(
        &self,
        envelope: &ObjectEnvelope<T>,
        plain: &[(&str, Vec<u8>)],
        people: &[(String, String)],
    ) -> Result<(), Failure> {
        let written = match self.format {
            Format::Json | Format::Ndjson => self.sink(|out| format::json(out, envelope)),
            Format::Plain => self.sink(|out| {
                let end = if self.null { b'\0' } else { b'\n' };
                for (name, value) in plain {
                    out.write_all(name.as_bytes())?;
                    out.write_all(b"\t")?;
                    out.write_all(value)?;
                    out.write_all(&[end])?;
                }
                Ok(())
            }),
            Format::Table | Format::Treemap => {
                let width = people.iter().map(|(name, _)| name.len()).max().unwrap_or(0);
                self.sink(|out| {
                    for (name, value) in people {
                        writeln!(out, "{name:<width$}  {}", printable(value))?;
                    }
                    Ok(())
                })
            }
        };
        if !self.is_json() && !self.quiet {
            warn(&envelope.warnings);
        }
        written
    }

    /// Writes `query`'s answer: the envelope under `--json` and `--format
    /// ndjson` (spilled past `--max-chars` like a list's), tab-separated
    /// values with a heading line under `--plain`, a table under the table
    /// format. `footer` is the one line for people, after the table or on
    /// stderr in the machine formats.
    pub fn sql(
        &self,
        envelope: &ObjectEnvelope<SqlResult, SqlQueryBlock>,
        footer: &str,
    ) -> Result<(), Failure> {
        let result = &envelope.result;
        let written = match self.format {
            Format::Json | Format::Ndjson => self.json(
                envelope,
                &result.rows,
                &envelope.warnings,
                envelope.scan.as_ref().and_then(|scan| scan.id.as_deref()),
                "query",
            ),
            Format::Plain => self.sink(|out| {
                let columns: Vec<&str> = result.columns.iter().map(String::as_str).collect();
                let records = result.rows.iter().map(|row| {
                    row.iter()
                        .map(|value| match value {
                            SqlValue::Null => Cow::Borrowed(&b""[..]),
                            SqlValue::Integer(n) => Cow::Owned(n.to_string().into_bytes()),
                            SqlValue::Real(x) => Cow::Owned(real(*x).into_bytes()),
                            SqlValue::Text(text) => Cow::Borrowed(text.as_bytes()),
                            SqlValue::Bytes(bytes) => Cow::Borrowed(bytes.as_slice()),
                        })
                        .collect()
                });
                format::plain(out, &columns, records, self.null, true)
            }),
            Format::Table | Format::Treemap => {
                let columns: Vec<String> = result
                    .columns
                    .iter()
                    .map(|c| printable(c).into_owned())
                    .collect();
                let columns: Vec<&str> = columns.iter().map(String::as_str).collect();
                let cells: Vec<Vec<String>> = result
                    .rows
                    .iter()
                    .map(|row| {
                        row.iter()
                            .map(|value| match value {
                                SqlValue::Null => String::new(),
                                SqlValue::Integer(n) => n.to_string(),
                                SqlValue::Real(x) => real(*x),
                                SqlValue::Text(text) => printable(text).into_owned(),
                                SqlValue::Bytes(bytes) => {
                                    printable(&String::from_utf8_lossy(bytes)).into_owned()
                                }
                            })
                            .collect()
                    })
                    .collect();
                let grid = table::grid(&columns, &cells, self.color);
                self.sink(|out| writeln!(out, "{grid}{footer}"))
            }
        };
        if self.is_machine() {
            stderr_line(footer);
        }
        if !self.is_json() && !self.quiet {
            warn(&envelope.warnings);
        }
        written
    }

    /// Writes a listing of saved scans (`scans`) in the decided format,
    /// as [`Output::list`] writes a list verb's.
    pub fn listing<T: Row>(&self, listing: &Listing<T>, footer: &str) -> Result<(), Failure> {
        let chosen = self.chosen::<T>(listing.query.command)?;
        let written = match (self.format, &chosen) {
            (Format::Json, Some(chosen)) => {
                let selected = fields::select_listing(listing, chosen);
                self.json(
                    &selected,
                    &selected.entries,
                    &listing.warnings,
                    None,
                    listing.query.command,
                )
            }
            (Format::Json, None) => self.json(
                listing,
                &listing.entries,
                &listing.warnings,
                None,
                listing.query.command,
            ),
            (Format::Ndjson, Some(chosen)) => self
                .sink(|out| format::ndjson_listing(out, &fields::select_listing(listing, chosen))),
            (Format::Ndjson, None) => self.sink(|out| format::ndjson_listing(out, listing)),
            (Format::Plain, chosen) => {
                self.sink(|out| self.plain(out, &listing.entries, chosen.as_deref()))
            }
            (Format::Table | Format::Treemap, _) => {
                let table = table::table(&listing.entries, self.units, "allocated", self.color);
                self.sink(|out| writeln!(out, "{table}{footer}"))
            }
        };
        if self.is_machine() {
            stderr_line(footer);
        }
        if !self.is_json() && !self.quiet {
            warn(&listing.warnings);
        }
        written
    }

    /// Writes a list verb's result in the decided format. `footer` is the
    /// one line for people: after the table, or on stderr in the machine
    /// formats. Warnings go in the envelope under JSON, on stderr otherwise.
    pub fn list<T: Row>(&self, envelope: &Envelope<T>, footer: &str) -> Result<(), Failure> {
        self.list_with(envelope, None, footer)
    }

    /// [`Output::list`] of `diff`, whose JSON envelope carries its totals
    /// as `diff` after the shared fields.
    pub fn list_diff<T: Row>(
        &self,
        envelope: &Envelope<T>,
        totals: &DiffTotals,
        footer: &str,
    ) -> Result<(), Failure> {
        self.list_with(envelope, Some(totals), footer)
    }

    fn list_with<T: Row>(
        &self,
        envelope: &Envelope<T>,
        totals: Option<&DiffTotals>,
        footer: &str,
    ) -> Result<(), Failure> {
        let verb = envelope.query.command;
        let chosen = self.chosen::<T>(verb)?;
        if let Some(chosen) = &chosen {
            let named = matches!(self.fields, Some(Fields::Named(_)));
            fields::check_attrs(chosen, envelope.scan.attrs, named)?;
        }
        let scan = envelope.scan.id.as_deref();
        let written = match (self.format, &chosen) {
            (Format::Json, Some(chosen)) => {
                let selected = fields::select(envelope, chosen);
                match totals {
                    Some(diff) => self.json(
                        &DiffEnvelope {
                            envelope: &selected,
                            diff,
                        },
                        &selected.entries,
                        &envelope.warnings,
                        scan,
                        verb,
                    ),
                    None => self.json(&selected, &selected.entries, &envelope.warnings, scan, verb),
                }
            }
            (Format::Json, None) => match totals {
                Some(diff) => self.json(
                    &DiffEnvelope { envelope, diff },
                    &envelope.entries,
                    &envelope.warnings,
                    scan,
                    verb,
                ),
                None => self.json(envelope, &envelope.entries, &envelope.warnings, scan, verb),
            },
            (Format::Ndjson, Some(chosen)) => {
                self.sink(|out| format::ndjson(out, &fields::select(envelope, chosen)))
            }
            (Format::Ndjson, None) => self.sink(|out| format::ndjson(out, envelope)),
            (Format::Plain, chosen) => {
                self.sink(|out| self.plain(out, &envelope.entries, chosen.as_deref()))
            }
            (Format::Table | Format::Treemap, _) => {
                let size_kind = envelope.scan.size_kind;
                let table = table::table(&envelope.entries, self.units, size_kind, self.color);
                self.sink(|out| writeln!(out, "{table}{footer}"))
            }
        };
        if self.is_machine() {
            stderr_line(footer);
        }
        if !self.is_json() && !self.quiet {
            warn(&envelope.warnings);
        }
        written
    }

    /// `--fields help`, written: the fields of rows of type `T`, one per
    /// line (an object under JSON), and `true`; or `unknown_field` for a
    /// name `--fields` gives that `T` does not have, before anything runs.
    pub fn fields_first<T: Row>(&self, verb: &str) -> Result<bool, Failure> {
        match &self.fields {
            Some(Fields::Help) => {
                if self.is_json() {
                    #[derive(Serialize)]
                    struct Listed<'a> {
                        ok: bool,
                        schema_version: &'static str,
                        command: &'a str,
                        fields: &'static [&'static str],
                    }
                    let listed = Listed {
                        ok: true,
                        schema_version: duscope_core::envelope::SCHEMA_VERSION,
                        command: verb,
                        fields: T::NAMES,
                    };
                    self.sink(|out| format::json(out, &listed))?;
                } else {
                    self.sink(|out| {
                        for name in T::NAMES {
                            writeln!(out, "{name}")?;
                        }
                        Ok(())
                    })?;
                }
                Ok(true)
            }
            Some(fields) => fields::chosen::<T>(fields, verb).map(|_| false),
            None => Ok(false),
        }
    }

    /// The fields `--fields` or `--detail` choose for rows of type `T`.
    fn chosen<T: Row>(&self, verb: &str) -> Result<Option<Vec<&'static str>>, Failure> {
        match &self.fields {
            Some(fields) => fields::chosen::<T>(fields, verb),
            None => Ok(None),
        }
    }

    /// `--plain` records of `rows`: their own fields, or `chosen`.
    fn plain<T: Row>(
        &self,
        out: &mut dyn Write,
        rows: &[T],
        chosen: Option<&[&'static str]>,
    ) -> std::io::Result<()> {
        match chosen {
            Some(chosen) => {
                let records = rows.iter().map(|row| fields::plain(row, chosen));
                format::plain(out, chosen, records, self.null, self.header)
            }
            None => format::plain(
                out,
                T::FIELDS,
                rows.iter().map(Row::plain),
                self.null,
                self.header,
            ),
        }
    }

    /// Writes `value`, an envelope of `rows` with `warnings`, as JSON:
    /// whole, or past `--max-chars` (without `-o`) to a file, and on stdout
    /// the stub that names it with the first rows. `scan` and `verb` name
    /// the file.
    fn json<V: Serialize, T: Serialize>(
        &self,
        value: &V,
        rows: &[T],
        warnings: &[Warning],
        scan: Option<&str>,
        verb: &str,
    ) -> Result<(), Failure> {
        let text = serde_json::to_string(value).map_err(|e| {
            Failure::new(
                Exit::Runtime,
                "io_error",
                format!("cannot write the output: {e}"),
            )
        })?;
        let chars = text.chars().count();
        let spill_dir = self
            .spill_dir
            .as_deref()
            .filter(|_| self.file.is_none() && self.max_chars > 0 && chars > self.max_chars);
        let Some(dir) = spill_dir else {
            return self.sink(|out| {
                out.write_all(text.as_bytes())?;
                out.write_all(b"\n")
            });
        };
        let path = spill::write(dir, scan, verb, text.as_bytes())?;
        let stub = spill::stub(
            rows.iter().collect(),
            warnings,
            &path,
            rows.len() as u64,
            text.len() as u64 + 1,
            chars,
            self.max_chars,
        );
        self.sink(|out| format::json(out, &stub))
    }

    /// Writes to the `-o` file, created or emptied first, or to stdout.
    fn sink(
        &self,
        write: impl FnOnce(&mut dyn Write) -> std::io::Result<()>,
    ) -> Result<(), Failure> {
        let Some(path) = &self.file else {
            return emit(write);
        };
        let failed = |e: std::io::Error| {
            Failure::new(
                Exit::Runtime,
                "io_error",
                format!("cannot write {}: {e}", path.display()),
            )
        };
        let file = std::fs::File::create(path).map_err(failed)?;
        let mut out = std::io::BufWriter::new(file);
        write(&mut out).and_then(|()| out.flush()).map_err(failed)
    }

    /// Writes `bytes` as they are (the treemap's escapes and cells) to the
    /// `-o` file or stdout.
    pub fn bytes(&self, bytes: &[u8]) -> Result<(), Failure> {
        self.sink(|out| out.write_all(bytes))
    }

    /// Reports a failure: the error envelope on stdout under JSON, two
    /// lines on stderr otherwise.
    pub fn failure(&self, failure: &Failure) {
        self.report(
            failure,
            |envelope| emit(|out| format::json(out, envelope)),
            to_stderr,
        );
    }

    /// Reports `failure` through `write` (the envelope to stdout) or
    /// `fallback` (stderr). An envelope that cannot be written (a full
    /// disk, a quota: the stdout that just failed) falls back to stderr, so
    /// the message is not lost; a closed pipe stays quiet, and the exit code
    /// reports the failure either way.
    fn report(
        &self,
        failure: &Failure,
        write: impl FnOnce(&ErrorEnvelope) -> Result<(), Failure>,
        fallback: impl FnOnce(&Failure),
    ) {
        if self.is_json() && write(&format::error_envelope(failure)).is_ok() {
            return;
        }
        fallback(failure);
    }
}

/// Warnings for people, on stderr.
fn warn(warnings: &[Warning]) {
    for warning in warnings {
        stderr_line(&format!("warning: {}", warning.message));
        if let Some(hint) = &warning.hint {
            stderr_line(&format!("hint: {hint}"));
        }
    }
}

/// Writes one line to stderr. Unlike `eprintln!`, it does not panic when
/// stderr is a closed pipe (`duscope … 2>&1 | head`): the line is dropped.
pub fn stderr_line(line: &str) {
    let mut err = std::io::stderr().lock();
    let _ = writeln!(err, "{line}");
}

/// A failure for people, on stderr.
pub fn to_stderr(failure: &Failure) {
    stderr_line(&format!("duscope: {}", failure.message));
    if let Some(hint) = &failure.hint {
        stderr_line(&format!("hint: {hint}"));
    }
}

#[cfg(test)]
mod tests {
    use super::format::EnvVars;
    use super::{Format, FormatArg, Output, OutputArgs};
    use crate::cli::TreemapWhen;
    use crate::verbs::{Exit, Failure};

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

    #[test]
    fn p1_cli_02_a_list_verb_asked_for_a_treemap_shows_its_table() {
        let treemap = OutputArgs {
            format: Some(FormatArg::Treemap),
            ..OutputArgs::default()
        };
        for tty in [true, false] {
            let decided = Output::decide_with(&treemap, None, &EnvVars::default(), tty).unwrap();
            assert_eq!(decided.format, Format::Table);
        }
        let env = EnvVars {
            duscope_format: Some("treemap".to_owned()),
            ..EnvVars::default()
        };
        let none = OutputArgs::default();
        assert_eq!(
            Output::decide_with(&none, None, &env, false)
                .unwrap()
                .format,
            Format::Table
        );
        // scan still draws it.
        assert_eq!(
            Output::decide_with(
                &treemap,
                Some(TreemapWhen::Auto),
                &EnvVars::default(),
                false
            )
            .unwrap()
            .format,
            Format::Treemap
        );
    }

    #[test]
    fn p1_cli_02_a_failure_stdout_cannot_take_reaches_stderr() {
        let json = Output::decide_with(
            &OutputArgs {
                json: true,
                ..OutputArgs::default()
            },
            None,
            &EnvVars::default(),
            false,
        )
        .unwrap();
        let table =
            Output::decide_with(&OutputArgs::default(), None, &EnvVars::default(), true).unwrap();
        let failure = Failure::new(Exit::Runtime, "io_error", "cannot write the output: full");
        let full = || {
            Err(Failure::new(
                Exit::Runtime,
                "io_error",
                "cannot write the output: No space left on device",
            ))
        };
        // Under JSON the envelope goes to stdout; when that fails, the
        // failure goes to stderr instead of nowhere.
        let reported = |output: &Output, write_ok: bool| {
            let mut written = None;
            let mut fallen_back = None;
            output.report(
                &failure,
                |envelope| {
                    written = Some(envelope.error.code);
                    if write_ok { Ok(()) } else { full() }
                },
                |f| fallen_back = Some(f.message.clone()),
            );
            (written, fallen_back)
        };
        assert_eq!(reported(&json, true), (Some("io_error"), None));
        assert_eq!(
            reported(&json, false),
            (Some("io_error"), Some(failure.message.clone()))
        );
        // The human formats write to stderr only.
        assert_eq!(
            reported(&table, true),
            (None, Some(failure.message.clone()))
        );
    }
}
