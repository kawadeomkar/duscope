//! `view [path|scan]` and `scan -i`: the interactive viewer, which takes the
//! terminal (raw mode, the alternate screen, the mouse) until `q` or
//! `Ctrl-C` and gives it back as it found it. `view` reads the newest saved
//! scan that holds the path, or the one `--scan` or the argument names (a
//! scan id where no such path exists), or scans the path and saves it as
//! the list verbs do; `scan -i` scans and opens the result, saved with
//! `--save`. Both refuse, before any scan, a stdout that is not a terminal,
//! an output format other than people's, and a terminal below 80×24.
//!
//! `i` in the viewer reads `explain` of the saved scan the viewer shows, or
//! of the one a scan run for it was saved as; a scan not saved is answered
//! from a fresh `stat`, as `explain` answers a path no saved scan holds.
//!
//! `y` writes OSC 52 straight to the terminal, between frames; `o` starts
//! the command lines that show the selection in the file manager
//! (`duscope_walk::reveal_commands`), detached from the terminal.
//!
//! `D` plans against the saved scan the viewer shows, or the one a scan run
//! for it was saved as. `r` runs a rescan beside the viewer, on a thread of
//! its own: of the folder the viewer is rooted at, alone, where that lies
//! below the saved scan's root (`view ~/proj` over a scan of `~`); of the
//! saved scan again as `rescan` does it otherwise; or the walk again where
//! nothing was saved. The viewer reloads on the new index; the terminal
//! stays the viewer's, so nothing is logged while it runs. When the viewer
//! closes, what it moved to the Trash is printed on stderr, and the
//! `index_untouched` warning with it.

use std::cell::Cell;
use std::ffi::OsString;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use duscope_core::envelope::{ScanBlock, Warning};
use duscope_core::index::{Index, index_path};
use duscope_core::query::{self, ScanSelector, TreeView};
use duscope_core::rules::{Caches, Classes};
use duscope_treemap::packs::Pack;
use duscope_treemap::pixels::Pixels;
use duscope_tui::{
    App, Background, Carried, Effects, Ended, Input, MIN_SIZE, PlanJob, Planned, Planner, Rescan,
    ScanFacts, Session, Sources, Tui,
};

use super::scan::Scanned;
use super::source::{Source, core_failure, scanned};
use super::treemap::shell_quote;
use super::{Exit, Failure, next, rescan, scan, source, treemap};
use crate::cli::{ColorBy, IndexArgs, MinTile, ScanArgs, TreemapWhen, ViewArgs, WalkArgs};
use crate::out::{Format, Output, stderr_line, thousands};

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
    let look = Look {
        theme: args.theme.as_ref(),
        color_by: args.color_by,
        depth: None,
        min_tile: None,
        pool_files: false,
        pixels: false,
    };
    let file = source.index_file();
    show(source, file, &walk, &[], &look, index_dir, output)
}

/// What `i` reads besides the view: the index a scan run for the viewer
/// was saved as, and the scan's block.
struct Facts<'a> {
    index: Option<&'a Index>,
    scan: ScanFacts,
}

/// The saved scan in `file`; `None`, and the facts read from a fresh
/// `stat`, where it cannot be opened, which the log says.
fn open(file: &Path) -> Option<Index> {
    Index::open(file)
        .inspect_err(|e| log::warn!("the facts read no saved scan: {e}"))
        .ok()
}

/// The scan's facts from its `block`: the root volume's size and free
/// space, what it uses that the scan did not find, when the scan ran
/// (`times`, Unix nanoseconds) and its threads.
fn scan_facts(block: &ScanBlock, unaccounted: Option<u64>, times: (i64, i64)) -> ScanFacts {
    ScanFacts {
        volume: block
            .volumes
            .first()
            .filter(|v| v.total_bytes > 0)
            .map(|v| (v.total_bytes, v.free_bytes)),
        unaccounted_bytes: unaccounted,
        times: Some(times),
        threads: Some(block.thread_count),
    }
}

