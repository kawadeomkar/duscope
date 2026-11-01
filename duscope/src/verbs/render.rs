//! `render [path]`: the treemap as a picture, `--format svg` or `png` (or
//! the `-o` file's extension), at `--width` × `--height` pixels (1200 × 800 by
//! default), laid out by `--layout squarify` (the default) or `gp`, in
//! `--color-by`'s colours and `--theme`'s pack, opening folders down to
//! `--depth`. The tree is the newest saved scan that holds the path, or one
//! run for the verb and saved once the picture is out, as for the list
//! verbs. The picture goes to stdout or the `-o` file, written whole; a
//! line for people to stderr.

use std::io::IsTerminal;
use std::path::Path;

use duscope_treemap::Algorithm;
use duscope_treemap::paint::{ColorBy, Glyphs, PaintOpts};
use duscope_treemap::picture::PictureOpts;
use duscope_treemap::png::{MAX_PIXELS, write_png};
use duscope_treemap::svg::write_svg;

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
    let (png, width, height) = checked(args, format, json_or_plain, output)?;
    let (pack, ls_colors) = treemap::picture(args.theme.as_ref(), args.configured_theme.as_ref())?;
    let classes = super::query::classes(args.walk.classes_extra.as_deref(), "[classes] extra")?;
    let source = source::source(&args.walk, &args.index, index_dir)?;
    let warnings = source.warnings(&args.walk);
    let size = scan::size_kind(&args.walk);
    let block = source.scan_block(&args.walk, size);
    let summary = source.footer(&block, output);
    let view = source.view();
    let opts = PictureOpts {
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
        tiles = if png {
            write_png(&*view, &opts, out)?
        } else {
            write_svg(&*view, &opts, out)?
        };
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
            "rendered {width}×{height} {}, {} tiles · {summary}",
            if png { "png" } else { "svg" },
            thousands(tiles)
        ));
    }
    source.finish(&args.walk, output)
}

/// Whether the picture is a PNG, and its width and height: the format
/// asked for (`--format`, else the `-o` file's extension, else SVG), and
/// what refuses it before any scan: another format, `--json` or `--plain`,
/// a PNG to a terminal or over [`MAX_PIXELS`].
fn checked(
    args: &RenderArgs,
    format: Option<FormatArg>,
    json_or_plain: bool,
    output: &Output,
) -> Result<(bool, u16, u16), Failure> {
    let extension = output
        .file
        .as_deref()
        .and_then(Path::extension)
        .map(|e| e.to_string_lossy().to_ascii_lowercase());
    let png = match (format, extension.as_deref()) {
        (Some(FormatArg::Svg), _) | (None, None | Some("svg")) => false,
        (Some(FormatArg::Png), _) | (None, Some("png")) => true,
        _ => {
            return Err(
                Failure::new(Exit::Usage, "usage", "render writes --format svg or png")
                    .hint("Rerun duscope render with --format svg or png, or -o FILE.svg"),
            );
        }
    };
    let (width, height) = (
        args.width.unwrap_or(DEFAULT_SIZE.0),
        args.height.unwrap_or(DEFAULT_SIZE.1),
    );
    if png && output.file.is_none() && std::io::stdout().is_terminal() {
        return Err(Failure::new(
            Exit::Usage,
            "usage",
            "a PNG is binary, which a terminal cannot show",
        )
        .hint("Rerun duscope render --format png with -o FILE.png"));
    }
    if png && u64::from(width) * u64::from(height) > MAX_PIXELS {
        return Err(Failure::new(
            Exit::Usage,
            "usage",
            format!("a PNG of {width} × {height} is more than its {MAX_PIXELS} pixels"),
        )
        .hint("Rerun duscope render with a smaller --width and --height, or --format svg"));
    }
    // The picture is the output: no envelope or records beside it.
    if json_or_plain {
        return Err(Failure::new(
            Exit::Usage,
            "usage",
            "render writes a picture, which --json and --plain do not describe",
        )
        .hint("Rerun duscope render without --json or --plain"));
    }
    Ok((png, width, height))
}
