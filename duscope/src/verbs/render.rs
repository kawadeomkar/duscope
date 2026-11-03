//! `render [path]`: the treemap as a picture, `--format svg` (or the `-o`
//! file's extension), at `--width` × `--height` pixels (1200 × 800 by
//! default), laid out by `--layout squarify` (the default) or `gp`, in
//! `--color-by`'s colours and `--theme`'s pack, opening folders down to
//! `--depth`. The tree is the newest saved scan that holds the path, or one
//! run for the verb and saved once the picture is out, as for the list
//! verbs. The picture goes to stdout or the `-o` file, written whole; a
//! line for people to stderr.

use std::path::Path;

use duscope_treemap::Algorithm;
use duscope_treemap::paint::{ColorBy, Glyphs, PaintOpts};
use duscope_treemap::svg::{SvgOpts, write_svg};

use super::export::whole;
use super::source;
use super::{Exit, Failure, scan, treemap};
use crate::cli::{ColorBy as ColorByArg, Layout, RenderArgs};
use crate::out::{self, FormatArg, Output, stderr_line, thousands};

/// The picture's size without `--width` and `--height`.
const DEFAULT_SIZE: (u16, u16) = (1200, 800);

/// `render`; `json_or_plain`: `--json` or `--plain` was given, which a picture does
/// not answer.
pub fn run(
    args: &RenderArgs,
    format: Option<FormatArg>,
    json_or_plain: bool,
    index_dir: Option<&Path>,
    output: &Output,
) -> Result<(), Failure> {
    let extension = output
        .file
        .as_deref()
        .and_then(Path::extension)
        .map(|e| e.to_string_lossy().to_ascii_lowercase());
    match (format, extension.as_deref()) {
        (Some(FormatArg::Svg), _) | (None, None | Some("svg")) => {}
        (None, Some("png")) => {
            return Err(Failure::new(
                Exit::Usage,
                "not_available",
                "render writes SVG for now: PNG arrives with P2-MAP-02",
            )
            .hint("Rerun duscope render with -o FILE.svg"));
        }
        _ => {
            return Err(
                Failure::new(Exit::Usage, "usage", "render writes --format svg")
                    .hint("Rerun duscope render with --format svg, or -o FILE.svg"),
            );
        }
    }
    // The picture is the output: no envelope or records beside it.
    if json_or_plain {
        return Err(Failure::new(
            Exit::Usage,
            "usage",
            "render writes an SVG picture, which --json and --plain do not describe",
        )
        .hint("Rerun duscope render without --json or --plain"));
    }
    let (pack, ls_colors) = treemap::picture(args.theme.as_ref(), args.configured_theme.as_ref())?;
    let classes = super::query::classes(args.walk.classes_extra.as_deref(), "[classes] extra")?;
    let source = source::source(&args.walk, &args.index, index_dir)?;
    let warnings = source.warnings(&args.walk);
    let size = scan::size_kind(&args.walk);
    let block = source.scan_block(&args.walk, size);
    let summary = source.footer(&block, output);
    let view = source.view();
    let (width, height) = (
        args.width.unwrap_or(DEFAULT_SIZE.0),
        args.height.unwrap_or(DEFAULT_SIZE.1),
    );
    let opts = SvgOpts {
        paint: PaintOpts {
            pack: &pack,
            color_by: match args.color_by {
                None | Some(ColorByArg::Type) => ColorBy::Type,
                Some(ColorByArg::Folder) => ColorBy::Folder,
                Some(ColorByArg::Level) => ColorBy::Level,
                Some(ColorByArg::Age) => ColorBy::Age,
                Some(ColorByArg::Lscolors) => ColorBy::LsColors(ls_colors),
            },
            glyphs: Glyphs::Unicode,
            depth: args.depth,
            min_tile: 1,
            pool_files: args.pool_files,
            size,
            now: scan::now(&args.walk)?,
            classes: &classes,
        },
        algorithm: match args.layout {
            None | Some(Layout::Squarify) => Algorithm::Squarify,
            Some(Layout::Gp) => Algorithm::Gp,
        },
        width,
        height,
        title: view.root_path().display().to_string(),
        legend: treemap::legend(&*view, &classes, size),
        summary: summary.clone(),
    };
    let mut tiles = 0;
    // An index that failed partway is the render's failure, found before
    // the picture replaces an -o file.
    let mut failed = None;
    let written = whole(output, |out| {
        tiles = write_svg(&*view, &opts, out)?;
        if let Err(failure) = source.check() {
            failed = Some(failure);
            return Err(std::io::Error::other("the index could not be read"));
        }
        Ok(())
    });
    if let Some(failure) = failed {
        return Err(failure);
    }
    written?;
    drop(view);
    if !output.quiet {
        out::warn(&warnings);
        stderr_line(&format!(
            "rendered {width}×{height} svg, {} tiles · {summary}",
            thousands(tiles)
        ));
    }
    source.finish(&args.walk, output)
}