/// `scan -i`: the scan, saved with `--save`, then the viewer on it.
pub fn scan(args: &ScanArgs, index_dir: Option<&Path>, output: &Output) -> Result<(), Failure> {
    refused(output)?;
    // The map's flags as scan refuses them (`--treemap pixels`, a bad
    // theme), before the scan.
    treemap::style(args, output)?;
    let scanned = scan::scan(&args.walk, &args.exclude)?;
    // Where scan::save wrote it, which `explain`, `D` and `r` read.
    let mut file = None;
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
        file = index_dir
            .map(Path::to_owned)
            .or_else(|| duscope_core::index::default_dir(|name| std::env::var_os(name)))
            .map(|dir| index_path(&dir, &id));
    }
    let look = Look {
        theme: args.theme.as_ref(),
        color_by: args.color_by,
        depth: args.depth,
        min_tile: args.min_tile,
        pool_files: args.pool_files,
        pixels: args.treemap == Some(TreemapWhen::Pixels),
    };
    let source = Source::Fresh {
        scanned: Box::new(scanned),
        save: None,
    };
    show(
        source,
        file,
        &args.walk,
        &args.exclude,
        &look,
        index_dir,
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
    /// `--treemap pixels` (`scan -i`).
    pixels: bool,
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

/// The scan the viewer shows and the file it is saved in, if it is.
struct Shown {
    source: Source,
    file: Option<PathBuf>,
}

/// What the viewers of one terminal session share: everything one reload
/// keeps.
struct Shared<'a> {
    walk: &'a WalkArgs,
    excludes: &'a [String],
    look: &'a Look<'a>,
    index_dir: Option<&'a Path>,
    output: &'a Output,
    classes: &'a Classes,
    caches: &'a Caches,
    size: duscope_core::query::SizeKind,
    now: i64,
    test_panic: bool,
    style: &'a treemap::Style,
    packs: &'a [Pack],
    pixels: Option<Pixels>,
    /// What the rescan thread sends when it ends.
    rx: mpsc::Receiver<Result<Landing, Failure>>,
    tx: mpsc::Sender<Result<Landing, Failure>>,
    /// A rescan is running beside the viewer.
    running: Cell<bool>,
    /// What each plan of the mark pane comes back on.
    plan_rx: mpsc::Receiver<Planned>,
    /// What a plan's thread answers on.
    plan_tx: mpsc::Sender<Planned>,
    /// The plans still running, those given up on included.
    planning: Cell<u32>,
}

/// What a rescan has to show for itself.
enum Landing {
    /// The saved scan it wrote.
    Saved(PathBuf),
    /// The scan, for a viewer whose scan was not saved.
    Walked(Box<Scanned>),
}

/// How the viewer got to its next state.
enum Next {
    /// `q` or `Ctrl-C`: the warnings of what it showed last, and what the
    /// session moved.
    Quit {
        warnings: Vec<Warning>,
        session: Session,
    },
    /// `r` landed: what the viewer kept and the new scan.
    Reload { carried: Box<Carried>, shown: Shown },
}

/// Runs the viewer on `source` until `q` or `Ctrl-C`: takes the terminal
/// (asking it for its background when the colours depend on it), draws,
/// answers events, reloads on what `r` scanned, and gives the terminal back;
/// then the warnings on stderr, which the frame has room for one of, and
/// what the session moved to the Trash. `file` is the saved scan it shows,
/// if it is saved: what `i`, `D` and `r` read.
fn show(
    source: Source,
    file: Option<PathBuf>,
    walk: &WalkArgs,
    excludes: &[String],
    look: &Look<'_>,
    index_dir: Option<&Path>,
    output: &Output,
) -> Result<(), Failure> {
    let classes = super::query::classes(walk.classes_extra.as_deref(), "[classes] extra")?;
    let caches = scan::cache_rules(walk.caches_extra.as_deref())?;
    let size = scan::size_kind(walk);
    let now = scan::now(walk)?;
    // test-only: DUSCOPE_TEST_PANIC=1 panics once the first frame is drawn,
    // in debug builds alone, for the tests of the panic hook.
    let test_panic =
        cfg!(debug_assertions) && std::env::var_os("DUSCOPE_TEST_PANIC").is_some_and(|v| v == "1");
    let probe = treemap::asks_background(look.theme, output.color);
    let mut tui = Tui::enter(probe).map_err(|e| terminal_failure(&e))?;
    let light = tui.background().map(|b| b == Background::Light);
    let style = treemap::viewer_style(look.theme, output.color, light)?;
    // The pixel probe needs the terminal held and its events not yet read;
    // a build without the feature compiles none of it.
    let (pixels, in_cells) = if duscope_treemap::pixels::COMPILED && look.pixels {
        match treemap::pixels(&style, output)? {
            treemap::Drawing::Pixels(found) => (Some(found), None),
            treemap::Drawing::Cells(why) => (None, Some(why)),
        }
    } else {
        (None, None)
    };
    let packs = style.packs();
    let (tx, rx) = mpsc::channel();
    let (plan_tx, plan_rx) = mpsc::channel();
    let shared = Shared {
        walk,
        excludes,
        look,
        index_dir,
        output,
        classes: &classes,
        caches: &caches,
        size,
        now,
        test_panic,
        style: &style,
        packs: &packs,
        pixels,
        rx,
        tx,
        running: Cell::new(false),
        plan_rx,
        plan_tx,
        planning: Cell::new(0),
    };
    let mut shown = Shown { source, file };
    let mut carried = None;
    let (warnings, session) = loop {
        match viewer(&mut tui, &shown, carried.take(), &shared)? {
            Next::Quit { warnings, session } => break (warnings, session),
            Next::Reload {
                carried: kept,
                shown: next,
            } => {
                carried = Some(*kept);
                shown = next;
            }
        }
    };
    drop(tui);
    if let Some(why) = in_cells {
        log::warn!("--treemap pixels: {why}, so the map was drawn in cells");
    }
    if !output.quiet {
        crate::out::warn(&warnings);
        if session.moved_count > 0 {
            stderr_line(&moved_line(session, output));
            crate::out::warn(&[index_untouched(&shown)]);
        }
    }
    // A saved scan whose rows the viewer found damaged as it walked them
    // fails as any verb's would, once the terminal is given back.
    shown.source.check()
}

