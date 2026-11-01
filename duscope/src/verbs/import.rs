//! `import FILE`: a tree another tool wrote (ncdu's or gdu's JSON export,
//! or GrandPerspective's `.gpscan`; gzipped or not) saved as a scan whose
//! `scan_meta.source` is `import:<format>`. `--format` names the format, or
//! the file shows it. The output is `scan --save`'s: the root's largest
//! entries. A saved import is read where `--scan` names it, never as the
//! newest scan of its path, so `diff` can set it against a scan of this
//! machine without the list verbs reading it in place of one.

// GrandPerspective is a name, not code.
#![allow(clippy::doc_markdown)]

use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Read};
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::time::Instant;

use duscope_core::CoreError;
use duscope_core::envelope::TopEntry;
use duscope_core::index::index_path;
use duscope_core::interchange::import::{self, ImportFormat};
use duscope_core::query::clock;
use duscope_core::rollup::roll_up;
use duscope_walk::WalkSummary;

use super::next;
use super::query::top_envelope;
use super::scan::{self, Scanned};
use super::source::{Source, core_failure};
use super::{Exit, Failure};
use crate::cli::{AttrsArg, ImportArgs, WalkArgs};
use crate::out::{FormatArg, Output};

/// `import`.
pub fn run(args: &ImportArgs, index_dir: Option<&Path>, output: &Output) -> Result<(), Failure> {
    let format = match args.format {
        None => None,
        Some(FormatArg::Ncdu) => Some(ImportFormat::Ncdu),
        Some(FormatArg::Gdu) => Some(ImportFormat::Gdu),
        Some(FormatArg::Gpscan) => Some(ImportFormat::Gpscan),
        Some(_) => {
            return Err(Failure::new(
                Exit::Usage,
                "usage",
                "import reads --format ncdu, gdu or gpscan",
            )
            .hint("Rerun duscope import without --format, which the file then shows"));
        }
    };
    // A wrong --fields costs no read.
    if output.fields_first::<TopEntry>("import")? {
        return Ok(());
    }
    let dir = index_dir
        .map(Path::to_owned)
        .or_else(|| duscope_core::index::default_dir(|name| std::env::var_os(name)))
        .ok_or_else(|| {
            Failure::new(
                Exit::Runtime,
                "index_write_failed",
                "no index directory to save the import in: neither HOME nor XDG_CACHE_HOME is set",
            )
            .hint("Run duscope import with --index-dir naming a writable directory")
        })?;
    let started = Instant::now();
    let shown = args.file.display().to_string();
    let (imported, root) = read(&args.file, &shown, format)?;
    let started_ns = imported.scanned_at.unwrap_or_else(clock::now);
    let mut arena = imported.arena;
    let rollup = roll_up(&mut arena);
    let summary = WalkSummary {
        entries: arena.len() as u64,
        dirs: arena.dir_count() as u64,
        unreadable_dirs: imported.unreadable_dirs,
        entry_errors: imported.entry_errors,
        skipped_dirs: imported.skipped_dirs,
        excluded: imported.excluded,
        mounts_excluded: imported.mounts_excluded,
        ..WalkSummary::default()
    };
    let walk = walk(&root);
    let scanned = Scanned {
        arena,
        rollup,
        summary,
        elapsed: started.elapsed(),
        root: root.clone(),
        root_as_given: root,
        clones_asked: false,
        started_ns,
        finished_ns: started_ns,
        parent: None,
        source: imported.format.source(),
    };
    let id = scan::scan_id(&scanned)?;
    let fresh = Source::Fresh {
        scanned: Box::new(scanned),
        save: None,
    };
    let (mut envelope, footer, fresh) =
        top_envelope(fresh, &walk, None, "import", false, true, None, output)?;
    envelope.scan.id = Some(id.to_string());
    let file = index_path(&dir, &id);
    envelope.next = next::list(&envelope, Some(&file), None);
    let footer = format!(
        "{footer} · imported from {shown} ({})",
        imported.format.as_str()
    );
    output.list(&envelope, &footer)?;
    let Source::Fresh { scanned, .. } = fresh else {
        return Ok(());
    };
    scan::save(&scanned, &walk, &[], &id, Some(&dir), output, true)
}

/// The tree in `file` (`-` for stdin), and its root, absolute: one the
/// file gives relative is taken from the working directory.
fn read(
    file: &Path,
    shown: &str,
    format: Option<ImportFormat>,
) -> Result<(import::Imported, PathBuf), Failure> {
    let cannot = |e: io::Error| {
        Failure::new(
            Exit::Runtime,
            "io_error",
            format!("cannot read {shown}: {e}"),
        )
    };
    let input: Box<dyn Read> = if file.as_os_str() == "-" {
        Box::new(io::stdin().lock())
    } else {
        Box::new(File::open(file).map_err(cannot)?)
    };
    let mut imported = import::import(input, format).map_err(|e| match e {
        CoreError::ImportInvalid(why) => {
            core_failure(CoreError::ImportInvalid(format!("{shown}: {why}")))
        }
        CoreError::ImportRead(e) => cannot(e),
        e => core_failure(e),
    })?;
    let root = PathBuf::from(OsString::from_vec(std::mem::take(&mut imported.root)));
    let root = if root.is_absolute() {
        root
    } else {
        std::env::current_dir().map_err(cannot)?.join(root)
    };
    Ok((imported, root))
}

/// The options an import is saved with: a plain scan's of `root`, which
/// no tool here ran.
fn walk(root: &Path) -> WalkArgs {
    WalkArgs {
        path: Some(root.to_owned()),
        threads: None,
        one_file_system: false,
        include_network: false,
        follow_symlinks: false,
        size: None,
        apparent_size: false,
        attrs: AttrsArg::Basic,
        clones: false,
        no_clones: false,
        exclude_from: Vec::new(),
        ignore_from: Vec::new(),
        exclude_caches: false,
        skip_containers: false,
        provider: None,
        test_now: None,
        config_excludes: Vec::new(),
        classes_extra: None,
        caches_extra: None,
    }
}
