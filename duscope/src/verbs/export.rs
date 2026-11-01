//! `export`: a saved scan written for other tools, in the format
//! `--format` names: `ncdu` (JSON, for `ncdu -f`), `gpscan`
//! (GrandPerspective's gzip XML), `qdirstat` (QDirStat's cache file), or
//! `ndjson` and `csv` (a row an entry). The scan is the newest saved one
//! that holds the path, or one run for the verb and saved first, as for the
//! list verbs; the export is of the path's entry and what is under it. The
//! format goes to stdout or the `-o` file, a line for people to stderr.

// GrandPerspective and QDirStat are names, not code.
#![allow(clippy::doc_markdown)]

use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::Path;

use clap::ValueEnum;

use duscope_core::CoreError;
use duscope_core::interchange::{flat, gpscan, ncdu, qdirstat};
use duscope_core::query::{TreeView, clock};
use duscope_walk::Attrs;

use super::source::{self, core_failure};
use super::{Exit, Failure};
use crate::cli::ExportArgs;
use crate::out::{self, FormatArg, Output, stderr_line, thousands};

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
    let name = format
        .to_possible_value()
        .map_or_else(String::new, |v| v.get_name().to_owned());
    if format == FormatArg::Gpscan && output.file.is_none() && std::io::stdout().is_terminal() {
        return Err(Failure::new(
            Exit::Usage,
            "usage",
            "a .gpscan file is gzip, which a terminal cannot show",
        )
        .hint("Rerun duscope export --format gpscan with -o FILE.gpscan"));
    }
    let source = source::source(&args.walk, &args.index, index_dir)?;
    let warnings = source.warnings(&args.walk);
    let indexed = source.indexed("export", &args.walk, &[], output)?;
    let (index, base) = (&*indexed.index, indexed.base);
    // ncdu, GrandPerspective and QDirStat open a directory's tree.
    let tree = matches!(
        format,
        FormatArg::Ncdu | FormatArg::Gpscan | FormatArg::Qdirstat
    );
    if tree && !index.node(base).is_dir() {
        return Err(Failure::new(
            Exit::Usage,
            "usage",
            format!(
                "export --format {name} writes a directory's tree, and {} is a file",
                String::from_utf8_lossy(&duscope_core::query::abs_path(index, base))
            ),
        )
        .hint("Rerun duscope export with the directory that holds it, or --format ndjson"));
    }
    // Before any output, so an -o file is left as it was.
    if format == FormatArg::Qdirstat && index.attrs() != Attrs::Full {
        return Err(core_failure(CoreError::AttrsNotCollected(
            "export --format qdirstat",
        )));
    }
    let version = env!("CARGO_PKG_VERSION");
    // An index error is the export's failure; a write error is the
    // output's, which ends quietly on a closed pipe.
    let mut failed = None;
    let written = whole(output, |out| {
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
            Err(CoreError::ExportWrite(e)) => Err(e),
            Err(e) => {
                failed = Some(core_failure(e));
                Err(io::Error::other("the index could not be read"))
            }
        }
    });
    if let Some(failure) = failed {
        return Err(failure);
    }
    written?;
    if !output.quiet {
        out::warn(&warnings);
        let node = index.node(base);
        let entries = if node.is_dir() {
            node.file_count + node.dir_count
        } else {
            1
        };
        stderr_line(&format!(
            "exported {} entries as {name} · {}",
            thousands(entries),
            indexed.label
        ));
    }
    Ok(())
}

/// Writes the export to the `-o` file whole or not at all: into a
/// temporary file beside it, renamed over it once complete, so a failed
/// export leaves the file as it was. Stdout, and an `-o` path that is not
/// a regular file (a FIFO, `/dev/stdout`), are written as the rows come.
fn whole(
    output: &Output,
    write: impl FnOnce(&mut dyn Write) -> io::Result<()>,
) -> Result<(), Failure> {
    let regular = |path: &Path| fs::metadata(path).map_or(true, |m| m.is_file());
    let Some((path, name)) = output
        .file
        .as_deref()
        .filter(|path| regular(path))
        .and_then(|path| Some((path, path.file_name()?)))
    else {
        return output.sink(write);
    };
    let part = path.with_file_name(format!(
        ".{}.{}.part",
        name.to_string_lossy(),
        std::process::id()
    ));
    let written = fs::File::create(&part)
        .and_then(|file| {
            let mut out = io::BufWriter::new(file);
            write(&mut out)?;
            out.flush()
        })
        .and_then(|()| fs::rename(&part, path));
    written.map_err(|e| {
        if let Err(gone) = fs::remove_file(&part)
            && gone.kind() != io::ErrorKind::NotFound
        {
            log::warn!("cannot remove {}: {gone}", part.display());
        }
        Failure::new(
            Exit::Runtime,
            "io_error",
            format!("cannot write {}: {e}", path.display()),
        )
    })
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