/// One viewer of `shown` on the terminal, until `q`, `Ctrl-C` or a landed
/// rescan; `carried` is what the viewer before it kept.
fn viewer(
    tui: &mut Tui,
    shown: &Shown,
    carried: Option<Carried>,
    shared: &Shared<'_>,
) -> Result<Next, Failure> {
    let (walk, look, output, size) = (shared.walk, shared.look, shared.output, shared.size);
    let source = &shown.source;
    let block = source.scan_block(walk, size);
    let summary = source.footer(&block, output);
    let warnings = source.warnings(walk);
    let warning = treemap::frame_warning(&warnings);
    // A scan run for the viewer is in the index now, unless not saved:
    // `explain` reads it there. A saved scan's view reads its own.
    let saved = match source {
        Source::Fresh { .. } => shown.file.as_deref().and_then(open),
        Source::Saved { .. } => None,
    };
    let unaccounted = match source {
        Source::Saved { index, .. } => index.meta().unaccounted_bytes,
        Source::Fresh { scanned, .. } => scan::unaccounted(scanned),
    };
    let facts = Facts {
        index: saved.as_ref(),
        scan: scan_facts(
            &block,
            unaccounted,
            (source.started_ns(), source.finished_ns()),
        ),
    };
    let view = source.view();
    let opts = shared.style.paint_opts(
        look.color_by,
        look.depth,
        look.min_tile,
        look.pool_files,
        size,
        shared.now,
        shared.classes,
    );
    let home = std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from);
    let mut app = App::new(
        &*view,
        opts,
        shared.style.depth(),
        home.as_deref(),
        summary,
        warning,
    );
    app.with_sources(Sources {
        // A scan run for the viewer and saved has the arena's ids.
        index: facts.index.map(|index| (index, 0)),
        caches: shared.caches.clone(),
        env: Box::new(|name| std::env::var_os(name)),
        scan: facts.scan,
    });
    app.with_packs(shared.packs);
    app.with_ls_colors(shared.style.ls_colors());
    let start: Rescan<'_> = Box::new(|| start_rescan(shown, shared));
    let plan: Planner<'_> = Box::new(|job| start_plan(job, shared));
    app.with_effects(Effects {
        write: Box::new(|bytes| {
            let mut out = io::stdout();
            out.write_all(bytes)?;
            out.flush()
        }),
        open: test_opener().unwrap_or_else(|| {
            Box::new(|path| duscope_tui::open_detached(duscope_walk::reveal_commands(path)))
        }),
        env: Box::new(|name| std::env::var_os(name)),
        size: Box::new(|bytes| output.units.size(bytes)),
        rescan: Some(start),
        plan: Some(plan),
    });
    if let Some(found) = &shared.pixels {
        app.draw_in_pixels(found.clone());
    }
    if let Some(carried) = carried {
        app.restore(carried);
    }
    let root = view.root_path().to_owned();
    let mut landed = None;
    let ended = duscope_tui::run_session(tui.terminal(), &mut app, || {
        next_input(shared, &root, &mut landed)
    })
    .map_err(|e| terminal_failure(&e))?;
    match ended {
        Ended::Quit => Ok(Next::Quit {
            warnings,
            session: app.session(),
        }),
        Ended::Reload => {
            let carried = Box::new(app.carry());
            match landed {
                Some(shown) => Ok(Next::Reload { carried, shown }),
                // The landing is what made the viewer return: there is one.
                None => Err(Failure::new(
                    Exit::Runtime,
                    "io_error",
                    "the rescan ended without a scan to show",
                )),
            }
        }
    }
}

