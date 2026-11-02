//! `view [path|scan]` and `scan -i`: the interactive viewer, which takes the
//! terminal (raw mode, the alternate screen, the mouse) until `q` or
//! `Ctrl-C` and gives it back as it found it. `view` reads the newest saved
//! scan that holds the path, or the one `--scan` or the argument names (a
//! scan id where no such path exists), or scans the path and saves it as
//! the list verbs do; `scan -i` scans and opens the result, saved with
//! `--save`. Both refuse, before any scan, a stdout that is not a terminal,
//! an output format other than people's, and a terminal below 80×24.

use std::ffi::OsString;
use std::io::{self, IsTerminal};
use std::path::Path;

use duscope_core::envelope::Warning;
use duscope_core::query::{self, ScanSelector, TreeView};
use duscope_tui::{App, Background, MIN_SIZE, Tui};

use super::{Exit, Failure, scan, source, treemap};
use crate::cli::{ColorBy, IndexArgs, MinTile, ScanArgs, ViewArgs, WalkArgs};
use crate::out::{Format, Output};

/// The hint of every refusal.
const HINT: &str = "Run duscope view in a terminal of at least 80×24, or duscope top or duscope tree for text output";

/// `view`.
pub fn run(args: &ViewArgs, index_dir: Option<&Path>, output: &Output) -> Result<(), Failure> {
    refused(output)?;
    // The colours are settled first: a theme the map refuses costs no scan.
    treemap::viewer_style(args.theme.as_ref(), output.color, None)?;
    let (walk, index) = named_scan(&args.walk, &args.index, index_dir);
    let source = source::source(&walk, &index, index_dir)?;
    source.check()?;
    // A scan run for the viewer is saved before the terminal is taken, so
    // nothing the viewer meets can lose it.
    source.finish(&walk, output)?;
    let size = scan::size_kind(&walk);
    let block = source.scan_block(&walk, size);
    let look = Look {
        theme: args.theme.as_ref(),
        color_by: args.color_by,
        depth: None,
        min_tile: None,
        pool_files: false,
    };
    let view = source.view();
    show(
        &*view,
        &walk,
        &look,
        source.footer(&block, output),
        &source.warnings(&walk),
        output,
    )
}

/// `scan -i`: the scan, saved with `--save`, then the viewer on it.
pub fn scan(args: &ScanArgs, index_dir: Option<&Path>, output: &Output) -> Result<(), Failure> {
    refused(output)?;
    // The map's flags as scan refuses them (`--treemap pixels`, a bad
    // theme), before the scan.
    treemap::style(args, output)?;
    let scanned = scan::scan(&args.walk, &args.exclude)?;
    if args.save {
        let id = scan::scan_id(&scanned)?;
        scan::save(
            &scanned,
            &args.walk,
            &args.exclude,
            &id,
            index_dir,
            output,
            true,
        )?;
    }
    let size = scan::size_kind(&args.walk);
    let block = scan::scan_block(&scanned, &args.walk, size);
    let look = Look {
        theme: args.theme.as_ref(),
        color_by: args.color_by,
        depth: args.depth,
        min_tile: args.min_tile,
        pool_files: args.pool_files,
    };
    show(
        &scanned.view(),
        &args.walk,
        &look,
        scan::footer(&block, &scanned, output),
        &scan::warnings(&scanned),
        output,
    )
}

/// The map's flags, as `view` or `scan` took them.
struct Look<'a> {
    theme: Option<&'a OsString>,
    color_by: Option<ColorBy>,
    depth: Option<u32>,
    min_tile: Option<MinTile>,
    pool_files: bool,
}

