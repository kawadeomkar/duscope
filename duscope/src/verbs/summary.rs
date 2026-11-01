//! `summary`: a scan's totals under a path (the scan's root by default),
//! as the scan counts them and as `du` does, each volume's share, what a
//! mount point's volume uses that the scan did not find, and how the scan
//! ran. The CLI twin of the `duscope_summary` tool.

use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use duscope_core::envelope::{
    ObjectEnvelope, ObjectQuery, SCHEMA_VERSION, Summary, SummaryVolume, Tool, VolumeEntry,
};
use duscope_core::query::{self, SizeKind};

use super::Failure;
use super::next::Hints;
use super::scan;
use super::source;
use crate::cli::SummaryArgs;
use crate::out::{Output, thousands};

/// `summary`.
pub fn run(args: &SummaryArgs, index_dir: Option<&Path>, output: &Output) -> Result<(), Failure> {
    let source = source::source(&args.walk, &args.index, index_dir)?;
    let size = scan::size_kind(&args.walk);
    let block = source.scan_block(&args.walk, size);
    // A mount point the scan did not enter starts nothing.
    let unentered: Vec<u64> = block
        .volumes
        .iter()
        .filter(|v| !v.entered)
        .map(|v| v.dev)
        .collect();
    let view = source.view();
    let bytes = query::summary(&*view, &unentered);
    let root = view.node(0);
    let root_path = view.root_path().to_owned();
    drop(view);
    let path_bytes = root_path.as_os_str().as_bytes().to_vec();
    let path = root_path.to_string_lossy().into_owned();
    // Each volume's share, by its device; a device met twice (a bind
    // mount) has its bytes at the first.
    let mut shared: Vec<u64> = Vec::new();
    let mut found = |dev: u64| {
        let share = bytes
            .volumes
            .iter()
            .find(|b| b.dev == dev && !shared.contains(&dev))
            .copied()
            .unwrap_or_default();
        shared.push(dev);
        share
    };
    let listed: Vec<(&VolumeEntry, query::VolumeBytes)> = block
        .volumes
        .iter()
        .filter(|v| {
            source.at_root()
                || bytes.volumes.iter().any(|b| b.dev == v.dev)
                || Path::new(&v.mount_point).starts_with(&root_path)
        })
        .map(|v| (v, found(v.dev)))
        .collect();
    let uses: Vec<query::VolumeUse<'_>> = listed
        .iter()
        .map(|(v, share)| query::VolumeUse {
            mount_point: Path::new(&v.mount_point),
            fs_type: &v.fs_type,
            total: (v.total_bytes > 0).then_some(v.total_bytes),
            free: (v.total_bytes > 0).then_some(v.free_bytes),
            entered: v.entered,
            options: &v.options,
            found: share.allocated,
        })
        .collect();
    let unaccounted = query::unaccounted(&root_path, &uses);
    let volumes: Vec<SummaryVolume> = listed.iter().map(|(v, share)| volume(v, *share)).collect();
    let result = Summary {
        path,
        path_bytes,
        volumes,
        allocated_bytes: bytes.allocated,
        apparent_bytes: bytes.apparent,
        du_compatible_bytes: bytes.du_compatible,
        unaccounted_bytes: unaccounted,
        dataless_bytes: bytes.dataless,
        file_count: root.file_count,
        dir_count: root.dir_count,
        error_count: if source.at_root() {
            block.error_count
        } else {
            source.errors_under()
        },
        vanished_count: block.vanished_count,
        attrs: block.attrs,
        incremental: block.incremental,
        parent_scan_id: source.parent_scan_id(),
        started_at: block.started_at.clone(),
        finished_at: block.finished_at.clone(),
        duration_ms: u64::try_from(
            source.finished_ns().saturating_sub(source.started_ns()) / 1_000_000,
        )
        .unwrap_or(0),
        thread_count: block.thread_count,
    };
    let warnings = source.warnings(&args.walk);
    let next = next(block.id.as_deref(), &result.path);
    let envelope = ObjectEnvelope {
        ok: true,
        schema_version: SCHEMA_VERSION,
        tool: Tool::CURRENT,
        query: ObjectQuery {
            command: "summary",
            path: result.path.clone(),
        },
        scan: Some(block),
        result,
        warnings,
        next,
    };
    source.check()?;
    let lines = people(&envelope.result, output, size);
    output.object(&envelope, &plain(&envelope.result), &lines)?;
    source.finish(&args.walk, output)
}