/// The next thing the viewer answers: a terminal event, waited for without
/// waking while nothing is running; while `r`'s rescan or `D`'s plan runs,
/// what it ends with, the terminal polled between looks at their channels.
fn next_input(shared: &Shared<'_>, root: &Path, landed: &mut Option<Shown>) -> io::Result<Input> {
    assert!(
        !shared.test_panic,
        "DUSCOPE_TEST_PANIC: a panic after the first frame"
    );
    if !shared.running.get() && shared.planning.get() == 0 {
        return duscope_tui::next_event().map(Input::Event);
    }
    loop {
        if shared.planning.get() > 0
            && let Ok(planned) = shared.plan_rx.try_recv()
        {
            shared.planning.set(shared.planning.get() - 1);
            return Ok(Input::Planned(planned));
        }
        if shared.running.get() {
            match shared.rx.try_recv() {
                Ok(result) => {
                    shared.running.set(false);
                    return Ok(match result.and_then(|landing| shown_of(landing, root)) {
                        Ok(shown) => {
                            *landed = Some(shown);
                            Input::Landed
                        }
                        Err(failure) => Input::RescanFailed(failure.message),
                    });
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    shared.running.set(false);
                    return Ok(Input::RescanFailed(
                        "the rescan stopped without an answer".to_owned(),
                    ));
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        // A plan is often done at once: looked for sooner than a rescan,
        // so the pane does not sit on "measuring" with its answer waiting.
        let wait = if shared.planning.get() > 0 { 10 } else { 100 };
        if duscope_tui::event_ready(Duration::from_millis(wait))? {
            return duscope_tui::next_event().map(Input::Event);
        }
    }
}

/// `D`: the plan of the marks beside the viewer, on a thread of its own, so
/// the viewer goes on answering its keys while the marked folders are
/// walked. Its answer is read by [`next_input`].
fn start_plan(job: PlanJob, shared: &Shared<'_>) -> Result<(), String> {
    let tx = shared.plan_tx.clone();
    let gate = test_plan_gate();
    thread::Builder::new()
        .name("plan".to_owned())
        .spawn(move || {
            if let Some(gate) = gate {
                while !gate.exists() && !job.stopped() {
                    thread::sleep(Duration::from_millis(10));
                }
            }
            // The viewer may have closed; nothing is waiting then.
            let _: Result<(), mpsc::SendError<Planned>> =
                tx.send(job.run(&|name| std::env::var_os(name)));
        })
        .map_err(|e| format!("cannot start it: {e}"))?;
    shared.planning.set(shared.planning.get() + 1);
    Ok(())
}

/// test-only: `DUSCOPE_TEST_PLAN_GATE=<path>` holds the mark pane's plan
/// until the file at `<path>` exists, in debug builds alone, for the tests
/// of a viewer that keeps answering while a plan runs.
fn test_plan_gate() -> Option<PathBuf> {
    if !cfg!(debug_assertions) {
        return None;
    }
    std::env::var_os("DUSCOPE_TEST_PLAN_GATE").map(PathBuf::from)
}

/// `r`: the rescan beside the viewer, on a thread of its own, its logging
/// off while it runs: the terminal is the viewer's.
fn start_rescan(shown: &Shown, shared: &Shared<'_>) -> Result<(), String> {
    let job = match &shown.file {
        Some(file) => Job::Saved {
            file: file.clone(),
            root: shown.source.view().root_path().to_owned(),
        },
        None => Job::Walked(Box::new(shared.walk.clone()), shared.excludes.to_vec()),
    };
    let dir = shared
        .index_dir
        .map(Path::to_owned)
        .or_else(|| duscope_core::index::default_dir(|name| std::env::var_os(name)));
    let tx = shared.tx.clone();
    thread::Builder::new()
        .name("rescan".to_owned())
        .spawn(move || {
            let level = log::max_level();
            log::set_max_level(log::LevelFilter::Off);
            let result = job.run(dir.as_deref());
            log::set_max_level(level);
            // The viewer may have closed; nothing is waiting then.
            let _ = tx.send(result);
        })
        .map_err(|e| format!("cannot start it: {e}"))?;
    shared.running.set(true);
    Ok(())
}

/// What `r` runs: a rescan of the saved scan in `file` for a viewer rooted
/// at `root`, or the walk again for a scan not saved.
enum Job {
    Saved { file: PathBuf, root: PathBuf },
    Walked(Box<WalkArgs>, Vec<String>),
}

impl Job {
    fn run(self, dir: Option<&Path>) -> Result<Landing, Failure> {
        match self {
            Job::Saved { file, root } => {
                let dir = dir.ok_or_else(|| {
                    Failure::new(
                        Exit::NotFound,
                        "scan_not_found",
                        "no index directory to save the rescan in: neither HOME nor \
                         XDG_CACHE_HOME is set",
                    )
                })?;
                rescan::in_viewer(&file, dir, &root).map(Landing::Saved)
            }
            Job::Walked(walk, excludes) => {
                scan::scan(&walk, &excludes).map(|scanned| Landing::Walked(Box::new(scanned)))
            }
        }
    }
}

/// What a landing shows: a saved scan opened again at the entry the viewer
/// is rooted on (`root`, or its own root if that has gone), or the walk.
fn shown_of(landing: Landing, root: &Path) -> Result<Shown, Failure> {
    match landing {
        Landing::Saved(file) => {
            let index = Index::open(&file).map_err(core_failure)?;
            let base = index
                .below_root(root)
                .and_then(|rel| index.lookup(rel))
                .unwrap_or(0);
            let unreadable = index.unreadable_dirs(base, index.node(base).last_id);
            let source = Source::Saved {
                index: Box::new(index),
                base,
                unreadable,
                stale: None,
            };
            source.check()?;
            Ok(Shown {
                source,
                file: Some(file),
            })
        }
        Landing::Walked(scanned) => Ok(Shown {
            source: Source::Fresh {
                scanned,
                save: None,
            },
            file: None,
        }),
    }
}

/// `clean`'s line once it has moved them, for what the session moved.
fn moved_line(session: Session, output: &Output) -> String {
    format!(
        "moved {} item{} to the Trash, {} (not yet freed)",
        thousands(session.moved_count),
        if session.moved_count == 1 { "" } else { "s" },
        output.units.size(session.moved_bytes)
    )
}

/// `clean`'s warning that the saved scans were not changed, and the rescan
/// that measures the tree again.
fn index_untouched(shown: &Shown) -> Warning {
    let root = shown
        .source
        .view()
        .root_path()
        .to_string_lossy()
        .into_owned();
    let command = match (&shown.file, shown.source.scan_id()) {
        (Some(file), Some(id)) if rescannable(shown) => {
            next::command("rescan", None, &[], &id, file)
        }
        _ => format!("duscope scan --save {}", shell_quote(&root)),
    };
    Warning {
        code: "index_untouched",
        message: "the saved scans still hold what moved to the Trash: they are never changed"
            .to_owned(),
        count: None,
        hint: Some(format!("Run {command} to measure the tree again")),
    }
}

/// Whether the viewer's saved scan can be rescanned: a scan, not an import.
fn rescannable(shown: &Shown) -> bool {
    match &shown.source {
        Source::Saved { index, .. } => scanned(index),
        Source::Fresh { .. } => true,
    }
}

/// test-only: with `DUSCOPE_TEST_OPENER` set (debug builds alone), the
/// file that `o` appends the command lines it would start to, in place of
/// starting them: one line each, its arguments separated by spaces. No
/// test starts a real one.
fn test_opener() -> Option<duscope_tui::Opener<'static>> {
    if !cfg!(debug_assertions) {
        return None;
    }
    let log = PathBuf::from(std::env::var_os("DUSCOPE_TEST_OPENER")?);
    Some(Box::new(move |path: &Path| {
        use std::os::unix::ffi::OsStrExt;
        let mut lines = Vec::new();
        for line in duscope_walk::reveal_commands(path) {
            let args: Vec<&[u8]> = line.iter().map(|arg| arg.as_bytes()).collect();
            lines.extend_from_slice(&args.join(&b' '));
            lines.push(b'\n');
        }
        std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&log)?
            .write_all(&lines)
    }))
}

/// The terminal failed: a read, a write or the size.
fn terminal_failure(e: &io::Error) -> Failure {
    Failure::new(
        Exit::Runtime,
        "io_error",
        format!("the terminal failed: {e}"),
    )
}