/// Refuses what the viewer cannot draw on: stdout is not a terminal (a
/// pipe, a file, `-o`), the output asked for is not people's (`--json`,
/// `--plain`, `--format`, or an agent's environment), or the terminal is
/// below [`MIN_SIZE`]. Each is a usage error, exit 2.
fn refused(output: &Output) -> Result<(), Failure> {
    let usage = |message: String| Failure::new(Exit::Usage, "usage", message).hint(HINT);
    if output.file.is_some() || !io::stdout().is_terminal() {
        return Err(usage(
            "the viewer draws on a terminal, and stdout is not one".to_owned(),
        ));
    }
    if !matches!(output.format, Format::Table | Format::Treemap) {
        return Err(usage(
            "the viewer draws on a terminal, which an output format for programs does not"
                .to_owned(),
        ));
    }
    let (w, h) = duscope_tui::size().map_err(|e| terminal_failure(&e))?;
    if w < MIN_SIZE.0 || h < MIN_SIZE.1 {
        return Err(usage(format!(
            "the viewer needs a terminal of at least {}×{}, and this one is {w}×{h}",
            MIN_SIZE.0, MIN_SIZE.1
        )));
    }
    Ok(())
}

/// `view a3f9`: an argument that names no path but names a saved scan
/// (`latest`, or the leading digits of a saved scan's id) is `--scan`,
/// unless `--scan` is given. A word that only looks like an id (`cafe`)
/// stays a path, and fails as one.
fn named_scan(
    walk: &WalkArgs,
    index: &IndexArgs,
    index_dir: Option<&Path>,
) -> (WalkArgs, IndexArgs) {
    let (mut walk, mut index) = (walk.clone(), index.clone());
    let saved = |prefix: &str| {
        index_dir
            .map(Path::to_owned)
            .or_else(|| duscope_core::index::default_dir(|name| std::env::var_os(name)))
            .is_some_and(|dir| {
                query::index_files(&dir)
                    .iter()
                    .any(|(id, _)| id.as_str().starts_with(prefix))
            })
    };
    if index.scan.is_none()
        && let Some(arg) = walk.path.as_ref().and_then(|p| p.to_str())
        && !Path::new(arg).exists()
        && match ScanSelector::parse(arg) {
            Ok(ScanSelector::Latest) => true,
            Ok(ScanSelector::Prefix(prefix)) => saved(&prefix),
            _ => false,
        }
    {
        index.scan = Some(arg.to_owned());
        walk.path = None;
    }
    (walk, index)
}

/// Runs the viewer on `view` until `q` or `Ctrl-C`: takes the terminal
/// (asking it for its background when the colours depend on it), draws,
/// answers events, and gives the terminal back; then the warnings on
/// stderr, which the frame has room for one of.
fn show(
    view: &dyn TreeView,
    walk: &WalkArgs,
    look: &Look<'_>,
    summary: String,
    warnings: &[Warning],
    output: &Output,
) -> Result<(), Failure> {
    let classes = super::query::classes(walk.classes_extra.as_deref(), "[classes] extra")?;
    let size = scan::size_kind(walk);
    let now = scan::now(walk)?;
    let warning = treemap::frame_warning(warnings);
    // test-only: DUSCOPE_TEST_PANIC=1 panics once the first frame is drawn,
    // in debug builds alone, for the tests of the panic hook.
    let test_panic =
        cfg!(debug_assertions) && std::env::var_os("DUSCOPE_TEST_PANIC").is_some_and(|v| v == "1");
    let probe = treemap::asks_background(look.theme, output.color);
    let mut tui = Tui::enter(probe).map_err(|e| terminal_failure(&e))?;
    let light = tui.background().map(|b| b == Background::Light);
    let style = treemap::viewer_style(look.theme, output.color, light)?;
    let opts = style.paint_opts(
        look.color_by,
        look.depth,
        look.min_tile,
        look.pool_files,
        size,
        now,
        &classes,
    );
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let mut app = App::new(view, opts, style.depth(), home.as_deref(), summary, warning);
    duscope_tui::run(tui.terminal(), &mut app, || {
        assert!(
            !test_panic,
            "DUSCOPE_TEST_PANIC: a panic after the first frame"
        );
        duscope_tui::next_event()
    })
    .map_err(|e| terminal_failure(&e))?;
    drop(tui);
    if !output.quiet {
        crate::out::warn(warnings);
    }
    Ok(())
}

/// The terminal failed: a read, a write or the size.
fn terminal_failure(e: &io::Error) -> Failure {
    Failure::new(
        Exit::Runtime,
        "io_error",
        format!("the terminal failed: {e}"),
    )
}