/// `summary`'s hints: the largest entries, the classes and the caches
/// under the path.
fn next(scan: Option<&str>, path: &str) -> Vec<String> {
    let Some(mut hints) = Hints::new(scan) else {
        return Vec::new();
    };
    hints
        .run("top", Some(path), &[])
        .run("by-type", Some(path), &[])
        .run("caches", Some(path), &[])
        .done()
}

/// A scan block's volume, with the bytes found on it.
fn volume(v: &VolumeEntry, share: query::VolumeBytes) -> SummaryVolume {
    SummaryVolume {
        mount_point: v.mount_point.clone(),
        fs_type: v.fs_type.clone(),
        total_bytes: (v.total_bytes > 0).then_some(v.total_bytes),
        free_bytes: (v.total_bytes > 0).then_some(v.free_bytes),
        allocated_bytes: share.allocated,
        apparent_bytes: share.apparent,
        du_compatible_bytes: share.du_compatible,
        entered: v.entered,
        reason: v.reason,
    }
}

/// The answer for people: one line each, name and value.
fn people(s: &Summary, output: &Output, size: SizeKind) -> Vec<(String, String)> {
    let bytes = |n: u64| output.units.size(n);
    let shown = |allocated: u64, apparent: u64| match size {
        SizeKind::Allocated => bytes(allocated),
        SizeKind::Apparent => bytes(apparent),
    };
    let mut lines = vec![
        ("path".to_owned(), s.path.clone()),
        (
            "size".to_owned(),
            format!(
                "{} allocated · {} apparent · {} as du counts it",
                bytes(s.allocated_bytes),
                bytes(s.apparent_bytes),
                bytes(s.du_compatible_bytes)
            ),
        ),
        (
            "contents".to_owned(),
            format!(
                "{} files · {} dirs",
                thousands(s.file_count),
                thousands(s.dir_count)
            ),
        ),
    ];
    if let Some(n) = s.unaccounted_bytes {
        lines.push(("not found".to_owned(), bytes(n)));
    }
    if s.dataless_bytes > 0 {
        lines.push(("dataless".to_owned(), bytes(s.dataless_bytes)));
    }
    for v in &s.volumes {
        let state = if v.entered {
            shown(v.allocated_bytes, v.apparent_bytes)
        } else {
            format!("not entered ({})", v.reason.unwrap_or("excluded"))
        };
        let room = match (v.total_bytes, v.free_bytes) {
            (Some(total), Some(free)) => format!(" · {} free of {}", bytes(free), bytes(total)),
            _ => String::new(),
        };
        lines.push((
            "volume".to_owned(),
            format!("{} ({}) · {state}{room}", v.mount_point, v.fs_type),
        ));
    }
    if s.error_count > 0 {
        lines.push(("unreadable".to_owned(), thousands(s.error_count)));
    }
    lines.push((
        "scan".to_owned(),
        format!(
            "{} · {} ms · {} threads · attrs {}",
            s.started_at, s.duration_ms, s.thread_count, s.attrs
        ),
    ));
    lines
}

/// The answer for `--plain`: name and value, raw bytes for the path.
fn plain(s: &Summary) -> Vec<(&'static str, Vec<u8>)> {
    let n = |v: u64| v.to_string().into_bytes();
    vec![
        ("path", s.path_bytes.clone()),
        ("allocated_bytes", n(s.allocated_bytes)),
        ("apparent_bytes", n(s.apparent_bytes)),
        ("du_compatible_bytes", n(s.du_compatible_bytes)),
        (
            "unaccounted_bytes",
            s.unaccounted_bytes.map(n).unwrap_or_default(),
        ),
        ("dataless_bytes", n(s.dataless_bytes)),
        ("file_count", n(s.file_count)),
        ("dir_count", n(s.dir_count)),
        ("error_count", n(s.error_count)),
        ("vanished_count", n(s.vanished_count)),
        ("duration_ms", n(s.duration_ms)),
    ]
}
