//! `import FILE`: a tree another tool wrote (ncdu's or gdu's JSON export,
//! or GrandPerspective's `.gpscan`; gzipped or not) saved as a scan whose
//! `scan_meta.source` is `import:<format>`. `--format` names the format, or
//! the file shows it. The output is `scan --save`'s: the root's largest
//! entries. A saved import is read where `--scan` names it, never as the
//! newest scan of its path, so `diff` can set it against a scan of this
//! machine without the list verbs reading it in place of one.
//!
//! Ctrl-C stops an import as it stops `scan --save`: while the file is
//! read, the process ends at once, nothing written; after it, the save
//! stops and leaves nothing, the output already written standing; both exit
//! 130. Once the index has its name, the save finishes.

// GrandPerspective is a name, not code.
#![allow(clippy::doc_markdown)]

use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Read};
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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
use super::scan::{self, Interrupts, Scanned};
use super::source::{Source, core_failure, saved_as};
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
    // Ctrl-C while the file is read ends the process at once, as during a
    // scan's walk; after it, Ctrl-C sets the flag alone, which stops the
    // index write. Under an MCP tool call, as for `scan`, the call's
    // cancellation and deadline set the flag instead, and Ctrl-C is left to
    // end the server.
    let interrupts = match super::mcp::running_call() {
        Some(call) => Interrupts::unarmed(call.cancel),
        None => Interrupts::arm(),
    };
    let stop = Arc::clone(&interrupts.flag);
    let read = read(&args.file, &shown, format);
    drop(interrupts);
    let (imported, root) = unless_stopped(read, &stop)?;
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
        incremental: None,
        fallback: None,
        source: imported.format.source(),
        full_disk_access_missing: false,
        stop,
    };
    let id = scan::scan_id(&scanned)?;
    let fresh = Source::Fresh {
        scanned: Box::new(scanned),
        save: None,
    };
    let (mut envelope, footer, fresh) =
        top_envelope(fresh, &walk, None, "import", false, true, None, output)?;
    saved_as(&mut envelope, id.to_string());
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
    scan::save(&scanned, &walk, &[], &id, Some(&dir), output, true).map_err(|failure| {
        scan::save_hint(
            failure,
            "Run duscope import again with --index-dir naming a writable directory",
        )
    })
}

/// `read`'s result, or `interrupted` for a read that failed once `stop` was
/// set: the pipe it read ends when the same Ctrl-C ends the program writing
/// it, which may beat the watcher to the guard's lock. A read that
/// succeeded stands; the flag then stops the save.
fn unless_stopped<T>(read: Result<T, Failure>, stop: &AtomicBool) -> Result<T, Failure> {
    match read {
        Err(_) if stop.load(Ordering::SeqCst) => Err(Failure::new(
            Exit::Interrupted,
            "interrupted",
            "the import was interrupted",
        )),
        read => read,
    }
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

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::atomic::AtomicBool;

    use duscope_testkit::TestEnv;

    use super::{read, unless_stopped};
    use crate::verbs::Exit;

    /// An ncdu export of `/srv/imported` and one file, cut after the file
    /// when `whole` is false, as a pipe leaves it when Ctrl-C ends ncdu.
    fn export(whole: bool) -> String {
        let mut export = String::from(
            "[1,2,{\"progname\":\"ncdu\",\"progver\":\"2.4\",\"timestamp\":2114380800},\n\
             [{\"name\":\"/srv/imported\",\"asize\":4096,\"dsize\":4096},\n\
             {\"name\":\"a\",\"asize\":1,\"dsize\":4096}",
        );
        if whole {
            export.push_str("]]\n");
        }
        export
    }

    /// The read of a file cut short is the import's usage error without
    /// Ctrl-C, and `interrupted`, exit 130, once the flag is set; a read
    /// that succeeded stands with the flag set, which then stops the save.
    #[test]
    fn p5_core_09_a_read_that_fails_once_the_flag_is_set_is_interrupted() {
        let env = TestEnv::new();
        let cut = env.work_dir().join("cut.json");
        std::fs::write(&cut, export(false)).unwrap();
        let whole = env.work_dir().join("whole.json");
        std::fs::write(&whole, export(true)).unwrap();
        let unset = AtomicBool::new(false);
        let set = AtomicBool::new(true);

        let failure = unless_stopped(read(&cut, "cut.json", None), &unset).unwrap_err();
        assert_eq!((failure.exit, failure.code), (Exit::Usage, "usage"));
        assert!(failure.message.contains("cut.json"), "{failure:?}");

        let failure = unless_stopped(read(&cut, "cut.json", None), &set).unwrap_err();
        assert_eq!(
            (failure.exit, failure.code),
            (Exit::Interrupted, "interrupted")
        );
        assert_eq!(failure.message, "the import was interrupted");
        assert_eq!(failure.hint, None);

        let (imported, root) = unless_stopped(read(&whole, "whole.json", None), &set).unwrap();
        assert_eq!(root, Path::new("/srv/imported"));
        assert_eq!(imported.arena.len(), 2);
    }
}
