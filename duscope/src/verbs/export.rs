//! `export`: a saved scan written for other tools, in the format
//! `--format` names: `ncdu` (JSON, for `ncdu -f`), `gpscan`
//! (GrandPerspective's gzip XML), `qdirstat` (QDirStat's cache file), or
//! `ndjson` and `csv` (a row an entry). The scan is the newest saved one
//! that holds the path, or one run for the verb and saved first, as for the
//! list verbs; the export is of the path's entry and what is under it. The
//! format goes to stdout or the `-o` file, a line for people to stderr.

// GrandPerspective and QDirStat are names, not code.
#![allow(clippy::doc_markdown)]

use std::io::IsTerminal;
use std::path::Path;

use clap::ValueEnum;

use duscope_core::interchange::{ExportError, flat, gpscan, ncdu, qdirstat};
use duscope_core::query::{TreeView, clock};

use super::source::{self, core_failure};
use super::{Exit, Failure};
use crate::cli::ExportArgs;
use crate::out::{FormatArg, Output, stderr_line, thousands};

/// `export`.
pub fn run(
    args: &ExportArgs,
    format: Option<FormatArg>,
    index_dir: Option<&Path>,
    output: &Output,
) -> Result<(), Failure> {
    let Some(
        format @ (FormatArg::Ncdu
        | FormatArg::Gpscan
        | FormatArg::Qdirstat
        | FormatArg::Ndjson
        | FormatArg::Csv),
    ) = format
    else {
        return Err(Failure::new(
            Exit::Usage,
            "usage",
            "export writes --format ncdu, gpscan, qdirstat, ndjson or csv",
        )
        .hint("Rerun duscope export with --format ncdu, or another of the five"));
    };
    if format == FormatArg::Gpscan && output.file.is_none() && std::io::stdout().is_terminal() {
        return Err(Failure::new(
            Exit::Usage,
            "usage",
            "a .gpscan file is gzip, which a terminal cannot show",
        )
        .hint("Rerun duscope export --format gpscan with -o FILE.gpscan"));
    }
    let source = source::source(&args.walk, &args.index, index_dir)?;
    let indexed = source.indexed(&args.walk, &[], output)?;
    let (index, base) = (&*indexed.index, indexed.base);
    let version = env!("CARGO_PKG_VERSION");
    // An index error is the export's failure; a write error is the sink's,
    // which ends quietly on a closed pipe.
    let mut failed = None;
    output.sink(|out| {
        let written = match format {
            FormatArg::Ncdu => {
                let opts = ncdu::NcduOpts {
                    progver: version,
                    timestamp: clock::now().div_euclid(1_000_000_000),
                };
                ncdu::write(index, base, &opts, out)
            }
            FormatArg::Gpscan => {
                let opts = gpscan_opts(index, base, &args.walk, version);
                gpscan::write(index, base, &opts, out)
            }
            FormatArg::Qdirstat => qdirstat::write(index, base, version, out),
            FormatArg::Csv => flat::write(index, base, flat::Flat::Csv, out),
            _ => flat::write(index, base, flat::Flat::Ndjson, out),
        };
        match written {
            Ok(()) => Ok(()),
            Err(ExportError::Write(e)) => Err(e),
            Err(ExportError::Index(e)) => {
                failed = Some(core_failure(e));
                Ok(())
            }
        }
    })?;
    if let Some(failure) = failed {
        return Err(failure);
    }
    if !output.quiet {
        let node = index.node(base);
        let entries = if node.is_dir() {
            node.file_count + node.dir_count
        } else {
            1
        };
        let name = format
            .to_possible_value()
            .map_or_else(String::new, |v| v.get_name().to_owned());
        stderr_line(&format!(
            "exported {} entries as {name} · {}",
            thousands(entries),
            indexed.label
        ));
    }
    Ok(())
}

/// What `ScanInfo` says: the exported entry's volume at the scan's start
/// (the one with the longest mount point above it; else, where the path
/// reaches its volume by other means, as macOS's firmlinks do, the root's
/// volume, at `/`), and the measure.
fn gpscan_opts(
    index: &duscope_core::index::Index,
    base: u32,
    walk: &crate::cli::WalkArgs,
    version: &'static str,
) -> gpscan::GpscanOpts {
    let path = duscope_core::query::abs_path(index, base);
    let volumes = index.volumes();
    let holding = volumes
        .iter()
        .filter(|v| {
            path.starts_with(&v.mount_point)
                && (path.len() == v.mount_point.len()
                    || v.mount_point.ends_with(b"/")
                    || path.get(v.mount_point.len()) == Some(&b'/'))
        })
        .max_by_key(|v| v.mount_point.len());
    let (volume_path, volume) = match holding {
        Some(v) => (v.mount_point.clone(), Some(v)),
        None => (b"/".to_vec(), volumes.first()),
    };
    let apparent = super::scan::size_kind(walk) == duscope_core::query::SizeKind::Apparent;
    gpscan::GpscanOpts {
        apparent,
        volume_path,
        volume_size: volume.and_then(|v| v.total_bytes).unwrap_or(0),
        free_space: volume.and_then(|v| v.free_bytes).unwrap_or(0),
        scan_time_ns: index.meta().started_at,
        app_version: version,
    }
}
